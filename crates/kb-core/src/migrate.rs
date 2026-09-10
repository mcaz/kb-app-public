//! 旧添付(`<ノート ID>.files/`)を Artifact の台帳へ載せる — ADR-0003 決定4・実装順8。
//!
//! 初段の台帳登録では実体を動かさない。旧ファイルは保管庫の中に残り、locator は
//! `LegacyGit` を指す。増えるのは台帳・参照・旧リンクの対応表だけなので、
//! 受入条件「**移行前に取得できた添付を、移行によって取得不能にしない**」は
//! 実体に触れないことで満たす — 移行しても Git の中の同じファイルを読み続ける。
//!
//! ## LFS への昇格と旧コピーの保持
//!
//! 二段目は固定planへ入力を再照合し、LFS upload成功後だけlocatorをManagedへ変える。
//! 旧ファイルと旧リンクは残し、直後versionからのrollbackでも両方のコピーを削除しない。
//! 型付き昇格証拠は旧pathの由来を固定するが、現在も保持コピーであるという判定は、
//! 監査側が現在のmanifest・ref/alias・実ファイルを同じsnapshotへ照合して行う。
//!
//! ## 区分を `private + full` にしてよい理由
//!
//! 正本は「`sensitivity` を `shared` と推定しない」「元が client repo だったかも
//! 推定不能」と言う。それでも `full`(本体も同期)を選べるのは、**旧添付は既に
//! 保管庫の Git に入っていて、既に全端末へ運ばれている**ため。ここで `full` と
//! 書いても持ち出し範囲は1ミリも広がらない — 現状を台帳の語で書き写しているだけ。

use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::artifact::{
    ArtifactId, ArtifactRef, ContentHash, Created, Hasher, Locator, Manifest, Policy, RefName, Role,
};
use crate::ledger::Ledger;
use crate::vault::Vault;

/// まだ台帳に載っていない旧添付。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub note_id: String,
    pub file_name: String,
    pub size: u64,
    /// 本文に書かれている形。対応表の鍵になる
    pub link: String,
}

/// 載せた結果。
#[derive(Debug, Clone)]
pub struct Migrated {
    pub note_id: String,
    pub file_name: String,
    pub id: ArtifactId,
    pub ref_name: RefName,
}

pub const PROMOTION_PLAN_SCHEMA: &str = "kb-app.legacy-artifact-promotion-plan/v1";
pub const PROMOTION_RESULT_SCHEMA: &str = "kb-app.legacy-artifact-promotion-result/v1";
pub(crate) const PROMOTION_PROOF_EVENT_KIND: &str = "legacy-promotion-proof";

