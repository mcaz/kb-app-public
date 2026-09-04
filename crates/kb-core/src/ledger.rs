//! 台帳(manifest)と参照(ref)の置き場 — ADR-0003 決定2・決定6。
//!
//! 実体は保管庫の外([`crate::store`])に置くが、**台帳と参照は保管庫の Git に入る**。
//! 検索に載るのはここまでで、実体そのものは載らない。
//!
//! ## ただし「同期しない」ものは台帳ごと外へ出す
//!
//! 正本は client repo 由来について「manifest も client Git や personal vault Git に
//! 入れず、リポジトリ外のローカル sidecar に保存する」と決めている。
//! 台帳にはファイル名・リポジトリ名・パスが載るので、**名前だけでも機密になりうる**。
//!
//! したがって置き場は同期区分で分かれる:
//!
//! - `full` → 保管庫の `.kb-artifacts/`(Git で運ばれる)
//! - `local_only` → 保管庫の外の sidecar(Git に触れない)
//!
//! 区分を変えると台帳は**引っ越す**。緩めたときに sidecar 側へ残しておくと、
//! 同じ ID の台帳が二箇所にある状態になるため、書き込みのたびに反対側を掃除する。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::artifact::{ArtifactId, ArtifactRef, ContentHash, Manifest, RefName, SyncPolicy};
use crate::vault::Vault;

/// 保管庫の中の台帳置き場。`.kb/` は索引 DB 用に ignore 済みなので別名。
pub const DIR: &str = ".kb-artifacts";

/// 台帳と参照の読み書き。
#[derive(Debug, Clone)]
pub struct Ledger {
    vault_root: PathBuf,
    /// 保管庫の外(同期しないものだけがここへ来る)
    sidecar: PathBuf,
}

/// 論理データの書き込み後に行う Git commit の結果。
///
/// 同期は派生なので commit 失敗でローカルの書き込みを巻き戻さない。一方で、
/// `full` Artifact の取り込みは remote 到達を確認する必要があるため、呼び出し側が
/// 失敗を握り潰さず劣化として返せるようにする。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CommitOutcome {
    pub sync_error: Option<String>,
}

impl Ledger {
    /// 既定の置き場で開く。
    pub fn open(vault: &Vault, workspace_id: &str) -> Result<Self> {
        let sidecar = crate::app_data_dir()?
            .join("artifacts")
            .join(workspace_id)
            .join("local-only");
        Ok(Self {
            vault_root: vault.root.clone(),
            sidecar,
        })
    }

    /// 置き場を指定して開く(テストと、置き場を移したいとき)。
    pub fn at(vault_root: PathBuf, sidecar: PathBuf) -> Self {
        Self {
            vault_root,
            sidecar,
        }
    }

    fn base(&self, sync: SyncPolicy) -> PathBuf {
        match sync {
            SyncPolicy::LocalOnly => self.sidecar.clone(),
            SyncPolicy::Full => self.vault_root.join(DIR),
        }
    }

    fn manifest_path(&self, sync: SyncPolicy, id: &ArtifactId) -> PathBuf {
        self.base(sync).join("manifests").join(format!("{id}.json"))
    }

    fn ref_path(&self, sync: SyncPolicy, name: &RefName) -> PathBuf {
        self.base(sync).join("refs").join(format!("{name}.json"))
    }

    /// 保管庫からの相対パス(commit に渡す用)。
    fn rel(path: &Path, root: &Path) -> Option<String> {
        path.strip_prefix(root)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
    }

    /// 台帳を書く。区分が変わっていたら反対側から消す(二重に残さない)。
    pub fn put(&self, vault: &Vault, manifest: &Manifest) -> Result<()> {
        self.put_with_outcome(vault, manifest).map(|_| ())
    }

