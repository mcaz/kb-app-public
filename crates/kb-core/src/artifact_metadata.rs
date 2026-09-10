//! Artifactの台帳・参照・aliasの実bytesから、本文checkpointと独立した変更印を作る。
//! payloadやLegacy実体は読まない。完全な保存契約の検証は監査時のstorage_contractが担う。

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use crate::artifact::{ArtifactRef, Manifest};
use crate::vault::Vault;

// hookの軽量確認で壊れた台帳1件を無制限に読み込まない。payloadには適用しない。
const MAX_METADATA_BYTES: u64 = 32 * 1024 * 1024;
const STAMP_SCHEMA: &[u8] = b"kb-app.artifact-metadata/v1";

/// trackedメタデータだけの変更印。mtimeを戻した同サイズ編集も実bytesで検出する。
pub(crate) fn stamp(vault: &Vault) -> Result<String> {
    let root = vault.root.join(crate::ledger::DIR);
    let mut digest = Sha256::new();
    frame(&mut digest, STAMP_SCHEMA);
    if directory_exists(&root)? {
        directory::<Manifest>(&root.join("manifests"), "manifests", &mut digest)?;
        directory::<ArtifactRef>(&root.join("refs"), "refs", &mut digest)?;
        let aliases = read_regular(&root.join("aliases.json"))?;
        frame(&mut digest, b"aliases.json");
        match aliases {
            Some(bytes) => {
                serde_json::from_slice::<BTreeMap<String, String>>(&bytes)
                    .context("Artifact aliasのJSONが不正")?;
                frame(&mut digest, b"present");
                frame(&mut digest, &bytes);
            }
            None => frame(&mut digest, b"absent"),
        }
    } else {
        // 空directoryの作成自体をArtifact変更と数えず、aliasの欠落は明示する。
        frame(&mut digest, b"aliases.json");
        frame(&mut digest, b"absent");
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn frame(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

fn directory<T: DeserializeOwned>(path: &Path, name: &str, digest: &mut Sha256) -> Result<()> {
    if !directory_exists(path)? {
        return Ok(());
    }
    let mut paths = fs::read_dir(path)
        .context("Artifactメタデータdirectoryを読めない")?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort();
    for path in paths {
        ensure!(
            path.extension()
                .is_some_and(|extension| extension == "json"),
            "Artifactメタデータdirectoryに未知の項目がある"
        );
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("Artifactメタデータのファイル名が不正")?;
        let bytes = read_regular(&path)?.context("確認中にArtifactメタデータが削除された")?;
        serde_json::from_slice::<T>(&bytes).context("ArtifactメタデータのJSONが不正")?;
        frame(digest, format!("{name}/{file_name}").as_bytes());
        frame(digest, &bytes);
    }
    Ok(())
}

fn directory_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                metadata.file_type().is_dir(),
                "Artifactメタデータのdirectoryが不正"
            );
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("Artifactメタデータdirectoryを確認できない"),
    }
}

