//! Artifact の purge — **「この端末と以降の同期から取り除く」**。
//!
//! ADR: `kb-app/artifact-deletion`(2026-09-04)。
//!
//! ## 「削除」と言わない範囲
//!
//! `full` の実体は LFS へコミット済みなので、**履歴からは消えない**。fresh clone
//! すれば取得できる。回収できるのはこの端末のディスクと、以降の commit に載る
//! pointer だけ。機密の誤アップロードには使えない(履歴の書き換えが要る別問題)。
//!
//! ## detach との違い
//!
//! [`crate::artifact::Manifest::detach`] は「そのノートから外す」だけで実体は残る。
//! purge は台帳ごと消す。どのノートからも外れた Artifact は [`orphans`] で拾える。
//!
//! ## 二段階にする理由
//!
//! ノート削除(`prepare_remove` / `commit_remove`)と同じ形にする。対象・理由・
//! 実体の状態を token へ固定し、間に変化があれば通さない。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use rand::RngCore as _;
use serde::{Deserialize, Serialize};

use crate::artifact::{ArtifactId, ContentHash, Manifest};
use crate::ledger::Ledger;
use crate::store::Stores;
use crate::vault::Vault;

/// 対象を固定しておける時間。ノート削除と揃える。
pub const PURGE_TOKEN_TTL: Duration = Duration::from_secs(5 * 60);

/// MCP の `attach` で入った実体は**原本が存在しない**(Base64 を会話から受け取る)。
/// UI 取り込み(`fs::copy`)は利用者のディスクに原本が残るので、扱いを分ける。
const MCP_ORIGIN_PREFIX: &str = "mcp-content:";

/// 実体が消えるとき、来歴によっては確認が要る。
fn is_only_copy(origin: &str) -> bool {
    origin.starts_with(MCP_ORIGIN_PREFIX)
}

/// purge の下見。**まだ何も消していない。**
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct PurgePlan {
    /// `commit` へ渡す短命 token。
    pub token: String,
    pub id: ArtifactId,
    pub display_name: String,
    pub hash: ContentHash,
    pub size: u64,
    pub origin: String,
    /// まだ結び付いているノート。空なら孤児。
    pub notes: Vec<String>,
    /// 同じ実体を指す他の台帳。1件でもあれば実体は残す。
    pub shares_object_with: Vec<ArtifactId>,
    /// この purge で実体も消えるか。
    pub drops_object: bool,
    /// 実体が消え、かつ原本が無い来歴 → `confirmed` 無しでは通さない。
    pub needs_confirmation: bool,
    /// この台帳を `supersedes` している版。purge すると参照が宙に浮く。
    pub superseded_by: Vec<ArtifactId>,
    pub reason: String,
}

/// purge の結果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Purged {
    pub id: ArtifactId,
    pub display_name: String,
    /// 実体も消したか(他が参照していれば false)。
    pub dropped_object: bool,
    /// 同期の失敗は purge 自体の失敗にしない(派生 — 契約4)。
    pub sync_error: Option<String>,
}

/// どのノートからも外れた台帳。**整理の下見**の実体。
pub fn orphans(ledger: &Ledger) -> Vec<Manifest> {
    let mut out: Vec<Manifest> = ledger
        .list()
        .into_iter()
        .filter(|m| m.notes.is_empty())
        .collect();
    // 古い順。溜まった順に片付けられるほうが素直
    out.sort_by(|a, b| a.created.at.cmp(&b.created.at).then(a.id.cmp(&b.id)));
    out
}

/// purge が触る置き場一式。[`crate::store::Stores`] と台帳と保管庫は保管庫 ID で
/// 揃って決まるので、別々に持ち回らない。
#[derive(Clone, Copy)]
pub struct Workspace<'a> {
    pub vault: &'a Vault,
    pub stores: &'a Stores,
    pub ledger: &'a Ledger,
}

/// 対象を固定した token の預かり所。呼び出し側(GUI / MCP)が1つ持つ。
#[derive(Debug, Default)]
pub struct PendingPurges {
    pending: HashMap<String, Pending>,
}

#[derive(Debug, Clone)]
struct Pending {
    id: ArtifactId,
    /// 下見のあと台帳が動いていないか。版と実体を見る
    version: u64,
    hash: ContentHash,
    needs_confirmation: bool,
    reason: String,
    expires_at: Instant,
}

impl PendingPurges {
    pub fn new() -> Self {
        Self::default()
    }

