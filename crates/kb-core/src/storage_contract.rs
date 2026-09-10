//! 保存形式に依存しない正本の検査・論理 export — Storage Contract / ADR-0004。
//!
//! 現行 backend は Markdown + Git だが、正本の条件はその形式ではない。この module が
//! 作る決定的な snapshot と同じ意味を別 backend からも出せ、clone 後に同じ digest へ
//! 戻せることを互換条件にする。`.kb/index.db` と `index.md` は派生なので含めない。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::artifact::{
    ArtifactId, ArtifactRef, ContentHash, Locator, Manifest, RefName, SyncPolicy,
};
use crate::authority::{AuthorityRole, AuthorityStatus, RelationKind};
use crate::frontmatter::{Frontmatter, Note};
use crate::ledger;
use crate::provenance::NoteEvent;
use crate::vault::Vault;

pub const SCHEMA_V1: &str = "kb-app.repository-snapshot/v1";
pub const SCHEMA_V2: &str = "kb-app.repository-snapshot/v2";

/// backend を交換するときの比較単位。物理パスや索引表ではなく、利用者が読む論理内容を持つ。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositorySnapshotV1 {
    pub schema: String,
    pub workspace_id: String,
    pub notes: Vec<SnapshotNote>,
    pub artifacts: Vec<Manifest>,
    pub artifact_refs: Vec<ArtifactRef>,
    pub artifact_aliases: BTreeMap<String, String>,
    /// OKF の監査ログ。無い保管庫では `null`。
    pub audit_log: Option<String>,
    /// 旧 transport の bytes は JSON に複製せず、パス・長さ・hash で同一性を表す。
    pub legacy_files: Vec<SnapshotFile>,
    /// 未指定は旧snapshotと同じbytes。指定があるsnapshotだけv2として比較する。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag_vocabulary_source: Option<crate::tag_vocabulary_source::SourceBinding>,
    /// ノートの来歴(契約20)。イベントの無い保管庫の digest を変えないため、
    /// 空なら serialize しない。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<NoteEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotNote {
    pub id: String,
    pub frontmatter: Frontmatter,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// `verify` の機械可読な結果。digest は snapshot の compact JSON に対する SHA-256。
#[derive(Debug, Clone, Serialize)]
pub struct StorageReport {
    pub schema: String,
    pub digest: String,
    pub notes: usize,
    pub artifacts: usize,
    pub artifact_refs: usize,
    pub legacy_files: usize,
    pub events: usize,
    /// 最新イベントの doc_hash が現在の document と一致しないノート数。
    /// 来歴は追記専用の台帳で、ノート正本そのものではない — 件数だけ示し verify は落とさない。
    pub provenance_mismatches: usize,
    pub legacy_inventory: LegacyInventory,
}

/// Artifact件数と物理path件数を分け、保持の根拠が足りない実体を完了へ数えない。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LegacyInventory {
    pub unpromoted_artifacts: usize,
    pub active_legacy_files: usize,
    pub retained_legacy_files: usize,
    pub unclassified_legacy_files: usize,
}