fn read_regular(path: &Path) -> Result<Option<Vec<u8>>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Artifactメタデータを確認できない"),
    };
    // FIFOやsymlinkはopen前に拒否する。payloadへのリンクも辿らない。
    ensure!(
        metadata.file_type().is_file(),
        "Artifactメタデータが通常ファイルではない"
    );
    ensure!(
        metadata.len() <= MAX_METADATA_BYTES,
        "Artifactメタデータが大きすぎる"
    );
    let file = fs::File::open(path).context("Artifactメタデータを読めない")?;
    ensure!(
        file.metadata()?.is_file(),
        "Artifactメタデータが通常ファイルではない"
    );
    let mut bytes = Vec::new();
    file.take(MAX_METADATA_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_METADATA_BYTES,
        "Artifactメタデータが大きすぎる"
    );
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{ArtifactId, ContentHash, Created, Locator, Policy, Role};

    fn manifest(unix_ms: u64) -> Manifest {
        Manifest::new(
            ArtifactId::new(unix_ms),
            ContentHash::of_bytes(b"fixture"),
            Created {
                media_type: "text/plain".into(),
                size: 7,
                at: "2026-09-08T00:00:00Z".into(),
                origin: "合成fixture".into(),
                by: "test/client".into(),
            },
            "fixture.txt".into(),
            Locator::LegacyGit {
                note_id: "notes/fixture".into(),
                file_name: "fixture.txt".into(),
            },
            Policy::migrated_legacy(),
            Role::File,
        )
    }

    /// 2026-09-08: 昇格・rollback相当のlocator変更とrefだけの更新を、列挙順によらず拾う。
    #[test]
    fn manifest_locator_and_ref_changes_are_observed_in_sorted_order() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        let root = vault.root.join(crate::ledger::DIR);
        fs::create_dir_all(root.join("manifests")).unwrap();
        fs::create_dir_all(root.join("refs")).unwrap();
        let manifests = [manifest(1), manifest(2)];
        let records = manifests
            .iter()
            .map(|manifest| {
                (
                    root.join("manifests").join(format!("{}.json", manifest.id)),
                    serde_json::to_vec(manifest).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        for (path, bytes) in records.iter().rev() {
            fs::write(path, bytes).unwrap();
        }
        let initial = stamp(&vault).unwrap();
        for (path, _) in &records {
            fs::remove_file(path).unwrap();
        }
        for (path, bytes) in &records {
            fs::write(path, bytes).unwrap();
        }
        assert_eq!(initial, stamp(&vault).unwrap());

        let mut promoted = manifests[0].clone();
        promoted.locator = Locator::Managed {
            hash: promoted.hash.clone(),
        };
        fs::write(&records[0].0, serde_json::to_vec(&promoted).unwrap()).unwrap();
        assert_ne!(initial, stamp(&vault).unwrap());
        fs::write(&records[0].0, &records[0].1).unwrap();
        assert_eq!(initial, stamp(&vault).unwrap());

        let mut reference = ArtifactRef::new(
            &crate::workspace::stored_workspace_id(&vault).unwrap(),
            "fixture".parse().unwrap(),
            manifests[0].id.clone(),
        );
        let ref_path = root.join("refs/fixture.json");
        fs::write(&ref_path, serde_json::to_vec(&reference).unwrap()).unwrap();
        let referenced = stamp(&vault).unwrap();
        assert_ne!(referenced, initial);
        reference.revision += 1;
        fs::write(&ref_path, serde_json::to_vec(&reference).unwrap()).unwrap();
        assert_ne!(referenced, stamp(&vault).unwrap());
    }

    #[test]
    fn absent_and_empty_metadata_are_stable_but_alias_presence_and_bytes_are_observed() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        let before = stamp(&vault).unwrap();
        let root = vault.root.join(crate::ledger::DIR);
        fs::create_dir_all(root.join("manifests")).unwrap();
        fs::create_dir_all(root.join("refs")).unwrap();
        assert_eq!(before, stamp(&vault).unwrap());
        fs::write(root.join("aliases.json"), "{}").unwrap();
        let present = stamp(&vault).unwrap();
        assert_ne!(before, present);
        fs::write(root.join("aliases.json"), "{\n}").unwrap();
        assert_ne!(present, stamp(&vault).unwrap());
        fs::remove_file(root.join("aliases.json")).unwrap();
        assert_eq!(before, stamp(&vault).unwrap());
    }

    /// 2026-09-08: 同サイズ・同mtimeでも本文変更を拾い、LFSと旧実体をhashし直さない。
    #[test]
    fn equal_size_and_restored_mtime_cannot_hide_alias_edits_and_payloads_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        let root = vault.root.join(crate::ledger::DIR);
        fs::create_dir_all(&root).unwrap();
        let aliases = root.join("aliases.json");
        fs::write(&aliases, r#"{"/old":"ref-a"}"#).unwrap();
        let before = stamp(&vault).unwrap();
        let modified = fs::metadata(&aliases).unwrap().modified().unwrap();
        fs::write(&aliases, r#"{"/old":"ref-b"}"#).unwrap();
        fs::File::options()
            .write(true)
            .open(&aliases)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let changed = stamp(&vault).unwrap();
        assert_ne!(before, changed);
        fs::create_dir_all(root.join("lfs")).unwrap();
        fs::create_dir_all(vault.root.join("notes/old.files")).unwrap();
        fs::write(root.join("lfs/payload"), "not JSON").unwrap();
        fs::write(vault.root.join("notes/old.files/payload"), "not JSON").unwrap();
        assert_eq!(changed, stamp(&vault).unwrap());
    }

    /// 2026-09-08: 破損や非通常ファイルを『Artifact変更なし』へ変換しない。
    #[test]
    fn malformed_and_non_regular_metadata_fail_safely() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        let root = vault.root.join(crate::ledger::DIR);
        fs::create_dir_all(root.join("manifests")).unwrap();
        let manifest = root.join("manifests/broken.json");
        fs::write(&manifest, "{}").unwrap();
        assert!(stamp(&vault).is_err());
        fs::remove_file(&manifest).unwrap();
        fs::create_dir(&manifest).unwrap();
        assert!(stamp(&vault).is_err());
        fs::remove_dir(&manifest).unwrap();
        fs::write(root.join("aliases.json"), "{broken").unwrap();
        assert!(stamp(&vault).is_err());
        fs::remove_file(root.join("aliases.json")).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/dev/null", &manifest).unwrap();
            assert!(stamp(&vault).is_err());
        }
    }

    /// 2026-09-08: FIFOを通常JSONとしてopenすると停止するため、開く前に型を拒否する。
    #[cfg(unix)]
    #[test]
    fn fifo_is_rejected_without_opening_it() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        let root = vault.root.join(crate::ledger::DIR);
        fs::create_dir_all(&root).unwrap();
        let result = std::process::Command::new("mkfifo")
            .arg(root.join("aliases.json"))
            .status()
            .unwrap();
        assert!(result.success());
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            send.send(stamp(&vault).is_err()).unwrap();
        });
        assert!(
            receive
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("FIFOをopenして停止してはならない")
        );
    }
}