    /// 台帳を書き、同期対象なら commit の成否も返す。
    pub fn put_with_outcome(&self, vault: &Vault, manifest: &Manifest) -> Result<CommitOutcome> {
        let sync = manifest.policy.sync;
        let dest = self.manifest_path(sync, &manifest.id);
        write_json(&dest, manifest)?;

        // 反対側に同じ ID があれば引っ越し。緩めた/締めた両方向で起きる
        let other = match sync {
            SyncPolicy::LocalOnly => SyncPolicy::Full,
            _ => SyncPolicy::LocalOnly,
        };
        let stale = self.manifest_path(other, &manifest.id);
        if stale.is_file() {
            fs::remove_file(&stale)?;
        }

        Ok(self.commit_if_tracked(vault, &[dest, stale], "vault: ファイルの台帳を更新"))
    }

    /// 台帳を消し、**消した記録を残す**。実体には触れない(呼ぶ側が [`crate::purge`])。
    ///
    /// 記録が残らない削除を作らない、という決定(ADR kb-app/artifact-deletion)の実体。
    /// `full` なら墓標も commit されるので履歴から辿れる。sidecar は Git に載らないため
    /// 墓標ファイルだけが記録になる。
    pub fn purge(
        &self,
        vault: &Vault,
        manifest: &Manifest,
        reason: &str,
        at: &str,
    ) -> Result<CommitOutcome> {
        let sync = manifest.policy.sync;
        let tomb = self.tombstone_path(sync, &manifest.id);
        write_json(
            &tomb,
            &Tombstone {
                id: manifest.id.clone(),
                display_name: manifest.display_name.clone(),
                hash: manifest.hash.clone(),
                size: manifest.created.size,
                origin: manifest.created.origin.clone(),
                notes: manifest.notes.clone(),
                reason: reason.to_string(),
                at: at.to_string(),
            },
        )?;
        // 両側を見る。区分を跨いで引っ越した直後でも取り残さない
        let mut removed = Vec::new();
        for s in [SyncPolicy::Full, SyncPolicy::LocalOnly] {
            let path = self.manifest_path(s, &manifest.id);
            if path.is_file() {
                fs::remove_file(&path)?;
                removed.push(path);
            }
        }
        removed.push(tomb);
        Ok(self.commit_if_tracked(vault, &removed, "vault: ファイルを取り除く"))
    }

    /// 消した記録の一覧(新しい順)。整理の結果を後から辿るため。
    pub fn tombstones(&self) -> Vec<Tombstone> {
        let mut out: Vec<Tombstone> = self.read_all("purged");
        out.sort_by(|a, b| b.at.cmp(&a.at).then(b.id.cmp(&a.id)));
        out
    }

    fn tombstone_path(&self, sync: SyncPolicy, id: &ArtifactId) -> PathBuf {
        self.base(sync).join("purged").join(format!("{id}.json"))
    }

    /// 台帳を読む。同期される側 → sidecar の順に探す。
    pub fn get(&self, id: &ArtifactId) -> Result<Option<Manifest>> {
        for sync in [SyncPolicy::Full, SyncPolicy::LocalOnly] {
            let path = self.manifest_path(sync, id);
            if path.is_file() {
                let text = fs::read_to_string(&path)
                    .with_context(|| format!("台帳が読めない: {}", path.display()))?;
                return Ok(Some(serde_json::from_str(&text).with_context(|| {
                    format!("台帳の形式が壊れている: {}", path.display())
                })?));
            }
        }
        Ok(None)
    }

    /// 台帳の一覧(同期される側と sidecar の両方)。
    pub fn list(&self) -> Vec<Manifest> {
        let mut out = self.read_all("manifests");
        out.sort_by(|a: &Manifest, b: &Manifest| a.id.cmp(&b.id));
        out
    }