impl LegacyInventory {
    pub fn summary(&self) -> String {
        let prefix = if self.unpromoted_artifacts == 0
            && self.active_legacy_files == 0
            && self.unclassified_legacy_files == 0
            && self.retained_legacy_files > 0
        {
            "物理昇格完了・旧実体保持中: "
        } else {
            ""
        };
        format!(
            "{prefix}未昇格Artifact {} / 現役旧実体 {} / 保持旧実体 {} / 未分類旧実体 {}",
            self.unpromoted_artifacts,
            self.active_legacy_files,
            self.retained_legacy_files,
            self.unclassified_legacy_files,
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RepositoryExportV1 {
    pub digest: String,
    pub snapshot: RepositorySnapshotV1,
}

/// 正本を厳密に読み、論理 snapshot を作る。修復や書き込みは一切しない。
pub fn snapshot(vault: &Vault) -> Result<RepositorySnapshotV1> {
    let workspace_id = crate::workspace::stored_workspace_id(vault)?;
    let notes = read_notes(vault)?;
    validate_note_authority(&notes)?;
    let note_ids: BTreeSet<&str> = notes.iter().map(|n| n.id.as_str()).collect();
    let tracked = vault.root.join(ledger::DIR);
    let artifacts =
        read_json_dir::<Manifest, _>(&tracked.join("manifests"), |m| m.id.as_str().to_string())?;
    let artifact_refs =
        read_json_dir::<ArtifactRef, _>(&tracked.join("refs"), |r| r.name.as_str().to_string())?;
    validate_artifacts(vault, &workspace_id, &note_ids, &artifacts, &artifact_refs)?;
    let artifact_aliases = read_aliases(&tracked.join("aliases.json"), &artifact_refs)?;
    let audit_log = read_optional_text(&vault.root.join("log.md"))?;
    let legacy_files = read_legacy_files(vault)?;
    // 壊れた行は数えるだけで snapshot から落とす(読める分の再現性を優先する)。
    let (events, _broken) = crate::provenance::read_events(&vault.root)?;
    let tag_vocabulary_source = crate::tag_vocabulary_source::exported_binding(vault)?;
    if let Some(binding) = &tag_vocabulary_source {
        let source = notes
            .iter()
            .find(|note| note.frontmatter.note_uid.as_ref() == Some(&binding.note_uid))
            .context("語彙正本のUIDが復元用ノートに存在しない")?;
        crate::tag_vocabulary_source::ensure_source_eligible(&Note {
            front: source.frontmatter.clone(),
            body: source.body.clone(),
        })?;
    }

    let snapshot = RepositorySnapshotV1 {
        schema: if tag_vocabulary_source.is_some() {
            SCHEMA_V2
        } else {
            SCHEMA_V1
        }
        .to_string(),
        workspace_id,
        notes,
        artifacts,
        artifact_refs,
        artifact_aliases,
        audit_log,
        legacy_files,
        tag_vocabulary_source,
        events,
    };
    legacy_inventory(&snapshot)?;
    Ok(snapshot)
}

pub fn export(vault: &Vault) -> Result<RepositoryExportV1> {
    let snapshot = snapshot(vault)?;
    let digest = digest(&snapshot)?;
    Ok(RepositoryExportV1 { digest, snapshot })
}

pub fn verify(vault: &Vault) -> Result<StorageReport> {
    let export = export(vault)?;
    let provenance_mismatches = count_provenance_mismatches(vault, &export.snapshot.events)?;
    Ok(StorageReport {
        schema: export.snapshot.schema.clone(),
        digest: export.digest,
        notes: export.snapshot.notes.len(),
        artifacts: export.snapshot.artifacts.len(),
        artifact_refs: export.snapshot.artifact_refs.len(),
        legacy_files: export.snapshot.legacy_files.len(),
        events: export.snapshot.events.len(),
        provenance_mismatches,
        legacy_inventory: legacy_inventory(&export.snapshot)?,
    })
}

/// note_uid を持つノートのうち、最新イベントの doc_hash が現在の Markdown と
/// 食い違う件数。来歴は追記専用で後から直さないので、ここでは数えるだけにする。
fn count_provenance_mismatches(vault: &Vault, events: &[NoteEvent]) -> Result<usize> {
    if events.is_empty() {
        return Ok(0);
    }
    let mut latest: BTreeMap<&str, &NoteEvent> = BTreeMap::new();
    for event in events {
        let Some(uid) = event.note_uid.as_deref() else {
            continue;
        };
        latest
            .entry(uid)
            .and_modify(|current| {
                // event_idだけでは時系列にならない(蒸留のop_idはsha256由来)。
                if (&current.at, &current.event_id) < (&event.at, &event.event_id) {
                    *current = event;
                }
            })
            .or_insert(event);
    }

    let mut mismatches = 0usize;
    for (id, path) in vault.list_note_files()? {
        let text = fs::read_to_string(&path)
            .with_context(|| format!("ノートが読めない: {}", path.display()))?;
        let note = Note::parse(&text).with_context(|| format!("ノートの形式が壊れている: {id}"))?;
        let Some(uid) = note.front.note_uid.as_ref() else {
            continue;
        };
        let Some(event) = latest.get(uid.as_str()) else {
            continue;
        };
        if event.doc_hash.as_deref() != Some(crate::provenance::document_hash(&text).as_str()) {
            mismatches += 1;
        }
    }
    Ok(mismatches)
}

fn legacy_inventory(snapshot: &RepositorySnapshotV1) -> Result<LegacyInventory> {
    let mut inventory = LegacyInventory::default();
    let mut active = BTreeSet::new();
    let mut retained = BTreeSet::new();
    let files: BTreeMap<&str, &SnapshotFile> = snapshot
        .legacy_files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    for manifest in &snapshot.artifacts {
        if let Locator::LegacyGit { note_id, file_name } = &manifest.locator {
            inventory.unpromoted_artifacts += 1;
            active.insert(format!("{note_id}.files/{file_name}"));
            continue;
        }
        let Locator::Managed { hash } = &manifest.locator else {
            continue;
        };
        // 2026-09-08 (#88): 同hashや過去の散文だけでは保持コピーと証明できない。
        // rollbackや新しい昇格より前の証拠を、現在の昇格へ流用しない。
        let latest = manifest.events.iter().rev().find(|event| {
            matches!(
                event.kind.as_str(),
                crate::migrate::PROMOTION_PROOF_EVENT_KIND
                    | "legacy-promoted"
                    | "legacy-promotion-rolled-back"
            )
        });
        let Some(event) = latest else { continue };
        let Some(proof) = crate::migrate::parse_promotion_proof(event)? else {
            continue;
        };
        if proof.artifact_id != manifest.id
            || proof.hash != manifest.hash
            || &proof.hash != hash
            || proof.size != manifest.created.size
            || proof
                .manifest_version
                .checked_add(1)
                .is_none_or(|v| v > manifest.version)
        {
            bail!(
                "昇格証拠の固定identityまたはversionが現在のArtifactと一致しない: {}",
                manifest.id
            );
        }
        let path = proof
            .legacy_path
            .strip_prefix('/')
            .context("旧pathの形式が不正")?;
        let file = files
            .get(path)
            .with_context(|| format!("保持を記録した旧実体がない: {}", proof.legacy_path))?;
        if file.sha256 != proof.hash.as_str() || file.size != proof.size {
            bail!(
                "保持を記録した旧実体とpromotion証拠が一致しない: {}",
                proof.legacy_path
            );
        }
        let current_refs: Vec<_> = snapshot
            .artifact_refs
            .iter()
            .filter(|reference| reference.artifact_id == manifest.id)
            .collect();
        let references_match = match (&proof.reference, current_refs.as_slice()) {
            (None, []) => true,
            (Some(expected), [current]) => {
                current.name == expected.name && current.revision == expected.revision
            }
            _ => false,
        };
        let current_aliases: Vec<_> = proof
            .reference
            .as_ref()
            .map(|reference| {
                snapshot
                    .artifact_aliases
                    .iter()
                    .filter(|(_, name)| name.as_str() == reference.name.as_str())
                    .map(|(path, name)| (path.clone(), name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        if references_match && current_aliases == proof.aliases {
            retained.insert(path.to_string());
        }
    }
    for file in &snapshot.legacy_files {
        // 同じpathを複数Artifactが使う場合、現役が一つでもあれば保持専用ではない。
        if active.contains(&file.path) {
            inventory.active_legacy_files += 1;
        } else if retained.contains(&file.path) {
            inventory.retained_legacy_files += 1;
        } else {
            inventory.unclassified_legacy_files += 1;
        }
    }
    Ok(inventory)
}

fn digest(snapshot: &RepositorySnapshotV1) -> Result<String> {
    let bytes = serde_json::to_vec(snapshot).context("論理 snapshot の serialize")?;
    Ok(hex_lower(&Sha256::digest(bytes)))
}

fn read_notes(vault: &Vault) -> Result<Vec<SnapshotNote>> {
    vault
        .list_note_files()?
        .into_iter()
        .map(|(id, path)| {
            let text = fs::read_to_string(&path)
                .with_context(|| format!("ノートが読めない: {}", path.display()))?;
            let Note { front, body } =
                Note::parse(&text).with_context(|| format!("ノートの形式が壊れている: {id}"))?;
            Ok(SnapshotNote {
                id,
                frontmatter: front,
                body,
            })
        })
        .collect()
}

pub(crate) fn validate_note_authority(notes: &[SnapshotNote]) -> Result<()> {
    let mut by_uid = BTreeMap::new();
    let mut active_canonical = BTreeMap::new();
    for note in notes {
        let front = &note.frontmatter;
        crate::proposal_workflow::guard_import(
            None,
            &Note {
                front: front.clone(),
                body: note.body.clone(),
            },
        )?;
        crate::authority::validate_envelope(
            front.note_uid.as_ref(),
            front.authority.as_ref(),
            &front.relations,
        )?;
        let Some(uid) = &front.note_uid else {
            continue;
        };
        if let Some(existing) = by_uid.insert(uid.as_str(), note) {
            bail!(
                "note_uidが重複している: {} ({}, {})",
                uid,
                existing.id,
                note.id
            );
        }
        let authority = front.authority.as_ref().expect("envelope検証済み");
        if authority.is_active_canonical() {
            let key = (authority.namespace.as_str(), authority.scope.as_str());
            if let Some(existing) = active_canonical.insert(key, note.id.as_str()) {
                bail!(
                    "active canonicalが重複している: {}/{} ({existing}, {})",
                    authority.namespace.as_str(),
                    authority.scope,
                    note.id
                );
            }
        }
    }

    let mut superseded_targets = BTreeSet::new();
    for source in notes {
        let Some(source_uid) = &source.frontmatter.note_uid else {
            continue;
        };
        let source_authority = source
            .frontmatter
            .authority
            .as_ref()
            .expect("envelope検証済み");
        for relation in &source.frontmatter.relations {
            let target = by_uid.get(relation.target.as_str()).ok_or_else(|| {
                anyhow::anyhow!(
                    "typed relationの参照先がない: {} {} -> {}",
                    source.id,
                    relation.kind.as_str(),
                    relation.target
                )
            })?;
            if relation.kind != RelationKind::Supersedes {
                continue;
            }
            let target_authority = target
                .frontmatter
                .authority
                .as_ref()
                .expect("UID付きnoteはauthority検証済み");
            if !source_authority.is_active_canonical()
                || target_authority.role != AuthorityRole::Canonical
                || target_authority.status != AuthorityStatus::Superseded
                || source_authority.namespace != target_authority.namespace
                || source_authority.scope != target_authority.scope
            {
                bail!(
                    "supersedesは同じnamespace/scopeのactive canonicalからsuperseded canonicalへ結ぶ: {} -> {}",
                    source_uid,
                    relation.target
                );
            }
            superseded_targets.insert(relation.target.as_str());
        }
    }

    for note in notes {
        let Some(authority) = &note.frontmatter.authority else {
            continue;
        };
        if authority.role == AuthorityRole::Canonical
            && authority.status == AuthorityStatus::Superseded
            && note
                .frontmatter
                .note_uid
                .as_ref()
                .is_none_or(|uid| !superseded_targets.contains(uid.as_str()))
        {
            bail!(
                "superseded canonicalに後継のsupersedes relationがない: {}",
                note.id
            );
        }
    }
    Ok(())
}

fn read_json_dir<T, F>(dir: &Path, key: F) -> Result<Vec<T>>
where
    T: DeserializeOwned,
    F: Fn(&T) -> String,
{
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let paths: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("台帳ディレクトリが読めない: {}", dir.display()))?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    for path in &paths {
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("json") {
            bail!("台帳ディレクトリに未知の項目がある: {}", path.display());
        }
    }
    let mut paths = paths;
    paths.sort();

    let mut out = Vec::with_capacity(paths.len());
    let mut keys = BTreeSet::new();
    for path in paths {
        let text = fs::read_to_string(&path)
            .with_context(|| format!("台帳が読めない: {}", path.display()))?;
        let value: T = serde_json::from_str(&text)
            .with_context(|| format!("台帳の形式が壊れている: {}", path.display()))?;
        let value_key = key(&value);
        let file_key = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if value_key != file_key {
            bail!(
                "台帳のファイル名と内部 ID が一致しない: {} ({value_key})",
                path.display()
            );
        }
        if !keys.insert(value_key.clone()) {
            bail!("台帳の ID が重複している: {value_key}");
        }
        out.push(value);
    }
    out.sort_by_key(&key);
    Ok(out)
}

fn validate_artifacts(
    vault: &Vault,
    workspace_id: &str,
    note_ids: &BTreeSet<&str>,
    artifacts: &[Manifest],
    refs: &[ArtifactRef],
) -> Result<()> {
    let artifact_ids: BTreeSet<&str> = artifacts.iter().map(|m| m.id.as_str()).collect();
    for manifest in artifacts {
        if ArtifactId::from_str(manifest.id.as_str()).is_err() {
            bail!("artifact_id の形式が不正: {}", manifest.id);
        }
        if ContentHash::from_str(manifest.hash.as_str()).is_err() {
            bail!("content_hash の形式が不正: {}", manifest.id);
        }
        if manifest.policy.sync != SyncPolicy::Full {
            bail!("local_only の台帳が Git 管理領域にある: {}", manifest.id);
        }
        if let Locator::Managed { hash } = &manifest.locator
            && hash != &manifest.hash
        {
            bail!(
                "managed locator と content_hash が一致しない: {}",
                manifest.id
            );
        }
        validate_tracked_payload(vault, manifest)?;
        for note_id in &manifest.notes {
            if !note_ids.contains(note_id.as_str()) {
                bail!(
                    "存在しないノートを指す artifact: {} -> {note_id}",
                    manifest.id
                );
            }
        }
    }

    for r in refs {
        if RefName::from_str(r.name.as_str()).is_err() {
            bail!("artifact_ref の名前が不正: {}", r.name);
        }
        if r.workspace_id != workspace_id {
            bail!("artifact_ref の workspace_id が一致しない: {}", r.name);
        }
        if !artifact_ids.contains(r.artifact_id.as_str()) {
            bail!("artifact_ref の参照先がない: {}", r.name);
        }
    }
    Ok(())
}

fn validate_tracked_payload(vault: &Vault, manifest: &Manifest) -> Result<()> {
    let path = match &manifest.locator {
        Locator::Managed { hash } => vault.root.join(ledger::DIR).join("lfs").join(hash.as_str()),
        Locator::LegacyGit { note_id, file_name } => {
            vault.legacy_attachment_path(note_id, file_name)?
        }
        Locator::Linked { .. } => {
            bail!(
                "端末外の linked locator が Git 管理領域にある: {}",
                manifest.id
            )
        }
    };
    let bytes = fs::read(&path)
        .with_context(|| format!("artifact の追跡実体がない: {}", path.display()))?;
    if bytes.starts_with(b"version https://git-lfs.github.com/spec/v1\n") {
        let pointer = std::str::from_utf8(&bytes).context("LFS pointer が UTF-8 ではない")?;
        let oid = pointer
            .lines()
            .find_map(|line| line.strip_prefix("oid sha256:"));
        let size = pointer
            .lines()
            .find_map(|line| line.strip_prefix("size "))
            .and_then(|s| s.parse::<u64>().ok());
        if oid != Some(manifest.hash.as_str()) || size != Some(manifest.created.size) {
            bail!("LFS pointer と manifest が一致しない: {}", manifest.id);
        }
        return Ok(());
    }

    if bytes.len() as u64 != manifest.created.size
        || hex_lower(&Sha256::digest(&bytes)) != manifest.hash.as_str()
    {
        bail!(
            "artifact の bytes と manifest が一致しない: {}",
            manifest.id
        );
    }
    Ok(())
}

fn read_aliases(path: &Path, refs: &[ArtifactRef]) -> Result<BTreeMap<String, String>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let text = fs::read_to_string(path)
        .with_context(|| format!("alias が読めない: {}", path.display()))?;
    let aliases: BTreeMap<String, String> = serde_json::from_str(&text)
        .with_context(|| format!("alias の形式が壊れている: {}", path.display()))?;
    let ref_names: BTreeSet<&str> = refs.iter().map(|r| r.name.as_str()).collect();
    for name in aliases.values() {
        if RefName::from_str(name).is_err() || !ref_names.contains(name.as_str()) {
            bail!("alias の参照先がない、または名前が不正: {name}");
        }
    }
    Ok(aliases)
}

fn read_optional_text(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    fs::read_to_string(path)
        .map(Some)
        .with_context(|| format!("監査ログが読めない: {}", path.display()))
}

fn read_legacy_files(vault: &Vault) -> Result<Vec<SnapshotFile>> {
    let mut out = Vec::new();
    let mut paths = BTreeSet::new();
    for entry in walkdir::WalkDir::new(&vault.root)
        .into_iter()
        .filter_entry(|e| e.file_name() != ".git" && e.file_name() != ".kb")
    {
        let entry = entry?;
        if !entry.file_type().is_file()
            || !entry.path().ancestors().any(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().ends_with(".files"))
            })
        {
            continue;
        }
        let bytes = fs::read(entry.path())
            .with_context(|| format!("旧ファイルが読めない: {}", entry.path().display()))?;
        let rel = legacy_relative_path(entry.path().strip_prefix(&vault.root)?)?;
        if !paths.insert(rel.clone()) {
            bail!("旧ファイルpathがsnapshot内で重複する: {rel}");
        }
        out.push(SnapshotFile {
            path: rel,
            size: bytes.len() as u64,
            sha256: hex_lower(&Sha256::digest(bytes)),
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn legacy_relative_path(relative: &Path) -> Result<String> {
    // Unixでbackslashはファイル名の一部。文字置換すると別実体が旧pathを偽装できる。
    // lossy UTF-8も異なる物理名を同じ識別子へ潰すので、正常成分だけを厳密に連結する。
    Ok(relative
        .components()
        .map(|component| match component {
            std::path::Component::Normal(name) => {
                name.to_str().context("旧ファイルpathがUTF-8ではない")
            }
            _ => bail!("旧ファイルpathに通常成分以外がある"),
        })
        .collect::<Result<Vec<_>>>()?
        .join("/"))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{Created, Policy, Role};
    use crate::authority::{Authority, NoteNamespace, NoteRelation, NoteUid, RelationKind};
    use crate::frontmatter::Note;
    use crate::index::{open_db, sync};
    use crate::ledger::Ledger;
    use tempfile::tempdir;

    fn authority_note(
        title: &str,
        uid: NoteUid,
        authority: Authority,
        relations: Vec<NoteRelation>,
    ) -> Note {
        let mut front = Frontmatter::new_note(title);
        front.origin = Some("agent".into());
        front.tags = vec!["test".into()];
        front.note_uid = Some(uid);
        front.authority = Some(authority);
        front.relations = relations;
        Note {
            front,
            body: "本文".into(),
        }
    }

    const LEGACY_BYTES: &[u8] = b"synthetic legacy audit fixture";

    fn legacy_fixture() -> (
        tempfile::TempDir,
        Vault,
        Ledger,
        crate::migrate::PromotionPlan,
    ) {
        let dir = tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        let note = vault
            .propose_for_test(
                "合成添付監査",
                "合成本文",
                None,
                &["test".into()],
                "test/agent",
            )
            .unwrap();
        let path = vault.legacy_attachment_path(&note, "old.bin").unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, LEGACY_BYTES).unwrap();
        let ledger = Ledger::at(vault.root.clone(), dir.path().join("sidecar"));
        crate::migrate::migrate(
            &vault,
            &ledger,
            &crate::workspace::stored_workspace_id(&vault).unwrap(),
            "2026-09-08T00:00:00Z",
        )
        .unwrap();
        let plan = crate::migrate::plan_promotions(&vault, &ledger)
            .unwrap()
            .remove(0);
        (dir, vault, ledger, plan)
    }

    fn promoted_fixture(vault: &Vault, ledger: &Ledger, plan: &crate::migrate::PromotionPlan) {
        let path = vault.root.join(&plan.destination);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, LEGACY_BYTES).unwrap();
        let mut manifest = ledger.get(&plan.artifact_id).unwrap().unwrap();
        manifest
            .promote_legacy_locator(plan.manifest_version, &plan.note_id, &plan.file_name)
            .unwrap();
        manifest.record("2026-09-08T00:01:00Z", "legacy-promoted", &plan.plan_id);
        manifest.record(
            "2026-09-08T00:01:00Z",
            crate::migrate::PROMOTION_PROOF_EVENT_KIND,
            &serde_json::to_string(plan).unwrap(),
        );
        ledger.put(vault, &manifest).unwrap();
    }

    /// 2026-09-08 (#88): 物理総数を減らさず、昇格待ちと保持コピーを識別する。
    #[test]
    fn promotion_inventory_separates_artifacts_from_retained_physical_files() {
        let (_dir, vault, ledger, plan) = legacy_fixture();
        assert_eq!(
            verify(&vault).unwrap().legacy_inventory,
            LegacyInventory {
                unpromoted_artifacts: 1,
                active_legacy_files: 1,
                ..Default::default()
            }
        );
        promoted_fixture(&vault, &ledger, &plan);
        let report = verify(&vault).unwrap();
        assert_eq!(report.legacy_files, 1);
        assert_eq!(
            report.legacy_inventory,
            LegacyInventory {
                retained_legacy_files: 1,
                ..Default::default()
            }
        );
        assert!(
            crate::migrate::plan_promotions(&vault, &ledger)
                .unwrap()
                .is_empty()
        );
        let json = serde_json::to_value(report).unwrap();
        assert_eq!(json["legacy_inventory"]["unpromoted_artifacts"], 0);
    }

    /// 2026-09-08 (#88): 共有pathは一度だけ数え、未昇格Artifactが使う間は現役を優先する。
    #[test]
    fn shared_legacy_path_is_active_until_all_artifacts_are_promoted() {
        let (_dir, vault, ledger, first) = legacy_fixture();
        let mut second = ledger.get(&first.artifact_id).unwrap().unwrap();
        second.id = ArtifactId::new(1_788_825_601_000);
        ledger.put(&vault, &second).unwrap();
        let before = verify(&vault).unwrap();
        assert_eq!(before.legacy_inventory.unpromoted_artifacts, 2);
        assert_eq!(before.legacy_inventory.active_legacy_files, 1);
        promoted_fixture(&vault, &ledger, &first);
        let partial = verify(&vault).unwrap();
        assert_eq!(partial.legacy_inventory.unpromoted_artifacts, 1);
        assert_eq!(partial.legacy_inventory.active_legacy_files, 1);
        assert_eq!(partial.legacy_inventory.retained_legacy_files, 0);
        let second_plan = crate::migrate::plan_promotions(&vault, &ledger)
            .unwrap()
            .remove(0);
        promoted_fixture(&vault, &ledger, &second_plan);
        assert_eq!(
            verify(&vault)
                .unwrap()
                .legacy_inventory
                .retained_legacy_files,
            1
        );
        let mut manifest = ledger.get(&second.id).unwrap().unwrap();
        manifest
            .rollback_legacy_locator(
                manifest.version,
                &second_plan.note_id,
                &second_plan.file_name,
            )
            .unwrap();
        manifest.record(
            "2026-09-08T00:02:00Z",
            "legacy-promotion-rolled-back",
            &second_plan.plan_id,
        );
        ledger.put(&vault, &manifest).unwrap();
        let rolled_back = verify(&vault).unwrap();
        assert_eq!(rolled_back.legacy_inventory.unpromoted_artifacts, 1);
        assert_eq!(rolled_back.legacy_inventory.active_legacy_files, 1);
        assert_eq!(rolled_back.legacy_inventory.retained_legacy_files, 0);
        assert!(vault.root.join(&second_plan.destination).is_file());
    }

    /// 2026-09-08 (#88): 旧散文・同hashの無関係実体・変更されたref/aliasを保持へ推定しない。
    #[test]
    fn unproven_legacy_files_remain_unclassified() {
        let (_dir, vault, ledger, plan) = legacy_fixture();
        promoted_fixture(&vault, &ledger, &plan);
        let unrelated = vault
            .legacy_attachment_path(&plan.note_id, "unrelated.bin")
            .unwrap();
        fs::write(&unrelated, LEGACY_BYTES).unwrap();
        let original = snapshot(&vault).unwrap();
        let mut changed = original.clone();
        changed.artifacts[0]
            .events
            .retain(|event| event.kind != crate::migrate::PROMOTION_PROOF_EVENT_KIND);
        assert_eq!(
            legacy_inventory(&changed)
                .unwrap()
                .unclassified_legacy_files,
            2
        );
        let proven = legacy_inventory(&original).unwrap();
        assert_eq!(proven.retained_legacy_files, 1);
        assert_eq!(proven.unclassified_legacy_files, 1);
        changed = original.clone();
        changed.artifact_refs[0].revision += 1;
        assert_eq!(
            legacy_inventory(&changed)
                .unwrap()
                .unclassified_legacy_files,
            2
        );
        changed = original.clone();
        changed.artifact_aliases.clear();
        assert_eq!(
            legacy_inventory(&changed)
                .unwrap()
                .unclassified_legacy_files,
            2
        );
        changed = original;
        changed.artifacts[0].record(
            "2026-09-08T00:02:00Z",
            "legacy-promoted",
            "旧証拠を流用しない新しい昇格",
        );
        assert_eq!(
            legacy_inventory(&changed)
                .unwrap()
                .unclassified_legacy_files,
            2
        );
    }

    /// 2026-09-08 (#88): 保持を宣言した旧実体の改変・欠損とManaged実体の破損を完了扱いしない。
    #[test]
    fn corrupted_or_missing_retained_and_managed_payloads_fail_verification() {
        let (_dir, vault, ledger, plan) = legacy_fixture();
        promoted_fixture(&vault, &ledger, &plan);
        let legacy = vault
            .legacy_attachment_path(&plan.note_id, &plan.file_name)
            .unwrap();
        fs::write(&legacy, b"modified").unwrap();
        assert!(
            verify(&vault)
                .unwrap_err()
                .to_string()
                .contains("保持を記録した旧実体")
        );
        fs::remove_file(&legacy).unwrap();
        assert!(
            export(&vault)
                .unwrap_err()
                .to_string()
                .contains("保持を記録した旧実体がない")
        );
        fs::write(&legacy, LEGACY_BYTES).unwrap();
        let managed = vault.root.join(&plan.destination);
        fs::write(&managed, b"modified managed").unwrap();
        assert!(verify(&vault).is_err());
        fs::remove_file(&managed).unwrap();
        assert!(verify(&vault).is_err());
    }

    /// 2026-09-08 (#88): 構造化証拠の破損を旧形式扱いで黙って捨てない。
    #[test]
    fn malformed_promotion_proof_is_not_silently_accepted() {
        let (_dir, vault, ledger, plan) = legacy_fixture();
        promoted_fixture(&vault, &ledger, &plan);
        let mut manifest = ledger.get(&plan.artifact_id).unwrap().unwrap();
        manifest.events.last_mut().unwrap().detail = "{broken".into();
        ledger.put(&vault, &manifest).unwrap();
        assert!(verify(&vault).is_err());
    }

    /// 2026-09-08 (#88): 同bytesでもUnixのbackslash名を別の旧pathの存在証拠へ変換しない。
    #[cfg(unix)]
    #[test]
    fn literal_backslash_copy_cannot_replace_a_missing_retained_path() {
        let (_dir, vault, ledger, plan) = legacy_fixture();
        promoted_fixture(&vault, &ledger, &plan);
        assert!(plan.note_id.contains('/'));
        let counterfeit = vault
            .root
            .join(format!("{}.files", plan.note_id.replace('/', "\\")))
            .join(&plan.file_name);
        fs::create_dir_all(counterfeit.parent().unwrap()).unwrap();
        fs::write(&counterfeit, LEGACY_BYTES).unwrap();
        let report = verify(&vault).unwrap();
        assert_eq!(report.legacy_files, 2);
        assert_eq!(report.legacy_inventory.retained_legacy_files, 1);
        assert_eq!(report.legacy_inventory.unclassified_legacy_files, 1);
        let files = read_legacy_files(&vault).unwrap();
        assert!(files.iter().any(|file| file.path.contains('\\')));
        fs::remove_file(
            vault
                .legacy_attachment_path(&plan.note_id, &plan.file_name)
                .unwrap(),
        )
        .unwrap();
        assert!(
            verify(&vault)
                .unwrap_err()
                .to_string()
                .contains("保持を記録した旧実体がない")
        );
        assert!(counterfeit.is_file());
    }

    /// 2026-09-08 (#88): 不正UTF-8を置換文字へ潰して別ファイルの証拠と混同しない。
    #[cfg(unix)]
    #[test]
    fn legacy_inventory_rejects_non_utf8_physical_paths() {
        use std::os::unix::ffi::OsStringExt;
        // macOSはこの名前の作成をEPERMで拒否するため、実際のpath変換口へ直接渡す。
        let invalid = PathBuf::from(std::ffi::OsString::from_vec(
            b"notes/a.files/invalid-\xff.bin".to_vec(),
        ));
        assert!(
            legacy_relative_path(&invalid)
                .unwrap_err()
                .to_string()
                .contains("旧ファイルpathがUTF-8ではない")
        );
        let replacement = "notes/a.files/invalid-\u{fffd}.bin";
        assert_eq!(
            legacy_relative_path(Path::new(replacement)).unwrap(),
            replacement
        );
    }

    /// 2026-09-08 (#88): 再hashした意味的不整合proofを捨てて旧copy欠損を全0へ隠さない。
    #[test]
    fn inconsistent_typed_promotion_binding_fails_even_when_the_old_copy_is_missing() {
        let (_dir, vault, ledger, plan) = legacy_fixture();
        promoted_fixture(&vault, &ledger, &plan);
        let current = ledger.get(&plan.artifact_id).unwrap().unwrap();
        fs::remove_file(
            vault
                .legacy_attachment_path(&plan.note_id, &plan.file_name)
                .unwrap(),
        )
        .unwrap();
        for field in ["artifact_id", "hash", "size", "manifest_version"] {
            let mut changed = plan.clone();
            match field {
                "artifact_id" => changed.artifact_id = ArtifactId::new(1),
                "hash" => {
                    changed.hash = ContentHash::of_bytes(b"other synthetic content");
                    changed.destination = format!(".kb-artifacts/lfs/{}", changed.hash);
                }
                "size" => changed.size += 1,
                _ => changed.manifest_version = current.version,
            }
            changed.plan_id.clear();
            changed.plan_id = format!(
                "sha256:{:x}",
                Sha256::digest(serde_json::to_vec(&changed).unwrap())
            );
            let mut manifest = current.clone();
            let proof = manifest.events.last_mut().unwrap();
            proof.detail = serde_json::to_string(&changed).unwrap();
            assert_eq!(
                crate::migrate::parse_promotion_proof(proof).unwrap(),
                Some(changed)
            );
            ledger.put(&vault, &manifest).unwrap();
            assert!(
                verify(&vault)
                    .unwrap_err()
                    .to_string()
                    .contains("昇格証拠の固定identityまたはversion"),
                "accepted {field}"
            );
        }
    }

    /// 2026-09-08 (#88): 未分類・現役旧実体を抱えた状態や空の保管庫を昇格完了と要約しない。
    #[test]
    fn legacy_inventory_summary_reports_completion_only_for_verified_retained_copies() {
        let retained = LegacyInventory {
            retained_legacy_files: 2,
            ..Default::default()
        };
        assert_eq!(
            retained.summary(),
            "物理昇格完了・旧実体保持中: 未昇格Artifact 0 / 現役旧実体 0 / 保持旧実体 2 / 未分類旧実体 0"
        );
        for inventory in [
            LegacyInventory::default(),
            LegacyInventory {
                unclassified_legacy_files: 1,
                ..retained.clone()
            },
            LegacyInventory {
                unpromoted_artifacts: 1,
                active_legacy_files: 1,
                ..retained.clone()
            },
            LegacyInventory {
                active_legacy_files: 1,
                ..retained
            },
        ] {
            let summary = inventory.summary();
            assert!(!summary.contains("物理昇格完了"));
            for label in ["未昇格Artifact", "現役旧実体", "保持旧実体", "未分類旧実体"]
            {
                assert!(summary.contains(label));
            }
        }
    }

    #[test]
    fn derived_index_does_not_change_the_repository_digest() {
        let dir = tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        vault
            .propose_for_test(
                "再現テスト",
                "索引は消せる。",
                None,
                &["test".into()],
                "test/agent",
            )
            .unwrap();
        let before = verify(&vault).unwrap().digest;

        let conn = open_db(&vault).unwrap();
        sync(&vault, &conn).unwrap();
        assert!(vault.index_db_path().is_file());
        drop(conn);
        fs::remove_dir_all(vault.root.join(".kb")).unwrap();
        vault.write_index_md().unwrap();

        assert_eq!(verify(&vault).unwrap().digest, before);
    }

    #[test]
    fn fresh_clone_reconstructs_the_same_logical_repository() {
        let dir = tempdir().unwrap();
        let source = Vault::create(dir.path().join("source")).unwrap();
        let note_id = source
            .propose_for_test(
                "クローン再現",
                "[[別端末]]でも意味が同じ。",
                Some("Storage Contract の受入テスト"),
                &["test".into()],
                "test/agent",
            )
            .unwrap();
        let workspace_id = crate::workspace::stored_workspace_id(&source).unwrap();
        let ledger = Ledger::at(source.root.clone(), dir.path().join("sidecar"));
        let bytes = b"tracked artifact";
        let hash = ContentHash::of_bytes(bytes);
        let artifact_id = ArtifactId::new(1_786_806_000_000);
        let mut manifest = Manifest::new(
            artifact_id.clone(),
            hash.clone(),
            Created {
                media_type: "application/octet-stream".into(),
                size: bytes.len() as u64,
                at: "2026-08-16T00:00:00Z".into(),
                origin: "test".into(),
                by: "test/agent".into(),
            },
            "原本.bin".into(),
            Locator::Managed { hash: hash.clone() },
            Policy::default_managed(),
            Role::File,
        );
        manifest.attach(1, &note_id).unwrap();
        let lfs_path = source
            .root
            .join(ledger::DIR)
            .join("lfs")
            .join(hash.as_str());
        fs::create_dir_all(lfs_path.parent().unwrap()).unwrap();
        fs::write(
            &lfs_path,
            format!(
                "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize {}\n",
                hash,
                bytes.len()
            ),
        )
        .unwrap();
        source
            .commit(
                &[&format!("{}/lfs/{}", ledger::DIR, hash)],
                "test: LFS pointer",
            )
            .unwrap();
        ledger.put(&source, &manifest).unwrap();
        let ref_name = RefName::from_str("source-file").unwrap();
        ledger
            .put_ref(
                &source,
                SyncPolicy::Full,
                &ArtifactRef::new(&workspace_id, ref_name.clone(), artifact_id),
            )
            .unwrap();
        ledger
            .put_alias(&source, "/notes/old.files/source.bin", &ref_name)
            .unwrap();
        fs::create_dir_all(source.root.join("notes/クローン再現.files")).unwrap();
        fs::write(
            source.root.join("notes/クローン再現.files/原本.bin"),
            b"portable",
        )
        .unwrap();
        source
            .commit(&["notes/クローン再現.files/原本.bin"], "test: legacy file")
            .unwrap();
        let expected = verify(&source).unwrap().digest;

        let clone_root = dir.path().join("clone");
        git2::Repository::clone(source.root.to_str().unwrap(), &clone_root).unwrap();
        let clone = Vault::open(&clone_root).unwrap();
        assert!(!clone.index_db_path().exists(), "派生 DB は clone されない");
        assert_eq!(verify(&clone).unwrap().digest, expected);

        let conn = open_db(&clone).unwrap();
        sync(&clone, &conn).unwrap();
        let found = crate::search::search(&conn, "クローン", 5);
        assert_eq!(found.hits.len(), 1);
    }

    #[test]
    fn verify_is_read_only_when_workspace_id_is_missing() {
        let dir = tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        fs::remove_file(vault.root.join(crate::workspace::ID_FILE)).unwrap();

        assert!(verify(&vault).is_err());
        assert!(!vault.root.join(crate::workspace::ID_FILE).exists());
    }

    #[test]
    fn malformed_tracked_manifest_is_not_silently_ignored() {
        let dir = tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        let manifests = vault.root.join(ledger::DIR).join("manifests");
        fs::create_dir_all(&manifests).unwrap();
        fs::write(manifests.join("broken.json"), "{not json").unwrap();

        let err = verify(&vault).unwrap_err().to_string();
        assert!(err.contains("形式が壊れている"), "{err}");
    }

    #[test]
    fn duplicate_active_canonical_scope_is_rejected() {
        let dir = tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        for (id, title, uid) in [
            ("notes/first", "一つ目", NoteUid::at(1)),
            ("notes/second", "二つ目", NoteUid::at(2)),
        ] {
            vault
                .write_note_fixture(
                    id,
                    &authority_note(
                        title,
                        uid,
                        Authority {
                            namespace: NoteNamespace::Decisions,
                            role: AuthorityRole::Canonical,
                            status: AuthorityStatus::Active,
                            scope: "kb-app/github-operations".into(),
                        },
                        Vec::new(),
                    ),
                )
                .unwrap();
        }

        let error = verify(&vault).unwrap_err().to_string();
        assert!(error.contains("active canonicalが重複"), "{error}");
    }

    #[test]
    fn relation_target_must_exist_in_the_same_snapshot() {
        let dir = tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        vault
            .write_note_fixture(
                "notes/source",
                &authority_note(
                    "参照元",
                    NoteUid::at(1),
                    Authority {
                        namespace: NoteNamespace::Knowledge,
                        role: AuthorityRole::Canonical,
                        status: AuthorityStatus::Active,
                        scope: "kb-app/relation-integrity".into(),
                    },
                    vec![NoteRelation {
                        kind: RelationKind::Supports,
                        target: NoteUid::at(99),
                    }],
                ),
            )
            .unwrap();

        let error = verify(&vault).unwrap_err().to_string();
        assert!(error.contains("参照先がない"), "{error}");
    }

    #[test]
    fn supersession_is_one_active_canonical_pointing_to_its_predecessor() {
        let dir = tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        let old_uid = NoteUid::at(1);
        vault
            .write_note_fixture(
                "notes/old",
                &authority_note(
                    "旧版",
                    old_uid.clone(),
                    Authority {
                        namespace: NoteNamespace::Procedures,
                        role: AuthorityRole::Canonical,
                        status: AuthorityStatus::Superseded,
                        scope: "kb-app/release".into(),
                    },
                    Vec::new(),
                ),
            )
            .unwrap();
        vault
            .write_note_fixture(
                "notes/current",
                &authority_note(
                    "現行版",
                    NoteUid::at(2),
                    Authority {
                        namespace: NoteNamespace::Procedures,
                        role: AuthorityRole::Canonical,
                        status: AuthorityStatus::Active,
                        scope: "kb-app/release".into(),
                    },
                    vec![NoteRelation {
                        kind: RelationKind::Supersedes,
                        target: old_uid,
                    }],
                ),
            )
            .unwrap();

        assert!(verify(&vault).is_ok());
    }

    /// 来歴イベントは snapshot の一部(契約20)だが、**イベントの無い保管庫の
    /// digest は変えない** — 旧 clone と新 clone の同値性をここで固定する。
    #[test]
    fn events_join_the_snapshot_without_moving_an_event_free_digest() {
        let dir = tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        let note = authority_note(
            "来歴なし",
            NoteUid::at(1),
            Authority {
                namespace: NoteNamespace::Records,
                role: crate::authority::AuthorityRole::Record,
                status: crate::authority::AuthorityStatus::Active,
                scope: "test/no-events".into(),
            },
            Vec::new(),
        );
        std::fs::write(
            vault.root.join("notes/legacy.md"),
            note.to_file_string().unwrap(),
        )
        .unwrap();

        let without = verify(&vault).unwrap();
        assert_eq!(without.events, 0);
        assert_eq!(without.provenance_mismatches, 0);
        let snapshot = snapshot(&vault).unwrap();
        let value = serde_json::to_value(&snapshot).unwrap();
        assert!(
            !value.as_object().unwrap().contains_key("events"),
            "空のeventsはserializeしない: {value}"
        );

        // eventsキーを知らない旧schemaのJSONも、同じ digest で読み戻せる
        let legacy: RepositorySnapshotV1 = serde_json::from_value(value).unwrap();
        assert_eq!(digest(&legacy).unwrap(), without.digest);
    }

    #[test]
    fn provenance_events_are_counted_and_mismatches_are_reported_without_failing() {
        let dir = tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        let id = vault
            .propose_for_test(
                "来歴つき",
                "本文。",
                None,
                &["test".into()],
                "codex-cli/gpt-5.6-sol",
            )
            .unwrap();

        let report = verify(&vault).unwrap();
        assert_eq!(report.events, 1);
        assert_eq!(report.provenance_mismatches, 0);

        // 外部編集で本文だけが進むと、最新イベントの doc_hash と食い違う。
        // 台帳は追記専用なので、verifyは落とさず件数だけを見せる。
        let path = vault.root.join(format!("{id}.md"));
        let edited = std::fs::read_to_string(&path).unwrap() + "\n外部編集。\n";
        std::fs::write(&path, edited).unwrap();
        let after = verify(&vault).unwrap();
        assert_eq!(after.events, 1);
        assert_eq!(after.provenance_mismatches, 1);
    }
}