    /// 下見して token を発行する。**何も消さない。**
    pub fn prepare(&mut self, ledger: &Ledger, id: &ArtifactId, reason: &str) -> Result<PurgePlan> {
        let reason = reason.trim();
        if reason.is_empty() || reason.chars().count() > 500 || reason.contains(['\n', '\r']) {
            bail!("purge の理由は1〜500文字の一行で指定する");
        }
        let manifest = ledger
            .get(id)?
            .ok_or_else(|| anyhow::anyhow!("台帳に無い: {id}"))?;

        // 参照名が指しているものは消さない。dangling ref を作らない
        if let Some(r) = ledger.ref_for(id) {
            bail!(
                "参照名 {} が指しているので purge できない。先に参照を外す",
                r.name
            );
        }

        let others = ledger.list();
        let shares_object_with: Vec<ArtifactId> = others
            .iter()
            .filter(|m| m.id != *id && m.hash == manifest.hash)
            .map(|m| m.id.clone())
            .collect();
        let superseded_by: Vec<ArtifactId> = others
            .iter()
            .filter(|m| m.supersedes.as_ref() == Some(id))
            .map(|m| m.id.clone())
            .collect();

        let drops_object = shares_object_with.is_empty();
        let needs_confirmation = drops_object && is_only_copy(&manifest.created.origin);

        self.pending
            .retain(|_, pending| pending.expires_at > Instant::now());
        let token = loop {
            let mut bytes = [0_u8; 32];
            rand::rng().fill_bytes(&mut bytes);
            let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            if !self.pending.contains_key(&token) {
                break token;
            }
        };
        self.pending.insert(
            token.clone(),
            Pending {
                id: manifest.id.clone(),
                version: manifest.version,
                hash: manifest.hash.clone(),
                needs_confirmation,
                reason: reason.to_string(),
                expires_at: Instant::now() + PURGE_TOKEN_TTL,
            },
        );

        Ok(PurgePlan {
            token,
            id: manifest.id.clone(),
            display_name: manifest.display_name.clone(),
            hash: manifest.hash.clone(),
            size: manifest.created.size,
            origin: manifest.created.origin.clone(),
            notes: manifest.notes.clone(),
            shares_object_with,
            drops_object,
            needs_confirmation,
            superseded_by,
            reason: reason.to_string(),
        })
    }