    /// 両側の JSON を読む。**壊れた1件で一覧全体を落とさない**
    /// (劣化は呼び出し側が出す — 契約4)。
    fn read_all<T: serde::de::DeserializeOwned>(&self, sub: &str) -> Vec<T> {
        let mut out = Vec::new();
        for sync in [SyncPolicy::Full, SyncPolicy::LocalOnly] {
            let Ok(entries) = fs::read_dir(self.base(sync).join(sub)) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(text) = fs::read_to_string(entry.path()) else {
                    continue;
                };
                if let Ok(value) = serde_json::from_str::<T>(&text) {
                    out.push(value);
                }
            }
        }
        out
    }

    /// そのノートにひもづく台帳。**ファイルはノートの持ち物**という見え方の実体。
    ///
    /// 差し替えられた版は返さない。内容の更新は新しい台帳になり前の版も残る
    /// (決定7)ので、「そのノートのファイル」が版の数だけ増えてしまう。
    /// 履歴は `supersedes` を辿れば読める。
    pub fn list_for_note(&self, note_id: &str) -> Vec<Manifest> {
        let all = self.list();
        let superseded: std::collections::HashSet<ArtifactId> =
            all.iter().filter_map(|m| m.supersedes.clone()).collect();
        all.into_iter()
            .filter(|m| m.notes.iter().any(|n| n == note_id) && !superseded.contains(&m.id))
            .collect()
    }

    /// 各ノートにひもづく現行ファイルの件数を、一度の台帳走査で返す。
    /// 一覧画面から `list_for_note` を件数分呼んで台帳を読み直さないための集計口。
    pub fn current_counts_by_note(&self) -> std::collections::HashMap<String, usize> {
        let all = self.list();
        let superseded: std::collections::HashSet<ArtifactId> =
            all.iter().filter_map(|m| m.supersedes.clone()).collect();
        let mut counts = std::collections::HashMap::new();
        for manifest in all
            .into_iter()
            .filter(|manifest| !superseded.contains(&manifest.id))
        {
            let mut seen = std::collections::HashSet::new();
            for note_id in manifest.notes {
                if seen.insert(note_id.clone()) {
                    *counts.entry(note_id).or_default() += 1;
                }
            }
        }
        counts
    }

    /// 参照を書く。置き場は指す先の区分に従う(台帳と同じ理由)。
    pub fn put_ref(&self, vault: &Vault, sync: SyncPolicy, r: &ArtifactRef) -> Result<()> {
        self.put_ref_with_outcome(vault, sync, r).map(|_| ())
    }

    /// 参照を書き、同期対象なら commit の成否も返す。
    pub fn put_ref_with_outcome(
        &self,
        vault: &Vault,
        sync: SyncPolicy,
        r: &ArtifactRef,
    ) -> Result<CommitOutcome> {
        let dest = self.ref_path(sync, &r.name);
        write_json(&dest, r)?;
        let other = match sync {
            SyncPolicy::LocalOnly => SyncPolicy::Full,
            _ => SyncPolicy::LocalOnly,
        };
        let stale = self.ref_path(other, &r.name);
        if stale.is_file() {
            fs::remove_file(&stale)?;
        }
        Ok(self.commit_if_tracked(vault, &[dest, stale], "vault: ファイルの参照を更新"))
    }

    /// 参照を引く。
    pub fn get_ref(&self, name: &RefName) -> Result<Option<ArtifactRef>> {
        for sync in [SyncPolicy::Full, SyncPolicy::LocalOnly] {
            let path = self.ref_path(sync, name);
            if path.is_file() {
                let text = fs::read_to_string(&path)?;
                return Ok(Some(serde_json::from_str(&text).with_context(|| {
                    format!("参照の形式が壊れている: {}", path.display())
                })?));
            }
        }
        Ok(None)
    }

    /// 旧 `/…files/…` リンク → 参照名 の対応表。
    ///
    /// 本文は**自動書換えしない**(ユーザーの文章なので触らない)。
    /// 代わりに旧パスから参照名へ辿れるようにして、レンダラは両方をコアへ渡す。
    fn alias_path(&self) -> PathBuf {
        self.vault_root.join(DIR).join("aliases.json")
    }

    pub fn aliases(&self) -> std::collections::BTreeMap<String, String> {
        fs::read_to_string(self.alias_path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// 旧パスに参照名を結びつける(移行のときだけ増える)。
    pub fn put_alias(&self, vault: &Vault, legacy_path: &str, name: &RefName) -> Result<()> {
        let mut map = self.aliases();
        map.insert(legacy_path.to_string(), name.to_string());
        write_json(&self.alias_path(), &map)?;
        self.commit_if_tracked(vault, &[self.alias_path()], "vault: 旧リンクの対応表を更新");
        Ok(())
    }

    pub fn alias(&self, legacy_path: &str) -> Option<RefName> {
        use std::str::FromStr;
        self.aliases()
            .get(legacy_path)
            .and_then(|n| RefName::from_str(n).ok())
    }

    /// その台帳を指している参照(あれば)。
    ///
    /// 新しい版を作ったときに参照を付け替えるために要る。付け替えないと、
    /// 本文リンク(`kb-artifact-ref:`)が古い版を指したままになり、
    /// 「本文リンクは最新版に追従する」(ADR-0003 決定5)が破れる。
    pub fn ref_for(&self, id: &ArtifactId) -> Option<ArtifactRef> {
        for sync in [SyncPolicy::Full, SyncPolicy::LocalOnly] {
            let dir = self.base(sync).join("refs");
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(text) = fs::read_to_string(entry.path()) else {
                    continue;
                };
                // 壊れた1件で逆引き全体を落とさない(list と同じ扱い)
                if let Ok(r) = serde_json::from_str::<ArtifactRef>(&text)
                    && r.artifact_id == *id
                {
                    return Some(r);
                }
            }
        }
        None
    }

    /// その名前が既に使われているか(衝突時に別名を提案するため)。
    pub fn ref_taken(&self, name: &RefName) -> bool {
        [SyncPolicy::Full, SyncPolicy::LocalOnly]
            .iter()
            .any(|s| self.ref_path(*s, name).is_file())
    }

    /// 保管庫の中にあるものだけ commit する。sidecar は Git に触れない。
    /// **同期は派生**なので、失敗しても書き込み自体は成功のまま(契約4)。
    fn commit_if_tracked(&self, vault: &Vault, paths: &[PathBuf], message: &str) -> CommitOutcome {
        let rels: Vec<String> = paths
            .iter()
            .filter_map(|p| Self::rel(p, &self.vault_root))
            .collect();
        if rels.is_empty() {
            return CommitOutcome::default();
        }
        let refs: Vec<&str> = rels.iter().map(String::as_str).collect();
        CommitOutcome {
            sync_error: vault.commit(&refs, message).err().map(|e| e.to_string()),
        }
    }
}

