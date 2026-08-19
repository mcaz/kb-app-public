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
use crate::vault::Vault;

pub const SCHEMA_V1: &str = "kb-app.repository-snapshot/v1";

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

    Ok(RepositorySnapshotV1 {
        schema: SCHEMA_V1.to_string(),
        workspace_id,
        notes,
        artifacts,
        artifact_refs,
        artifact_aliases,
        audit_log,
        legacy_files,
    })
}

pub fn export(vault: &Vault) -> Result<RepositoryExportV1> {
    let snapshot = snapshot(vault)?;
    let digest = digest(&snapshot)?;
    Ok(RepositoryExportV1 { digest, snapshot })
}

pub fn verify(vault: &Vault) -> Result<StorageReport> {
    let export = export(vault)?;
    Ok(StorageReport {
        schema: export.snapshot.schema.clone(),
        digest: export.digest,
        notes: export.snapshot.notes.len(),
        artifacts: export.snapshot.artifacts.len(),
        artifact_refs: export.snapshot.artifact_refs.len(),
        legacy_files: export.snapshot.legacy_files.len(),
    })
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

fn validate_note_authority(notes: &[SnapshotNote]) -> Result<()> {
    let mut by_uid = BTreeMap::new();
    let mut active_canonical = BTreeMap::new();
    for note in notes {
        let front = &note.frontmatter;
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
        let rel = entry
            .path()
            .strip_prefix(&vault.root)?
            .to_string_lossy()
            .replace('\\', "/");
        out.push(SnapshotFile {
            path: rel,
            size: bytes.len() as u64,
            sha256: hex_lower(&Sha256::digest(bytes)),
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
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
}