/// 1 Artifact だけを対象にする小規模 promotion plan。
/// read-only の棚卸し時点に見えた入力をすべて固定し、apply 時に再照合する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromotionPlan {
    pub schema: String,
    pub plan_id: String,
    pub artifact_id: ArtifactId,
    pub manifest_version: u64,
    pub note_id: String,
    pub file_name: String,
    pub legacy_path: String,
    pub hash: ContentHash,
    pub size: u64,
    pub destination: String,
    pub reference: Option<PromotionRefSnapshot>,
    pub aliases: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromotionRefSnapshot {
    pub name: RefName,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionResult {
    pub schema: String,
    pub result_id: String,
    pub plan_id: String,
    pub artifact_id: ArtifactId,
    pub before_version: u64,
    pub after_version: u64,
    pub note_id: String,
    pub file_name: String,
    pub hash: ContentHash,
    pub old_path_retained: bool,
    pub already_applied: bool,
}

/// LegacyGit 台帳を1件ずつ deterministic plan にする。**書き込みも network I/O もない。**
pub fn plan_promotions(vault: &Vault, ledger: &Ledger) -> Result<Vec<PromotionPlan>> {
    let mut out = Vec::new();
    for manifest in ledger.list() {
        let Locator::LegacyGit { note_id, file_name } = &manifest.locator else {
            continue;
        };
        let path = vault.legacy_attachment_path(note_id, file_name)?;
        let (hash, size) = hash_file(&path)?;
        if hash != manifest.hash || size != manifest.created.size {
            bail!("旧実体が台帳と一致しない: {}", manifest.id);
        }
        let reference = ledger.ref_for(&manifest.id).map(|r| PromotionRefSnapshot {
            name: r.name,
            revision: r.revision,
        });
        let mut aliases: Vec<(String, String)> = reference
            .as_ref()
            .map(|r| {
                ledger
                    .aliases()
                    .into_iter()
                    .filter(|(_, name)| name == &r.name.to_string())
                    .collect()
            })
            .unwrap_or_default();
        aliases.sort();
        let mut plan = PromotionPlan {
            schema: PROMOTION_PLAN_SCHEMA.to_string(),
            plan_id: String::new(),
            artifact_id: manifest.id,
            manifest_version: manifest.version,
            note_id: note_id.clone(),
            file_name: file_name.clone(),
            legacy_path: format!("/{note_id}.files/{file_name}"),
            hash,
            size,
            destination: format!(".kb-artifacts/lfs/{}", manifest.hash),
            reference,
            aliases,
        };
        plan.plan_id = promotion_plan_id(&plan)?;
        out.push(plan);
    }
    out.sort_by(|a, b| a.artifact_id.cmp(&b.artifact_id));
    Ok(out)
}

fn promotion_plan_id(plan: &PromotionPlan) -> Result<String> {
    let mut fixed = plan.clone();
    fixed.plan_id.clear();
    let bytes = serde_json::to_vec(&fixed)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

/// 旧proseを証拠へ読み替えず、新種別だけを検証する。pathはここで構築・照合し、
/// 返されたplanも現在のmanifest/ref/alias/実体とのsnapshot照合なしでは保持コピーと扱わない。
pub(crate) fn parse_promotion_proof(
    event: &crate::artifact::Event,
) -> Result<Option<PromotionPlan>> {
    if event.kind != PROMOTION_PROOF_EVENT_KIND {
        return Ok(None);
    }
    let plan: PromotionPlan =
        serde_json::from_str(&event.detail).context("昇格証拠の構造が不正")?;
    validate_promotion_plan(&plan)?;
    Ok(Some(plan))
}

fn validate_promotion_plan(plan: &PromotionPlan) -> Result<()> {
    ensure!(
        plan.schema == PROMOTION_PLAN_SCHEMA && promotion_plan_id(plan)? == plan.plan_id,
        "promotion planのschemaまたはplan_idが不正"
    );
    // serdeのString newtypeはFromStrを通らないので、保存済み証拠も型の境界で再検証する。
    ArtifactId::from_str(plan.artifact_id.as_str())?;
    ContentHash::from_str(plan.hash.as_str())?;
    ensure!(
        plan.manifest_version > 0 && plan.manifest_version.checked_add(1).is_some(),
        "promotion planのversionが不正"
    );
    let note = crate::note_id::NoteId::parse(&plan.note_id)?;
    note.legacy_attachment_relative_path(&plan.file_name)?;
    ensure!(
        plan.legacy_path == format!("/{}.files/{}", note.as_str(), plan.file_name),
        "promotion planの旧pathが正規形ではない"
    );
    ensure!(
        plan.destination == format!(".kb-artifacts/lfs/{}", plan.hash),
        "promotion planの昇格先がhash由来ではない"
    );
    if let Some(reference) = &plan.reference {
        RefName::from_str(reference.name.as_str())?;
        ensure!(reference.revision > 0, "promotion planのref revisionが不正");
        // aliasのsourceは比較値だけ。任意文字列をファイルpathとして解決しない。
        ensure!(
            plan.aliases
                .iter()
                .all(|(_, name)| name == reference.name.as_str()),
            "promotion planのaliasがrefと一致しない"
        );
    } else {
        ensure!(
            plan.aliases.is_empty(),
            "promotion planのrefなしaliasは不正"
        );
    }
    ensure!(
        plan.aliases.windows(2).all(|pair| pair[0].0 < pair[1].0),
        "promotion planのaliasが一意な正規順序ではない"
    );
    Ok(())
}

trait PromotionTransport {
    fn stage_and_upload(
        &mut self,
        vault: &Vault,
        source: &Path,
        expected_hash: &ContentHash,
        expected_size: u64,
    ) -> Result<()>;
}

struct LfsPromotionTransport;

impl PromotionTransport for LfsPromotionTransport {
    fn stage_and_upload(
        &mut self,
        vault: &Vault,
        source: &Path,
        expected_hash: &ContentHash,
        expected_size: u64,
    ) -> Result<()> {
        let (hash, size) = crate::lfs::import(vault, source)?;
        if &hash != expected_hash || size != expected_size {
            bail!("LFS staging後のhash/sizeがplanと一致しない");
        }
        if crate::lfs::verify(vault, &hash)? != crate::store::Verified::Ok {
            bail!("LFS objectをlocalで照合できない: {hash}");
        }
        // この成功を確認するまでは manifest を一切変更しない。
        crate::lfs::push_object(vault, &hash)
    }
}

/// LFS upload 確認後にだけ LegacyGit を Managed へ切り替える。
pub fn apply_promotion(
    vault: &Vault,
    ledger: &Ledger,
    plan: &PromotionPlan,
    at: &str,
) -> Result<PromotionResult> {
    let mut transport = LfsPromotionTransport;
    apply_promotion_with(vault, ledger, plan, at, &mut transport, true)
}

fn apply_promotion_with(
    vault: &Vault,
    ledger: &Ledger,
    plan: &PromotionPlan,
    at: &str,
    transport: &mut impl PromotionTransport,
    push_git: bool,
) -> Result<PromotionResult> {
    validate_promotion_plan(plan)?;
    let _lock = crate::connect::sync_lock(vault)?;
    let current = ledger
        .get(&plan.artifact_id)?
        .with_context(|| format!("Artifactがない: {}", plan.artifact_id))?;

    // crash/retry: uploadとswitchが済んだ同じplanだけは、Git pushを再試行して成功扱いにする。
    if matches!(&current.locator, Locator::Managed { hash } if hash == &plan.hash)
        && current.version == plan.manifest_version + 1
        && current
            .events
            .iter()
            .any(|event| event.kind == "legacy-promoted" && event.detail.contains(&plan.plan_id))
    {
        validate_ref_alias_snapshot(ledger, plan)?;
        if crate::lfs::verify(vault, &plan.hash)? != crate::store::Verified::Ok {
            bail!("promotion済み台帳に対応するLFS objectを照合できない");
        }
        if push_git {
            crate::connect::push_now_locked(vault)?;
        }
        return Ok(promotion_result(plan, current.version, true));
    }

    validate_plan_inputs(vault, ledger, plan, &current)?;
    let source = vault.legacy_attachment_path(&plan.note_id, &plan.file_name)?;
    transport.stage_and_upload(vault, &source, &plan.hash, plan.size)?;

    // upload 中の並行変更も切替直前に再照合する。
    let before = ledger
        .get(&plan.artifact_id)?
        .context("upload後にArtifactが見つからない")?;
    validate_plan_inputs(vault, ledger, plan, &before)?;
    let mut after = before.clone();
    after.promote_legacy_locator(plan.manifest_version, &plan.note_id, &plan.file_name)?;
    after.record(
        at,
        "legacy-promoted",
        &format!("{}; LFS upload確認後にManagedへ昇格", plan.plan_id),
    );
    // 旧イベントはcrash/retry互換のため残す。証拠はuploadと入力再照合後の同じ保存に含める。
    after.record(
        at,
        PROMOTION_PROOF_EVENT_KIND,
        &serde_json::to_string(plan)?,
    );
    let outcome = ledger.put_with_outcome(vault, &after)?;
    if let Some(error) = outcome.sync_error {
        // commitできない変更はprimaryにしない。旧実体もLFS objectも残る。
        let _ = ledger.put(vault, &before);
        bail!("promotion commitに失敗しLegacyGitへ復元した: {error}");
    }
    if push_git {
        crate::connect::push_now_locked(vault)?;
    }
    Ok(promotion_result(plan, after.version, false))
}

/// apply 結果に固定された直後versionからだけ LegacyGit へ戻す。
/// LFS object/pointerも旧実体も削除しない。
pub fn rollback_promotion(
    vault: &Vault,
    ledger: &Ledger,
    result: &PromotionResult,
    at: &str,
) -> Result<PromotionResult> {
    rollback_promotion_with(vault, ledger, result, at, true)
}

fn rollback_promotion_with(
    vault: &Vault,
    ledger: &Ledger,
    result: &PromotionResult,
    at: &str,
    push_git: bool,
) -> Result<PromotionResult> {
    if result.schema != PROMOTION_RESULT_SCHEMA
        || promotion_result_id(result)? != result.result_id
        || !result.old_path_retained
    {
        bail!("promotion resultがrollback可能な形式ではない");
    }
    let _lock = crate::connect::sync_lock(vault)?;
    let mut current = ledger
        .get(&result.artifact_id)?
        .context("rollback対象のArtifactがない")?;
    if current.version != result.after_version || current.hash != result.hash {
        bail!("promotion後にArtifactが変更されているためrollbackできない");
    }
    let path = vault.legacy_attachment_path(&result.note_id, &result.file_name)?;
    let (hash, _) = hash_file(&path)?;
    if hash != result.hash {
        bail!("旧実体が変更されているためrollbackできない");
    }
    let before = current.clone();
    current.rollback_legacy_locator(result.after_version, &result.note_id, &result.file_name)?;
    current.record(
        at,
        "legacy-promotion-rolled-back",
        &format!("{} をLegacyGitへ復元", result.plan_id),
    );
    let outcome = ledger.put_with_outcome(vault, &current)?;
    if let Some(error) = outcome.sync_error {
        let _ = ledger.put(vault, &before);
        bail!("rollback commitに失敗しManagedへ復元した: {error}");
    }
    if push_git {
        crate::connect::push_now_locked(vault)?;
    }
    let mut rolled_back = PromotionResult {
        after_version: current.version,
        already_applied: false,
        ..result.clone()
    };
    rolled_back.result_id = promotion_result_id(&rolled_back)?;
    Ok(rolled_back)
}

fn promotion_result(
    plan: &PromotionPlan,
    after_version: u64,
    already_applied: bool,
) -> PromotionResult {
    let mut result = PromotionResult {
        schema: PROMOTION_RESULT_SCHEMA.to_string(),
        result_id: String::new(),
        plan_id: plan.plan_id.clone(),
        artifact_id: plan.artifact_id.clone(),
        before_version: plan.manifest_version,
        after_version,
        note_id: plan.note_id.clone(),
        file_name: plan.file_name.clone(),
        hash: plan.hash.clone(),
        old_path_retained: true,
        already_applied,
    };
    result.result_id = promotion_result_id(&result).expect("PromotionResultはserializeできる");
    result
}

fn promotion_result_id(result: &PromotionResult) -> Result<String> {
    let mut fixed = result.clone();
    fixed.result_id.clear();
    let bytes = serde_json::to_vec(&fixed)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn validate_plan_inputs(
    vault: &Vault,
    ledger: &Ledger,
    plan: &PromotionPlan,
    manifest: &Manifest,
) -> Result<()> {
    if manifest.version != plan.manifest_version
        || manifest.hash != plan.hash
        || manifest.created.size != plan.size
        || !matches!(
            &manifest.locator,
            Locator::LegacyGit { note_id, file_name }
                if note_id == &plan.note_id && file_name == &plan.file_name
        )
    {
        bail!("promotion planが現在のmanifestと一致しない。planを取り直す");
    }
    let path = vault.legacy_attachment_path(&plan.note_id, &plan.file_name)?;
    let (hash, size) = hash_file(&path)?;
    if hash != plan.hash || size != plan.size {
        bail!("旧実体がpromotion planと一致しない。planを取り直す");
    }
    validate_ref_alias_snapshot(ledger, plan)
}

fn validate_ref_alias_snapshot(ledger: &Ledger, plan: &PromotionPlan) -> Result<()> {
    let current_ref = ledger
        .ref_for(&plan.artifact_id)
        .map(|r| PromotionRefSnapshot {
            name: r.name,
            revision: r.revision,
        });
    if current_ref != plan.reference {
        bail!("Artifact refがpromotion planから変更されている");
    }
    let mut aliases: Vec<(String, String)> = plan
        .reference
        .as_ref()
        .map(|r| {
            ledger
                .aliases()
                .into_iter()
                .filter(|(_, name)| name == &r.name.to_string())
                .collect()
        })
        .unwrap_or_default();
    aliases.sort();
    if aliases != plan.aliases {
        bail!("legacy aliasがpromotion planから変更されている");
    }
    Ok(())
}

/// 棚卸し。**何も書かない。**
pub fn survey(vault: &Vault, ledger: &Ledger) -> Result<Vec<Pending>> {
    let done: HashSet<(String, String)> = ledger
        .list()
        .iter()
        .filter_map(|m| match &m.locator {
            Locator::LegacyGit { note_id, file_name } => Some((note_id.clone(), file_name.clone())),
            _ => None,
        })
        .collect();

    let mut out = Vec::new();
    for (note_id, _) in vault.list_note_files()? {
        for (file_name, size) in vault.list_attachments(&note_id)? {
            if done.contains(&(note_id.clone(), file_name.clone())) {
                continue;
            }
            out.push(Pending {
                link: format!("/{note_id}.files/{file_name}"),
                note_id: note_id.clone(),
                file_name,
                size,
            });
        }
    }
    out.sort_by(|a, b| (&a.note_id, &a.file_name).cmp(&(&b.note_id, &b.file_name)));
    Ok(out)
}

/// 台帳・参照・対応表を重ねる。**冪等** — 既に載っているものは飛ばす。
pub fn migrate(
    vault: &Vault,
    ledger: &Ledger,
    workspace_id: &str,
    at: &str,
) -> Result<Vec<Migrated>> {
    let mut out = Vec::new();
    for pending in survey(vault, ledger)? {
        let path = vault.legacy_attachment_path(&pending.note_id, &pending.file_name)?;
        let (hash, size) = hash_file(&path)?;

        let mut manifest = Manifest::new(
            ArtifactId::new(unix_ms(at)),
            hash.clone(),
            Created {
                media_type: crate::intake::guess_media_type(&path),
                size,
                at: at.to_string(),
                // 作成時の来歴は分からない。**分からないことを分かると書かない**
                origin: "migration".into(),
                by: crate::OWNER_ACTOR.into(),
            },
            pending.file_name.clone(),
            Locator::LegacyGit {
                note_id: pending.note_id.clone(),
                file_name: pending.file_name.clone(),
            },
            Policy::migrated_legacy(),
            Role::File,
        );
        manifest.notes.push(pending.note_id.clone());
        manifest.record(
            at,
            "migrated",
            &format!("{} を台帳に載せた(実体は動かしていない)", pending.link),
        );
        ledger.put(vault, &manifest)?;

        // 本文の `/…files/…` は書き換えない。対応表で辿れるようにする(決定5)
        let ref_name = free_ref_name(ledger, &pending.file_name, &hash);
        let r = ArtifactRef::new(workspace_id, ref_name.clone(), manifest.id.clone());
        ledger.put_ref(vault, manifest.policy.sync, &r)?;
        ledger.put_alias(vault, &pending.link, &ref_name)?;

        out.push(Migrated {
            note_id: pending.note_id,
            file_name: pending.file_name,
            id: manifest.id,
            ref_name,
        });
    }
    Ok(out)
}

/// 参照名は本文に書かれうるので、**読める英字**に寄せる。
/// 日本語だけの名前は英字が残らないので、照合値の頭を使う。
fn free_ref_name(ledger: &Ledger, file_name: &str, hash: &ContentHash) -> RefName {
    let stem = Path::new(file_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(file_name);
    let mut base = String::new();
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() {
            base.push(c.to_ascii_lowercase());
        } else if !base.ends_with('-') && !base.is_empty() {
            base.push('-');
        }
    }
    // 連番の余地を残して詰める(RefName の上限は 64)
    let trimmed: String = base.trim_matches('-').chars().take(48).collect();
    let base = match trimmed.trim_end_matches('-') {
        "" => format!("legacy-{}", &hash.as_str()[..8]),
        ok => ok.to_string(),
    };

    let mut name = base.clone();
    let mut n = 1;
    loop {
        match RefName::from_str(&name) {
            Ok(r) if !ledger.ref_taken(&r) => return r,
            _ => {
                n += 1;
                name = format!("{base}-{n}");
            }
        }
    }
}

fn hash_file(path: &Path) -> Result<(ContentHash, u64)> {
    let mut f = File::open(path).with_context(|| format!("読めない: {}", path.display()))?;
    let mut hasher = Hasher::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let size = hasher.len();
    Ok((hasher.finish(), size))
}

fn unix_ms(at: &str) -> u64 {
    time::OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339)
        .map(|t| (t.unix_timestamp_nanos() / 1_000_000).max(0) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::SyncPolicy;
    use crate::resolve::{self, Link};
    use crate::store::{Availability, Stores, availability};
    use std::fs;
    use tempfile::{TempDir, tempdir};

    struct Env {
        _dir: TempDir,
        vault: Vault,
        ledger: Ledger,
        stores: Stores,
    }

    fn env() -> Env {
        let dir = tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let ledger = Ledger::at(vault.root.clone(), dir.path().join("sidecar"));
        let stores = Stores::at(dir.path().join("artifacts"), "ws-a");
        Env {
            _dir: dir,
            vault,
            ledger,
            stores,
        }
    }

    /// 旧添付を1つ置く(移行前の状態を作る)。
    fn legacy(vault: &Vault, title: &str, file_name: &str, bytes: &[u8]) -> String {
        let id = vault
            .propose_for_test(title, "本文。", None, &["test".into()], "test/client")
            .unwrap();
        let dir = vault.attach_dir(&id).unwrap();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(file_name), bytes).unwrap();
        id
    }

    const AT: &str = "2026-08-13T09:00:00Z";

    #[derive(Default)]
    struct MockUpload {
        fail: bool,
        calls: usize,
    }

    impl PromotionTransport for MockUpload {
        fn stage_and_upload(
            &mut self,
            vault: &Vault,
            source: &Path,
            expected_hash: &ContentHash,
            expected_size: u64,
        ) -> Result<()> {
            self.calls += 1;
            if self.fail {
                bail!("simulated upload failure");
            }
            crate::connect::ensure_vault_config(vault)?;
            let repo = git2::Repository::open(&vault.root)?;
            let storage = repo.config()?.get_string("lfs.storage")?;
            let oid = expected_hash.as_str();
            let dest = Path::new(&storage)
                .join("objects")
                .join(&oid[0..2])
                .join(&oid[2..4])
                .join(oid);
            fs::create_dir_all(dest.parent().unwrap())?;
            fs::copy(source, &dest)?;
            let (hash, size) = hash_file(&dest)?;
            assert_eq!(&hash, expected_hash);
            assert_eq!(size, expected_size);
            Ok(())
        }
    }

    fn migrated_legacy(e: &Env, bytes: &[u8]) -> (String, Migrated) {
        let note_id = legacy(&e.vault, "設計メモ", "図.png", bytes);
        let migrated = migrate(&e.vault, &e.ledger, "ws-a", AT).unwrap().remove(0);
        (note_id, migrated)
    }

    #[test]
    fn survey_reads_without_writing() {
        let e = env();
        let id = legacy(&e.vault, "設計メモ", "図.png", b"png bytes");

        let found = survey(&e.vault, &e.ledger).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].note_id, id);
        assert_eq!(found[0].file_name, "図.png");
        assert_eq!(found[0].size, 9);
        assert_eq!(found[0].link, format!("/{id}.files/図.png"));
        // 棚卸しは何も書かない
        assert!(e.ledger.list().is_empty());
    }

    /// 受入条件: **移行前に取得できた添付は、移行で取得不能にならない。**
    /// 実体を動かさないので、移行後も同じファイルを読み続ける。
    #[test]
    fn migrating_does_not_move_the_bytes() {
        let e = env();
        let id = legacy(&e.vault, "設計メモ", "図.png", b"png bytes");
        let path = e.vault.legacy_attachment_path(&id, "図.png").unwrap();

        let done = migrate(&e.vault, &e.ledger, "ws-a", AT).unwrap();
        assert_eq!(done.len(), 1);

        // 旧ファイルはそのまま
        assert!(path.is_file());
        assert_eq!(fs::read(&path).unwrap(), b"png bytes");

        // 台帳からは「手元にある」と見え、中身も読める
        let m = e.ledger.get(&done[0].id).unwrap().unwrap();
        assert_eq!(
            availability(&e.vault, &e.stores, &m),
            Availability::Local,
            "移行した瞬間に missing になってはいけない"
        );
        let resolved = resolve::resolve(&e.vault, &e.stores, &e.ledger, &Link::Fixed(m.id.clone()))
            .unwrap()
            .unwrap();
        let mut opened = resolved.open(&e.vault, &e.stores).unwrap().unwrap();
        let mut got = Vec::new();
        opened.read_to_end(&mut got).unwrap();
        assert_eq!(got, b"png bytes");
    }

    /// 本文の `/…files/…` は書き換えず、対応表で辿れるようにする(決定5)。
    #[test]
    fn the_old_body_link_resolves_through_the_alias() {
        let e = env();
        let id = legacy(&e.vault, "設計メモ", "図.png", b"png bytes");
        migrate(&e.vault, &e.ledger, "ws-a", AT).unwrap();

        let link = parse_and_resolve(&e, &format!("/{id}.files/図.png"));
        assert_eq!(link.manifest.display_name, "図.png");
        assert_eq!(link.availability, Availability::Local);
    }

    fn parse_and_resolve(e: &Env, raw: &str) -> resolve::Resolved {
        let link = resolve::parse_link(raw).expect("旧リンクとして読める");
        resolve::resolve(&e.vault, &e.stores, &e.ledger, &link)
            .unwrap()
            .expect("解決できる")
    }

    #[test]
    fn migrating_twice_changes_nothing() {
        let e = env();
        legacy(&e.vault, "設計メモ", "図.png", b"png bytes");

        let first = migrate(&e.vault, &e.ledger, "ws-a", AT).unwrap();
        let second = migrate(&e.vault, &e.ledger, "ws-a", AT).unwrap();

        assert_eq!(first.len(), 1);
        assert!(second.is_empty(), "2回目は何もしない");
        assert_eq!(e.ledger.list().len(), 1);
        assert!(survey(&e.vault, &e.ledger).unwrap().is_empty());
    }

    /// 区分は `private + full`。持ち出し範囲は広がらない(既に Git の中にある)。
    #[test]
    fn the_boundary_is_written_as_it_already_is() {
        let e = env();
        legacy(&e.vault, "設計メモ", "図.png", b"png bytes");
        let done = migrate(&e.vault, &e.ledger, "ws-a", AT).unwrap();
        let m = e.ledger.get(&done[0].id).unwrap().unwrap();

        assert_eq!(m.policy.sync, SyncPolicy::Full);
        assert!(!m.policy.client_repo);
        assert_eq!(m.created.origin, "migration");
        // 実体の場所は旧サイドカーのまま
        assert!(matches!(m.locator, Locator::LegacyGit { .. }));
        assert!(m.events.iter().any(|ev| ev.kind == "migrated"));
    }

    /// 参照名は本文に書かれうるので英字に寄せる。日本語だけなら照合値の頭を使う。
    #[test]
    fn reference_names_stay_writable_even_for_japanese_file_names() {
        let e = env();
        legacy(&e.vault, "設計", "local-agent-control-room.html", b"a");
        legacy(&e.vault, "別の設計", "図面.png", b"b");
        let done = migrate(&e.vault, &e.ledger, "ws-a", AT).unwrap();

        let names: Vec<String> = done.iter().map(|d| d.ref_name.to_string()).collect();
        assert!(names.contains(&"local-agent-control-room".to_string()));
        assert!(
            names.iter().any(|n| n.starts_with("legacy-")),
            "英字が残らない名前は照合値へ倒す: {names:?}"
        );
    }

    #[test]
    fn colliding_reference_names_get_a_number() {
        let e = env();
        legacy(&e.vault, "一つ目", "図.png", b"a");
        legacy(&e.vault, "二つ目", "図.png", b"b");
        let done = migrate(&e.vault, &e.ledger, "ws-a", AT).unwrap();

        let names: Vec<String> = done.iter().map(|d| d.ref_name.to_string()).collect();
        assert_eq!(names.len(), 2);
        assert_ne!(names[0], names[1], "参照名は保管庫の中で一意: {names:?}");
    }

    #[test]
    fn promotion_plan_is_read_only_deterministic_and_snapshot_bound() {
        let e = env();
        let (_note_id, migrated) = migrated_legacy(&e, b"legacy bytes");
        let before = e.ledger.get(&migrated.id).unwrap().unwrap();

        let first = plan_promotions(&e.vault, &e.ledger).unwrap();
        let second = plan_promotions(&e.vault, &e.ledger).unwrap();

        assert_eq!(first, second);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].artifact_id, migrated.id);
        assert_eq!(first[0].manifest_version, before.version);
        assert_eq!(first[0].hash, before.hash);
        assert_eq!(first[0].size, before.created.size);
        assert!(first[0].destination.ends_with(first[0].hash.as_str()));
        assert_eq!(e.ledger.get(&migrated.id).unwrap().unwrap(), before);
    }

    /// 2026-09-08: 旧proseでは保持コピーの根拠が不足する。固定入力全体を型付き証拠で照合する。
    #[test]
    fn promotion_proof_roundtrips_and_binds_every_plan_field() {
        let e = env();
        migrated_legacy(&e, b"synthetic promotion proof");
        let plan = plan_promotions(&e.vault, &e.ledger).unwrap().remove(0);
        let same = plan_promotions(&e.vault, &e.ledger).unwrap().remove(0);
        let mut event = crate::artifact::Event {
            at: AT.into(),
            kind: PROMOTION_PROOF_EVENT_KIND.into(),
            detail: serde_json::to_string(&plan).unwrap(),
        };
        assert_eq!(event.detail, serde_json::to_string(&same).unwrap());
        assert_eq!(parse_promotion_proof(&event).unwrap(), Some(plan.clone()));
        for kind in [
            "legacy-promoted",
            "migrated",
            "legacy-promotion-rolled-back",
        ] {
            event.kind = kind.into();
            assert_eq!(parse_promotion_proof(&event).unwrap(), None);
        }
        event.kind = PROMOTION_PROOF_EVENT_KIND.into();
        let mut reference = serde_json::to_value(&plan.reference).unwrap();
        reference["revision"] = serde_json::json!(plan.reference.as_ref().unwrap().revision + 1);
        for (field, replacement) in [
            ("schema", serde_json::json!("unknown/v2")),
            ("plan_id", serde_json::json!("sha256:invalid")),
            ("artifact_id", serde_json::json!(ArtifactId::new(1))),
            (
                "manifest_version",
                serde_json::json!(plan.manifest_version + 1),
            ),
            ("note_id", serde_json::json!("notes/other")),
            ("file_name", serde_json::json!("other.bin")),
            (
                "legacy_path",
                serde_json::json!("/notes/other.files/other.bin"),
            ),
            (
                "hash",
                serde_json::json!(ContentHash::of_bytes(b"different")),
            ),
            ("size", serde_json::json!(plan.size + 1)),
            (
                "destination",
                serde_json::json!(".kb-artifacts/lfs/different"),
            ),
            ("reference", reference),
            ("aliases", serde_json::json!([])),
        ] {
            let mut changed = serde_json::to_value(&plan).unwrap();
            changed[field] = replacement;
            event.detail = serde_json::to_string(&changed).unwrap();
            assert!(
                parse_promotion_proof(&event).is_err(),
                "unbound field: {field}"
            );
        }
    }

    /// 2026-09-08: 再計算したplan_idがあっても任意pathや不正newtypeを証拠へ受け入れない。
    #[test]
    fn promotion_proof_rejects_noncanonical_paths_and_invalid_typed_evidence() {
        let e = env();
        migrated_legacy(&e, b"synthetic canonical proof");
        let plan = plan_promotions(&e.vault, &e.ledger).unwrap().remove(0);
        let reference = plan.reference.as_ref().unwrap();
        for (field, replacement) in [
            ("note_id", serde_json::json!("notes/../outside")),
            ("file_name", serde_json::json!("../outside.bin")),
            (
                "legacy_path",
                serde_json::json!("/notes/other.files/other.bin"),
            ),
            ("destination", serde_json::json!("/tmp/other.bin")),
            ("artifact_id", serde_json::json!("not-an-id")),
            ("hash", serde_json::json!("../outside")),
            ("manifest_version", serde_json::json!(0)),
            ("manifest_version", serde_json::json!(u64::MAX)),
            (
                "reference",
                serde_json::json!({"name":reference.name,"revision":0}),
            ),
            (
                "reference",
                serde_json::json!({"name":"Invalid-Ref","revision":1}),
            ),
            ("reference", serde_json::Value::Null),
            (
                "aliases",
                serde_json::json!([["/notes/old.files/a.bin", "different-ref"]]),
            ),
            (
                "aliases",
                serde_json::json!([plan.aliases[0], plan.aliases[0]]),
            ),
        ] {
            let mut value = serde_json::to_value(&plan).unwrap();
            value[field] = replacement;
            let mut changed: PromotionPlan = serde_json::from_value(value).unwrap();
            changed.plan_id = promotion_plan_id(&changed).unwrap();
            let event = crate::artifact::Event {
                at: AT.into(),
                kind: PROMOTION_PROOF_EVENT_KIND.into(),
                detail: serde_json::to_string(&changed).unwrap(),
            };
            assert!(
                parse_promotion_proof(&event).is_err(),
                "accepted invalid {field}"
            );
        }
        let mut unknown = serde_json::to_value(&plan).unwrap();
        unknown["future_path"] = serde_json::json!("/tmp/unvalidated");
        let event = crate::artifact::Event {
            at: AT.into(),
            kind: PROMOTION_PROOF_EVENT_KIND.into(),
            detail: serde_json::to_string(&unknown).unwrap(),
        };
        assert!(parse_promotion_proof(&event).is_err());
        let mut unreferenced = plan;
        unreferenced.reference = None;
        unreferenced.aliases.clear();
        unreferenced.plan_id = promotion_plan_id(&unreferenced).unwrap();
        validate_promotion_plan(&unreferenced).unwrap();
    }

    #[test]
    fn upload_failure_never_switches_primary_locator() {
        let e = env();
        let (_note_id, migrated) = migrated_legacy(&e, b"legacy bytes");
        let plan = plan_promotions(&e.vault, &e.ledger).unwrap().remove(0);
        let mut upload = MockUpload {
            fail: true,
            ..Default::default()
        };

        assert!(apply_promotion_with(&e.vault, &e.ledger, &plan, AT, &mut upload, false).is_err());
        let current = e.ledger.get(&migrated.id).unwrap().unwrap();
        assert!(matches!(current.locator, Locator::LegacyGit { .. }));
        assert_eq!(current.version, plan.manifest_version);
        assert!(
            current
                .events
                .iter()
                .all(|event| event.kind != PROMOTION_PROOF_EVENT_KIND)
        );
    }

    #[test]
    fn stale_manifest_ref_or_alias_rejects_the_plan_before_upload() {
        let e = env();
        let (_note_id, migrated) = migrated_legacy(&e, b"legacy bytes");
        let plan = plan_promotions(&e.vault, &e.ledger).unwrap().remove(0);
        let mut manifest = e.ledger.get(&migrated.id).unwrap().unwrap();
        manifest
            .apply(
                manifest.version,
                crate::artifact::Change {
                    display_name: Some("changed.png".into()),
                    ..Default::default()
                },
                false,
            )
            .unwrap();
        e.ledger.put(&e.vault, &manifest).unwrap();
        let mut upload = MockUpload::default();

        assert!(apply_promotion_with(&e.vault, &e.ledger, &plan, AT, &mut upload, false).is_err());
        assert_eq!(upload.calls, 0);
    }

    #[test]
    fn successful_promotion_is_locator_only_idempotent_and_keeps_old_links() {
        let e = env();
        let (note_id, migrated) = migrated_legacy(&e, b"legacy bytes");
        let old_path = e.vault.legacy_attachment_path(&note_id, "図.png").unwrap();
        let before = e.ledger.get(&migrated.id).unwrap().unwrap();
        let before_ref = e.ledger.ref_for(&migrated.id).unwrap();
        let before_aliases = e.ledger.aliases();
        let plan = plan_promotions(&e.vault, &e.ledger).unwrap().remove(0);
        let mut upload = MockUpload::default();

        let applied =
            apply_promotion_with(&e.vault, &e.ledger, &plan, AT, &mut upload, false).unwrap();
        let after = e.ledger.get(&migrated.id).unwrap().unwrap();
        assert_eq!(after.id, before.id);
        assert_eq!(after.hash, before.hash);
        assert_eq!(after.created, before.created);
        assert_eq!(after.policy, before.policy);
        assert_eq!(after.notes, before.notes);
        assert!(matches!(after.locator, Locator::Managed { .. }));
        assert!(old_path.is_file(), "旧実体はfallbackとして残す");
        assert_eq!(e.ledger.ref_for(&migrated.id).unwrap(), before_ref);
        assert_eq!(e.ledger.aliases(), before_aliases);
        let proofs = after
            .events
            .iter()
            .filter_map(|event| parse_promotion_proof(event).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(proofs, vec![plan.clone()]);
        assert!(
            after
                .events
                .iter()
                .any(|event| event.kind == "legacy-promoted")
        );

        let retried =
            apply_promotion_with(&e.vault, &e.ledger, &plan, AT, &mut upload, false).unwrap();
        assert!(retried.already_applied);
        assert_eq!(upload.calls, 1, "retryで再uploadしない");
        assert_eq!(retried.after_version, applied.after_version);
        assert_eq!(
            e.ledger.get(&migrated.id).unwrap().unwrap().events,
            after.events
        );

        let old_link = format!("/{note_id}.files/図.png");
        let resolved = parse_and_resolve(&e, &old_link);
        assert_eq!(resolved.manifest.id, migrated.id);
        let mut opened = resolved.open(&e.vault, &e.stores).unwrap().unwrap();
        let mut bytes = Vec::new();
        opened.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"legacy bytes");
    }

    #[test]
    fn rollback_requires_the_exact_post_apply_version_and_keeps_both_copies() {
        let e = env();
        let (note_id, migrated) = migrated_legacy(&e, b"legacy bytes");
        let plan = plan_promotions(&e.vault, &e.ledger).unwrap().remove(0);
        let mut upload = MockUpload::default();
        let applied =
            apply_promotion_with(&e.vault, &e.ledger, &plan, AT, &mut upload, false).unwrap();

        let rolled_back =
            rollback_promotion_with(&e.vault, &e.ledger, &applied, AT, false).unwrap();
        let current = e.ledger.get(&migrated.id).unwrap().unwrap();
        assert!(
            matches!(&current.locator, Locator::LegacyGit { note_id: restored_note, file_name } if restored_note == &note_id && file_name == "図.png")
        );
        assert!(
            e.vault
                .legacy_attachment_path(&note_id, "図.png")
                .unwrap()
                .is_file()
        );
        assert_eq!(
            crate::lfs::verify(&e.vault, &plan.hash).unwrap(),
            crate::store::Verified::Ok
        );
        assert!(rollback_promotion_with(&e.vault, &e.ledger, &applied, AT, false).is_err());
        assert_eq!(rolled_back.after_version, applied.after_version + 1);
        assert_eq!(
            current
                .events
                .iter()
                .filter_map(|event| parse_promotion_proof(event).unwrap())
                .collect::<Vec<_>>(),
            vec![plan.clone()]
        );
        assert_eq!(
            fs::read(e.vault.legacy_attachment_path(&note_id, "図.png").unwrap()).unwrap(),
            b"legacy bytes"
        );

        // 復元後の再昇格は別version/planになる。監査は過去proofだけで現在状態を決めない。
        let next_plan = plan_promotions(&e.vault, &e.ledger).unwrap().remove(0);
        assert_ne!(next_plan.plan_id, plan.plan_id);
        apply_promotion_with(&e.vault, &e.ledger, &next_plan, AT, &mut upload, false).unwrap();
        let next = e.ledger.get(&migrated.id).unwrap().unwrap();
        assert_eq!(
            next.events
                .iter()
                .filter_map(|event| parse_promotion_proof(event).unwrap())
                .collect::<Vec<_>>(),
            vec![plan, next_plan]
        );
    }

    /// 2026-09-08: 旧版が書いたproseだけの成功記録はretryできるが、新しいproofを捏造しない。
    #[test]
    fn legacy_prose_only_retry_does_not_backfill_structured_promotion_proof() {
        let e = env();
        let (_, migrated) = migrated_legacy(&e, b"legacy retry bytes");
        let plan = plan_promotions(&e.vault, &e.ledger).unwrap().remove(0);
        let mut upload = MockUpload::default();
        apply_promotion_with(&e.vault, &e.ledger, &plan, AT, &mut upload, false).unwrap();
        let mut previous_format = e.ledger.get(&migrated.id).unwrap().unwrap();
        previous_format
            .events
            .retain(|event| event.kind != PROMOTION_PROOF_EVENT_KIND);
        e.ledger.put(&e.vault, &previous_format).unwrap();
        let retried =
            apply_promotion_with(&e.vault, &e.ledger, &plan, AT, &mut upload, false).unwrap();
        assert!(retried.already_applied);
        assert_eq!(upload.calls, 1);
        assert_eq!(
            e.ledger.get(&migrated.id).unwrap().unwrap(),
            previous_format
        );
        assert!(
            previous_format
                .events
                .iter()
                .all(|event| parse_promotion_proof(event).unwrap().is_none())
        );
    }
}