/// purge の墓標。**台帳を消しても「何を、なぜ消したか」は残す。**
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Tombstone {
    pub id: ArtifactId,
    pub display_name: String,
    pub hash: ContentHash,
    pub size: u64,
    pub origin: String,
    /// 消した時点で結び付いていたノート(通常は空 — 孤児を消すため)
    pub notes: Vec<String>,
    pub reason: String,
    /// RFC3339
    pub at: String,
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(value)?;
    fs::write(path, text).with_context(|| format!("書き込めない: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{ContentHash, Created, Locator, Policy, Role, Sensitivity};
    use std::str::FromStr;
    use tempfile::{TempDir, tempdir};

    fn setup() -> (TempDir, Vault, Ledger) {
        let dir = tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let ledger = Ledger::at(vault.root.clone(), dir.path().join("sidecar"));
        (dir, vault, ledger)
    }

    fn manifest(sync: SyncPolicy, client_repo: bool) -> Manifest {
        let hash = ContentHash::of_bytes(b"bytes");
        Manifest::new(
            ArtifactId::new(1_755_000_000_000),
            hash.clone(),
            Created {
                media_type: "image/png".into(),
                size: 5,
                at: "2026-08-12T09:04:00Z".into(),
                origin: "conversation".into(),
                by: crate::OWNER_ACTOR.into(),
            },
            "検討スケッチ.png".into(),
            Locator::Managed { hash },
            Policy {
                sensitivity: Sensitivity::Private,
                sync,
                client_repo,
            },
            Role::File,
        )
    }

    #[test]
    fn synced_manifest_lands_in_the_vault_and_is_committed() {
        let (_d, vault, ledger) = setup();
        let m = manifest(SyncPolicy::Full, false);
        ledger.put(&vault, &m).unwrap();

        let path = vault
            .root
            .join(DIR)
            .join("manifests")
            .join(format!("{}.json", m.id));
        assert!(path.is_file(), "台帳が保管庫に無い");
        // 追跡されている(= 同期で運ばれる)
        let repo = git2::Repository::open(&vault.root).unwrap();
        let rel = format!("{DIR}/manifests/{}.json", m.id);
        assert!(
            repo.index().unwrap().get_path(Path::new(&rel), 0).is_some(),
            "台帳が Git に入っていない"
        );
    }

    /// 2026-08-16 まで Artifact の commit 失敗は捨てられ、取り込み側が
    /// remote 到達を確認できなかった。ローカルの台帳は残しつつ劣化を返す。
    #[test]
    fn tracked_write_reports_commit_failure_without_losing_the_manifest() {
        let (_d, vault, ledger) = setup();
        let m = manifest(SyncPolicy::Full, false);
        fs::write(vault.root.join(".git/index.lock"), b"locked").unwrap();

        let outcome = ledger.put_with_outcome(&vault, &m).unwrap();

        assert!(outcome.sync_error.is_some(), "commit 失敗が見えない");
        assert!(
            ledger.get(&m.id).unwrap().is_some(),
            "同期失敗でローカルの台帳まで失ってはいけない"
        );
    }

    #[test]
    fn ledger_dir_is_not_ignored() {
        let (_d, vault, _l) = setup();
        let ignore = fs::read_to_string(vault.root.join(".gitignore")).unwrap();
        assert!(
            !ignore.contains(DIR),
            "台帳は同期されないと意味がない: {ignore}"
        );
    }

    #[test]
    fn local_only_manifest_never_touches_the_vault() {
        let (_d, vault, ledger) = setup();
        let m = manifest(SyncPolicy::LocalOnly, true);
        ledger.put(&vault, &m).unwrap();

        // 名前・パスだけでも機密になりうるので、保管庫側には現れない
        assert!(!vault.root.join(DIR).join("manifests").exists());
        assert_eq!(ledger.get(&m.id).unwrap().as_ref(), Some(&m));
    }

    #[test]
    fn narrowing_moves_the_manifest_out_of_the_vault() {
        let (_d, vault, ledger) = setup();
        let mut m = manifest(SyncPolicy::Full, false);
        ledger.put(&vault, &m).unwrap();
        let in_vault = vault
            .root
            .join(DIR)
            .join("manifests")
            .join(format!("{}.json", m.id));
        assert!(in_vault.is_file());

        // 「同期しない」へ締める → 台帳ごと外へ出る
        m.policy.sync = SyncPolicy::LocalOnly;
        ledger.put(&vault, &m).unwrap();
        assert!(!in_vault.exists(), "保管庫側に台帳が残っている");
        assert_eq!(ledger.get(&m.id).unwrap().as_ref(), Some(&m));

        // 削除も commit に乗っている(次の pull で他端末からも消える)
        let repo = git2::Repository::open(&vault.root).unwrap();
        let rel = format!("{DIR}/manifests/{}.json", m.id);
        assert!(
            repo.index().unwrap().get_path(Path::new(&rel), 0).is_none(),
            "削除が index に反映されていない"
        );
    }

    #[test]
    fn widening_moves_the_manifest_into_the_vault() {
        let (_d, vault, ledger) = setup();
        let mut m = manifest(SyncPolicy::LocalOnly, false);
        ledger.put(&vault, &m).unwrap();

        m.policy.sync = SyncPolicy::Full;
        ledger.put(&vault, &m).unwrap();

        let in_vault = vault
            .root
            .join(DIR)
            .join("manifests")
            .join(format!("{}.json", m.id));
        assert!(in_vault.is_file());
        // 二重に残らない
        let in_sidecar = ledger
            .sidecar
            .join("manifests")
            .join(format!("{}.json", m.id));
        assert!(!in_sidecar.exists(), "sidecar に台帳が残っている");
    }

    #[test]
    fn list_covers_both_sides_and_survives_a_broken_file() {
        let (_d, vault, ledger) = setup();
        let synced = manifest(SyncPolicy::Full, false);
        let mut local = manifest(SyncPolicy::LocalOnly, true);
        local.id = ArtifactId::new(1_755_000_100_000);
        ledger.put(&vault, &synced).unwrap();
        ledger.put(&vault, &local).unwrap();

        // 壊れた1件は飛ばす(一覧全体を落とさない)
        write_json(
            &vault.root.join(DIR).join("manifests").join("broken.json"),
            &"not a manifest",
        )
        .unwrap();

        let ids: Vec<_> = ledger.list().into_iter().map(|m| m.id).collect();
        assert!(ids.contains(&synced.id));
        assert!(ids.contains(&local.id));
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn refs_follow_the_same_boundary_rule() {
        let (_d, vault, ledger) = setup();
        let name = RefName::from_str("sketch").unwrap();
        let r = ArtifactRef::new("ws-a", name.clone(), ArtifactId::new(1_755_000_000_000));

        ledger.put_ref(&vault, SyncPolicy::LocalOnly, &r).unwrap();
        assert!(!vault.root.join(DIR).join("refs").exists());
        assert!(ledger.ref_taken(&name));
        assert_eq!(ledger.get_ref(&name).unwrap().as_ref(), Some(&r));

        // 緩めると保管庫側へ移り、sidecar からは消える
        ledger.put_ref(&vault, SyncPolicy::Full, &r).unwrap();
        assert!(
            vault
                .root
                .join(DIR)
                .join("refs")
                .join("sketch.json")
                .is_file()
        );
        assert!(!ledger.sidecar.join("refs").join("sketch.json").exists());
    }

    /// 版を重ねても前の版は残る(内容は不変)ので、素直に一覧すると同じファイルが
    /// 版の数だけ並ぶ。ノートの持ち物として見えるのは最新版だけ。
    #[test]
    fn a_note_shows_only_the_current_version() {
        let (_d, vault, ledger) = setup();
        let mut old = manifest(SyncPolicy::Full, false);
        old.notes.push("notes/decision".into());
        let new = {
            let mut m = old.succeed(
                ArtifactId::new(1_755_000_001_000),
                ContentHash::of_bytes(b"v2"),
                old.created.clone(),
            );
            m.notes = old.notes.clone();
            m
        };
        ledger.put(&vault, &old).unwrap();
        ledger.put(&vault, &new).unwrap();

        let listed = ledger.list_for_note("notes/decision");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, new.id);
        // 前の版は消えていない(履歴は supersedes で辿れる)
        assert!(ledger.get(&old.id).unwrap().is_some());
        assert_eq!(ledger.list().len(), 2);
        assert_eq!(
            ledger.current_counts_by_note().get("notes/decision"),
            Some(&1)
        );
    }

    #[test]
    fn a_reference_can_be_found_from_the_artifact_it_points_at() {
        let (_d, vault, ledger) = setup();
        let name = RefName::from_str("sketch").unwrap();
        let id = ArtifactId::new(1_755_000_000_000);
        let r = ArtifactRef::new("ws-a", name, id.clone());
        ledger.put_ref(&vault, SyncPolicy::Full, &r).unwrap();

        assert_eq!(ledger.ref_for(&id).as_ref(), Some(&r));
        assert_eq!(ledger.ref_for(&ArtifactId::new(1_755_000_009_000)), None);
    }

    #[test]
    fn unknown_id_and_name_are_none_not_errors() {
        let (_d, _v, ledger) = setup();
        let id = ArtifactId::new(1_755_000_000_000);
        assert_eq!(ledger.get(&id).unwrap(), None);
        let name = RefName::from_str("nope").unwrap();
        assert_eq!(ledger.get_ref(&name).unwrap(), None);
        assert!(!ledger.ref_taken(&name));
    }
}