    /// 下見どおりなら取り除く。`confirmed` は画面が本人へ訊いたときだけ true。
    pub fn commit(
        &mut self,
        ws: Workspace<'_>,
        id: &ArtifactId,
        token: &str,
        confirmed: bool,
        at: &str,
    ) -> Result<Purged> {
        let pending = self
            .pending
            .remove(token)
            .ok_or_else(|| anyhow::anyhow!("purge token が無効または使用済み。下見からやり直す"))?;
        if pending.expires_at <= Instant::now() {
            bail!("purge token の期限が切れた。下見からやり直す");
        }
        if pending.id != *id {
            bail!("purge 対象が下見時と一致しない。下見からやり直す");
        }
        if pending.needs_confirmation && !confirmed {
            bail!("この実体は原本が無く、purge すると復元できない。画面で確認を取ってからやり直す");
        }

        let manifest = ws
            .ledger
            .get(id)?
            .ok_or_else(|| anyhow::anyhow!("台帳に無い: {id}"))?;
        if manifest.version != pending.version || manifest.hash != pending.hash {
            bail!("purge 対象が下見のあとで変わった。内容を確認してからやり直す");
        }

        // 下見のあとに別の台帳が同じ実体を指し始めていないか、直前にもう一度見る
        let still_shared = ws
            .ledger
            .list()
            .into_iter()
            .any(|m| m.id != *id && m.hash == manifest.hash);

        let outcome = ws.ledger.purge(ws.vault, &manifest, &pending.reason, at)?;
        let mut dropped_object = false;
        if !still_shared {
            ws.stores.remove(manifest.policy.sync, &manifest.hash)?;
            if manifest.policy.sync == crate::artifact::SyncPolicy::Full {
                crate::lfs::forget(ws.vault, &manifest.hash)?;
            }
            dropped_object = true;
        }

        Ok(Purged {
            id: manifest.id,
            display_name: manifest.display_name,
            dropped_object,
            sync_error: outcome.sync_error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{Created, Locator, Policy, Role, SyncPolicy};
    use std::path::PathBuf;
    use std::str::FromStr;
    use tempfile::{TempDir, tempdir};

    struct Env {
        _dir: TempDir,
        vault: Vault,
        stores: Stores,
        ledger: Ledger,
    }

    fn env() -> Env {
        let dir = tempdir().unwrap();
        let root: PathBuf = dir.path().to_path_buf();
        let vault = Vault::create(root.join("v")).unwrap();
        let stores = Stores::at(root.join("artifacts"), "ws-a");
        let ledger = Ledger::at(vault.root.clone(), root.join("sidecar"));
        Env {
            _dir: dir,
            vault,
            stores,
            ledger,
        }
    }

    impl Env {
        fn ws(&self) -> Workspace<'_> {
            Workspace {
                vault: &self.vault,
                stores: &self.stores,
                ledger: &self.ledger,
            }
        }
    }

    /// sidecar 側(local_only)へ置く。LFS を通さないので Git 無しで実体まで検証できる。
    fn put(e: &Env, id: &str, bytes: &[u8], origin: &str, notes: &[&str]) -> Manifest {
        let hash = e
            .stores
            .import(SyncPolicy::LocalOnly, &mut &bytes[..])
            .unwrap()
            .hash;
        let mut policy = Policy::default_managed();
        policy.sync = SyncPolicy::LocalOnly;
        let mut m = Manifest::new(
            ArtifactId::from_str(id).unwrap(),
            hash.clone(),
            Created {
                media_type: "application/octet-stream".into(),
                size: bytes.len() as u64,
                at: "2026-09-04T00:00:00Z".into(),
                origin: origin.into(),
                by: "test".into(),
            },
            "f.bin".into(),
            Locator::Managed { hash },
            policy,
            Role::File,
        );
        m.notes = notes.iter().map(|n| n.to_string()).collect();
        e.ledger.put(&e.vault, &m).unwrap();
        m
    }

    const A: &str = "01M0EW60589ZHSZ0HJ6S1VWMYQ";
    const B: &str = "01M0EW60589ZHSZ0HJ6S1VWMYR";

    #[test]
    fn mcp_attached_content_is_treated_as_the_only_copy() {
        assert!(is_only_copy("mcp-content:claude-code/claude"));
        assert!(!is_only_copy("picker"));
        assert!(!is_only_copy("clipboard"));
        assert!(!is_only_copy("migration"));
    }

    #[test]
    fn orphans_are_the_manifests_no_note_points_at() {
        let e = env();
        put(&e, A, b"attached", "picker", &["notes/a"]);
        put(&e, B, b"detached", "picker", &[]);
        let found = orphans(&e.ledger);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id.as_str(), B);
    }

    #[test]
    fn reason_must_be_a_single_short_line() {
        let e = env();
        let m = put(&e, A, b"x", "picker", &[]);
        let mut p = PendingPurges::new();
        assert!(p.prepare(&e.ledger, &m.id, "  ").is_err());
        assert!(p.prepare(&e.ledger, &m.id, "壊れている\n二行目").is_err());
        assert!(p.prepare(&e.ledger, &m.id, &"あ".repeat(501)).is_err());
        assert!(
            p.prepare(&e.ledger, &m.id, "壊れているので取り除く")
                .is_ok()
        );
    }

    #[test]
    fn purging_removes_the_ledger_entry_and_the_object() {
        let e = env();
        let m = put(&e, A, b"broken", "picker", &[]);
        assert!(e.stores.has(SyncPolicy::LocalOnly, &m.hash));

        let mut p = PendingPurges::new();
        let plan = p.prepare(&e.ledger, &m.id, "壊れている").unwrap();
        assert!(plan.drops_object);
        assert!(!plan.needs_confirmation, "UI 取り込みは原本が残る");

        let out = p
            .commit(e.ws(), &m.id, &plan.token, false, "2026-09-04T01:00:00Z")
            .unwrap();
        assert!(out.dropped_object);
        assert!(e.ledger.get(&m.id).unwrap().is_none());
        assert!(!e.stores.has(SyncPolicy::LocalOnly, &m.hash));
    }

    /// 同じ内容の別 Artifact が居るなら、実体は消さない。
    #[test]
    fn a_shared_object_survives_the_purge_of_one_manifest() {
        let e = env();
        let a = put(&e, A, b"same bytes", "picker", &[]);
        let b = put(&e, B, b"same bytes", "picker", &["notes/keep"]);
        assert_eq!(a.hash, b.hash, "content-addressed なので同じ実体を指す");

        let mut p = PendingPurges::new();
        let plan = p.prepare(&e.ledger, &a.id, "重複").unwrap();
        assert!(!plan.drops_object);
        assert_eq!(plan.shares_object_with, vec![b.id.clone()]);

        let out = p
            .commit(e.ws(), &a.id, &plan.token, false, "2026-09-04T01:00:00Z")
            .unwrap();
        assert!(!out.dropped_object);
        assert!(e.ledger.get(&a.id).unwrap().is_none());
        assert!(
            e.stores.has(SyncPolicy::LocalOnly, &b.hash),
            "残った台帳から実体を読めなくなってはいけない"
        );
    }

    /// MCP 添付は原本が無い。実体が消える purge は確認を要求する。
    #[test]
    fn the_only_copy_needs_confirmation() {
        let e = env();
        let m = put(&e, A, b"only", "mcp-content:claude-code/claude", &[]);
        let mut p = PendingPurges::new();
        let plan = p.prepare(&e.ledger, &m.id, "壊れている").unwrap();
        assert!(plan.needs_confirmation);

        let refused = p.commit(e.ws(), &m.id, &plan.token, false, "2026-09-04T01:00:00Z");
        assert!(refused.is_err());
        assert!(
            e.ledger.get(&m.id).unwrap().is_some(),
            "拒否したのに台帳が消えていてはいけない"
        );

        // 拒否でも token は使い切る。下見からやり直させる
        let plan = p.prepare(&e.ledger, &m.id, "壊れている").unwrap();
        p.commit(e.ws(), &m.id, &plan.token, true, "2026-09-04T01:00:00Z")
            .unwrap();
        assert!(e.ledger.get(&m.id).unwrap().is_none());
    }

    #[test]
    fn a_token_is_single_use_and_bound_to_its_target() {
        let e = env();
        let a = put(&e, A, b"a", "picker", &[]);
        let b = put(&e, B, b"b", "picker", &[]);
        let mut p = PendingPurges::new();
        let plan = p.prepare(&e.ledger, &a.id, "理由").unwrap();

        // 別の対象へは使えない
        assert!(
            p.commit(e.ws(), &b.id, &plan.token, false, "2026-09-04T01:00:00Z")
                .is_err()
        );
        // 対象違いでも token は消費済み
        assert!(
            p.commit(e.ws(), &a.id, &plan.token, false, "2026-09-04T01:00:00Z")
                .is_err()
        );
        assert!(e.ledger.get(&a.id).unwrap().is_some());
    }

    /// 下見のあとに中身が差し替わったら通さない。
    #[test]
    fn a_changed_target_is_refused() {
        let e = env();
        let m = put(&e, A, b"before", "picker", &[]);
        let mut p = PendingPurges::new();
        let plan = p.prepare(&e.ledger, &m.id, "理由").unwrap();

        let mut moved = e.ledger.get(&m.id).unwrap().unwrap();
        moved.attach(moved.version, "notes/late").unwrap();
        e.ledger.put(&e.vault, &moved).unwrap();

        assert!(
            p.commit(e.ws(), &m.id, &plan.token, false, "2026-09-04T01:00:00Z")
                .is_err()
        );
        assert!(e.ledger.get(&m.id).unwrap().is_some());
    }

    /// 参照名が指しているものは dangling ref を作るので下見で止める。
    #[test]
    fn an_artifact_a_ref_points_at_cannot_be_purged() {
        let e = env();
        let m = put(&e, A, b"referenced", "picker", &[]);
        let name = crate::artifact::RefName::from_str("logo").unwrap();
        e.ledger
            .put_ref(
                &e.vault,
                SyncPolicy::LocalOnly,
                &crate::artifact::ArtifactRef::new("ws-a", name.clone(), m.id.clone()),
            )
            .unwrap();
        let mut p = PendingPurges::new();
        assert!(p.prepare(&e.ledger, &m.id, "理由").is_err());
    }

    /// 消したことは残す。
    #[test]
    fn a_tombstone_records_what_was_removed_and_why() {
        let e = env();
        let m = put(&e, A, b"gone", "picker", &[]);
        let mut p = PendingPurges::new();
        let plan = p
            .prepare(&e.ledger, &m.id, "壊れているので取り除く")
            .unwrap();
        p.commit(e.ws(), &m.id, &plan.token, false, "2026-09-04T01:00:00Z")
            .unwrap();
        let tomb = e.ledger.tombstones();
        assert_eq!(tomb.len(), 1);
        assert_eq!(tomb[0].id, m.id);
        assert_eq!(tomb[0].reason, "壊れているので取り除く");
        assert_eq!(tomb[0].at, "2026-09-04T01:00:00Z");
    }
}
