//! 「本体も同期」の実体を運ぶ層 — ADR-0003 決定2、PoC `poc/lfs-transport` の実測。
//!
//! ## なぜ自前の CAS を使わないのか
//!
//! LFS は「作業ツリーにファイルがある」ことを前提に動く。実体を運ばせるには
//! 保管庫の中にファイルが要るが、自前の CAS([`crate::store`])にも同じ実体を
//! 置くと**二重保存**になる。したがって `full` の実体は **LFS の置き場が持つ**
//! (自前 CAS が持つのは「同期しない」境界だけ)。
//!
//! ## 作業ツリーに実体を残さない
//!
//! 素直に置くと、作った端末の保管庫フォルダに実体が残る(Git の履歴には
//! pointer しか入らないが、フォルダは太る)。そこで取り込みの最後に、
//! 作業ツリーのファイルを**pointer へ戻す**:
//!
//! 1. `.kb-artifacts/lfs/<hash>` へ実体を書いて `git add`(clean フィルタが
//!    実体を保管庫の外の置き場へ移す)
//! 2. commit
//! 3. 作業ツリーのファイルを消して、smudge を止めて checkout し直す
//!
//! 実体は手順1の時点で外の置き場に入っているので、3で消えるのは複製だけ。
//! 実測では 900,000 バイト → 131 バイト(pointer)になり、`git lfs checkout` で
//! 元の内容に戻せることも確認している。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::artifact::{ContentHash, Hasher};
use crate::backup::{BackupFailureKind, failure, git_failure};
use crate::ledger;
use crate::vault::Vault;

/// 保管庫の中で LFS が追跡する場所。`.gitattributes` の filter 指定と対。
fn tracked_path(vault: &Vault, hash: &ContentHash) -> PathBuf {
    vault.root.join(ledger::DIR).join("lfs").join(hash.as_str())
}

/// 保管庫からの相対パス(git コマンドへ渡す用)。
fn rel(hash: &ContentHash) -> String {
    format!("{}/lfs/{}", ledger::DIR, hash.as_str())
}

fn git(vault: &Vault, args: &[&str]) -> Result<std::process::Output> {
    let mut command = crate::external_tools::git_command()?;
    command
        .args(args)
        .current_dir(&vault.root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes")
        .env("GCM_INTERACTIVE", "Never");
    let accesses_remote = args.starts_with(&["lfs", "push"])
        || args.starts_with(&["lfs", "pull"])
        || args.starts_with(&["lfs", "fetch"]);
    if accesses_remote {
        let repo = git2::Repository::open(&vault.root)?;
        if let Ok(remote) = repo.find_remote("origin")
            && let Some(url) = remote.url()
        {
            crate::github_auth::configure_git_auth(&mut command, url)?;
        }
    }
    command.output().context("git 実行")
}

/// LFS の置き場にある実体のパス。LFS の配置は `objects/aa/bb/<oid>`。
fn object_path(vault: &Vault, hash: &ContentHash) -> Result<PathBuf> {
    let repo = git2::Repository::open(&vault.root)?;
    let storage = repo
        .config()?
        .get_string("lfs.storage")
        .context("lfs.storage が未設定(connect::ensure_lfs_config を先に呼ぶ)")?;
    let oid = hash.as_str();
    Ok(Path::new(&storage)
        .join("objects")
        .join(&oid[0..2])
        .join(&oid[2..4])
        .join(oid))
}

/// この端末に実体があるか。**pointer があるだけでは「ある」と言わない。**
pub fn has(vault: &Vault, hash: &ContentHash) -> bool {
    object_path(vault, hash)
        .map(|p| p.is_file())
        .unwrap_or(false)
}

/// 取り込む。実体は保管庫の外の置き場へ入り、作業ツリーには pointer が残る。
pub fn import(vault: &Vault, src: &Path) -> Result<(ContentHash, u64)> {
    // 属性が張られていないまま add すると、実体がそのまま Git に入る。
    // 順序に依存する事故なので、呼ぶ側の作法に任せずここで担保する
    crate::connect::ensure_vault_config(vault)?;
    let (hash, size) = hash_file(src)?;
    let dest = tracked_path(vault, &hash);
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir)?;
    }
    // object が既にあっても、このhashをfresh cloneへ知らせるtracked pointerが無ければ作る。
    // objectの存在だけで省くと、別Artifactからdedupeされた実体をremoteから取得できない。
    if !dest.is_file() {
        fs::copy(src, &dest).with_context(|| format!("複製できない: {}", dest.display()))?;
        let rel = rel(&hash);
        // **ここは git CLI で通す。** `Vault::commit` は libgit2 で、libgit2 は
        // LFS のフィルタを走らせない。CLI で staged した pointer を、libgit2 の
        // add_path が実体で上書きしてしまう(実測で踏んだ)
        let add = git(vault, &["add", "--", &rel])?;
        if !add.status.success() {
            bail!("git add: {}", String::from_utf8_lossy(&add.stderr).trim());
        }
        let staged = git(vault, &["diff", "--cached", "--quiet", "--", &rel])?;
        match staged.status.code() {
            Some(0) => {} // 同じpointerが既に履歴にある。作業ツリーの再構成だけでよい
            Some(1) => commit_via_cli(vault, "vault: ファイルの実体を追加")?,
            _ => bail!(
                "LFS pointerの差分を確認できない: {}",
                String::from_utf8_lossy(&staged.stderr).trim()
            ),
        }
        // Git に入ったのが pointer であることを確かめる。属性が効いていないと
        // 実体がそのまま履歴に入り、あとから剥がすのは破壊的な作業になる
        let stored = git(vault, &["cat-file", "-p", &format!(":{rel}")])?;
        let head = String::from_utf8_lossy(&stored.stdout);
        if !head.starts_with("version https://git-lfs") {
            bail!("LFS の属性が効いていない(実体が Git に入りかけた)");
        }
    }
    shrink_to_pointer(vault, &hash)?;
    Ok((hash, size))
}

/// commit を git CLI で行う。識別子が未設定の環境では既定値で通す
/// (`Vault::commit` の Signature フォールバックと同じ考え方)。
fn commit_via_cli(vault: &Vault, message: &str) -> Result<()> {
    let out = git(vault, &["commit", "-q", "-m", message])?;
    if out.status.success() {
        return Ok(());
    }
    let retry = git(
        vault,
        &[
            "-c",
            "user.name=kb-app",
            "-c",
            "user.email=kb-app@localhost",
            "commit",
            "-q",
            "-m",
            message,
        ],
    )?;
    if !retry.status.success() {
        bail!(
            "git commit: {}",
            String::from_utf8_lossy(&retry.stderr).trim()
        );
    }
    Ok(())
}

/// 作業ツリーの複製を pointer へ戻す。実体は既に外の置き場にある。
fn shrink_to_pointer(vault: &Vault, hash: &ContentHash) -> Result<()> {
    let path = tracked_path(vault, hash);
    if !path.is_file() {
        return Ok(());
    }
    // pointer のままなら何もしない(2度目の呼び出し)。sizeだけで判定すると、
    // 1KiB未満のraw添付をpointerと誤認してVault作業ツリーへ残してしまう。
    let mut prefix = [0u8; 64];
    let read = std::io::Read::read(&mut fs::File::open(&path)?, &mut prefix)?;
    if prefix[..read].starts_with(b"version https://git-lfs") {
        return Ok(());
    }
    fs::remove_file(&path)?;
    let rel = rel(hash);
    let out = crate::external_tools::git_command()?
        .args(["checkout", "--", &rel])
        .current_dir(&vault.root)
        .env("GIT_LFS_SKIP_SMUDGE", "1") // 実体を書き戻させない
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("git checkout")?;
    if !out.status.success() {
        bail!(
            "pointer へ戻せない: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// LFS object 1件を origin へ送る。
///
/// Git ref より先に実体の到達を確認するため、通常の `git push` の pre-push hookへ
/// 任せず object ID を明示する。成功後にだけ manifest を含む ref を push すれば、
/// 別端末から見える台帳が「remote に実体が無い」状態を作らない。
pub fn push_object(vault: &Vault, hash: &ContentHash) -> Result<()> {
    let out = git(
        vault,
        &["lfs", "push", "--object-id", "origin", hash.as_str()],
    )?;
    if !out.status.success() {
        return Err(git_failure(
            "LFS upload に失敗",
            &String::from_utf8_lossy(&out.stderr),
            BackupFailureKind::LfsUpload,
        ));
    }
    Ok(())
}

/// 実体を取り寄せる。
///
/// **`-X ""` が要る。** `.lfsconfig` の `fetchexclude = *` は `-I` だけでは
/// 上書きされず、指定しても黙って何も起きない(PoC で実測)。
/// この組み立てをコアに閉じ込めて、UI から `git lfs` を直接叩かせない。
pub fn fetch(vault: &Vault, hash: &ContentHash) -> Result<()> {
    let rel = rel(hash);
    let out = git(vault, &["lfs", "pull", "-I", &rel, "-X", ""])?;
    if !out.status.success() {
        return Err(git_failure(
            "取り寄せに失敗",
            &String::from_utf8_lossy(&out.stderr),
            BackupFailureKind::RemoteObjectMissing,
        ));
    }
    if !has(vault, hash) {
        return Err(failure(
            BackupFailureKind::RemoteObjectMissing,
            format!("取り寄せたが実体が見つからない: {hash}"),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreReport {
    pub total: usize,
    pub fetched: usize,
    pub reused: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectRestoreProgress {
    pub completed: usize,
    pub total: usize,
    pub fetched: usize,
    pub reused: usize,
}

/// fresh clone にある全 `full` Artifact を明示的に復元して hash 照合する。
///
/// `.lfsconfig` は通常の clone で実体を取らないため、この一括処理を通ったときだけ
/// 「別端末で復元済み」と言える。先に Storage Contract を検査し、壊れた台帳を一覧処理が
/// 黙って飛ばす余地を作らない。同じ content hash は1回だけ取得する。
pub fn restore_all(vault: &Vault) -> Result<RestoreReport> {
    restore_all_with_progress(vault, |_| {})
}

/// 取得済みobjectはworkspace ID単位のLFS storageで再利用する。途中失敗で一時cloneが
/// 消えても実体は残るため、同じVaultを再度復元すると未完了分から再開できる。
pub fn restore_all_with_progress(
    vault: &Vault,
    mut progress: impl FnMut(ObjectRestoreProgress),
) -> Result<RestoreReport> {
    if let Err(error) = crate::storage_contract::verify(vault) {
        return Err(failure(
            BackupFailureKind::InvalidVault,
            format!("復元元が Storage Contract を満たさない: {error}"),
        ));
    }
    crate::connect::ensure_vault_config(vault)?;
    let workspace_id = crate::workspace::stored_workspace_id(vault).map_err(|error| {
        failure(
            BackupFailureKind::InvalidVault,
            format!("復元元の workspace ID を確認できない: {error}"),
        )
    })?;
    let ledger = crate::ledger::Ledger::open(vault, &workspace_id).map_err(|error| {
        failure(
            BackupFailureKind::InvalidVault,
            format!("復元元の Artifact 台帳を確認できない: {error}"),
        )
    })?;
    let hashes: std::collections::BTreeSet<ContentHash> = ledger
        .list()
        .into_iter()
        .filter(|manifest| manifest.policy.sync == crate::artifact::SyncPolicy::Full)
        .filter_map(|manifest| match manifest.locator {
            crate::artifact::Locator::Managed { hash } => Some(hash),
            _ => None,
        })
        .collect();
    let mut fetched = 0;
    let mut reused = 0;
    progress(ObjectRestoreProgress {
        completed: 0,
        total: hashes.len(),
        fetched,
        reused,
    });
    for (index, hash) in hashes.iter().enumerate() {
        let existing = verify(vault, hash).map_err(|error| {
            failure(
                BackupFailureKind::IntegrityMismatch,
                format!("取得済み実体を検証できない({hash}): {error}"),
            )
        })?;
        if existing == crate::store::Verified::Ok {
            reused += 1;
        } else {
            // 中断された転送が不完全なobjectを残しても、再実行時にそれを完成品として
            // 扱わない。hash不一致のcacheだけを捨て、この1件を取り直す。
            if existing == crate::store::Verified::Mismatch {
                discard_object(vault, hash)?;
            }
            fetch(vault, hash)?;
            fetched += 1;
        }
        let restored = verify(vault, hash).map_err(|error| {
            failure(
                BackupFailureKind::IntegrityMismatch,
                format!("復元した実体を検証できない({hash}): {error}"),
            )
        })?;
        match restored {
            crate::store::Verified::Ok => {}
            crate::store::Verified::Missing => {
                return Err(failure(
                    BackupFailureKind::RemoteObjectMissing,
                    format!("復元後も実体がない: {hash}"),
                ));
            }
            crate::store::Verified::Mismatch => {
                return Err(failure(
                    BackupFailureKind::IntegrityMismatch,
                    format!("復元した実体の hash が一致しない: {hash}"),
                ));
            }
        }
        progress(ObjectRestoreProgress {
            completed: index + 1,
            total: hashes.len(),
            fetched,
            reused,
        });
    }
    Ok(RestoreReport {
        total: hashes.len(),
        fetched,
        reused,
    })
}

fn discard_object(vault: &Vault, hash: &ContentHash) -> Result<()> {
    let path = object_path(vault, hash)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(failure(
            BackupFailureKind::IntegrityMismatch,
            format!("壊れた実体を再取得のため破棄できない: {error}"),
        )),
    }
}

/// 作業ツリーの pointer と、この端末の LFS 置き場からこの実体を取り除く。
///
/// **履歴からは消えない。** object は既にコミット済みで、fresh clone すれば取得
/// できる。回収できるのはこの端末のディスクと、以降の commit に載る pointer だけ。
/// 呼ぶ側は「完全に削除」と表示してはいけない(ADR kb-app/artifact-deletion)。
pub fn forget(vault: &Vault, hash: &ContentHash) -> Result<()> {
    let rel = rel(hash);
    if tracked_path(vault, hash).is_file() {
        let out = git(vault, &["rm", "--quiet", "--force", "--", &rel])?;
        if !out.status.success() {
            // index から既に外れている場合もある。作業ツリーだけ落として先へ進む
            let _ = fs::remove_file(tracked_path(vault, hash));
        }
        commit_via_cli(vault, "vault: ファイルの実体を取り除く")?;
    }
    discard_object(vault, hash)
}

/// 実体を読む。無ければ `None`(呼び出し側が「この端末にない」として扱う)。
pub fn read(vault: &Vault, hash: &ContentHash) -> Result<Option<fs::File>> {
    let path = object_path(vault, hash)?;
    match fs::File::open(&path) {
        Ok(f) => Ok(Some(f)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("実体が読めない"),
    }
}

/// 置いてある実体を読み直して照合する。
pub fn verify(vault: &Vault, hash: &ContentHash) -> Result<crate::store::Verified> {
    use std::io::Read;
    let Some(mut f) = read(vault, hash)? else {
        return Ok(crate::store::Verified::Missing);
    };
    let mut hasher = Hasher::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(if &hasher.finish() == hash {
        crate::store::Verified::Ok
    } else {
        crate::store::Verified::Mismatch
    })
}

fn hash_file(path: &Path) -> Result<(ContentHash, u64)> {
    use std::io::Read;
    let mut f =
        fs::File::open(path).with_context(|| format!("読み込めない: {}", path.display()))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connect::ensure_vault_config;
    use crate::store::Verified;
    use tempfile::tempdir;

    fn lfs_ready() -> bool {
        crate::external_tools::git_lfs_available()
    }

    #[test]
    fn import_keeps_the_vault_folder_small_and_the_bytes_outside() {
        if !lfs_ready() {
            eprintln!("git-lfs が無いので飛ばす");
            return;
        }
        let dir = tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        ensure_vault_config(&vault).unwrap();

        let src = dir.path().join("big.bin");
        let data: Vec<u8> = (0..900_000).map(|i| (i % 251) as u8).collect();
        fs::write(&src, &data).unwrap();

        let (hash, size) = import(&vault, &src).unwrap();
        assert_eq!(size, 900_000);
        assert_eq!(hash, ContentHash::of_bytes(&data));

        // 作業ツリーに残るのは pointer だけ
        let tracked = tracked_path(&vault, &hash);
        let left = fs::metadata(&tracked).unwrap().len();
        assert!(left < 1024, "作業ツリーに実体が残っている: {left} バイト");

        // 実体は保管庫の外にあり、照合が通る
        assert!(has(&vault, &hash));
        let obj = object_path(&vault, &hash).unwrap();
        assert!(
            !obj.starts_with(&vault.root),
            "実体が保管庫の中にある: {}",
            obj.display()
        );
        assert_eq!(verify(&vault, &hash).unwrap(), Verified::Ok);
    }

    #[test]
    fn importing_the_same_bytes_twice_does_not_duplicate() {
        if !lfs_ready() {
            return;
        }
        let dir = tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        ensure_vault_config(&vault).unwrap();
        let src = dir.path().join("a.bin");
        fs::write(&src, vec![9u8; 300_000]).unwrap();

        let (h1, _) = import(&vault, &src).unwrap();
        let (h2, _) = import(&vault, &src).unwrap();
        assert_eq!(h1, h2);
        let tracked = tracked_path(&vault, &h1);
        assert!(fs::metadata(&tracked).unwrap().len() < 1024);
    }

    #[test]
    fn small_and_preexisting_objects_still_leave_a_tracked_pointer() {
        if !lfs_ready() {
            return;
        }
        let dir = tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        ensure_vault_config(&vault).unwrap();
        let src = dir.path().join("small.bin");
        fs::write(&src, b"small legacy bytes").unwrap();

        let (hash, _) = import(&vault, &src).unwrap();
        let tracked = tracked_path(&vault, &hash);
        let pointer = fs::read(&tracked).unwrap();
        assert!(pointer.starts_with(b"version https://git-lfs"));
        assert_ne!(pointer, b"small legacy bytes");

        // local objectだけが残りtracked pathが失われた状態でもpointerを再構成する。
        fs::remove_file(&tracked).unwrap();
        assert!(has(&vault, &hash));
        import(&vault, &src).unwrap();
        assert!(
            fs::read(&tracked)
                .unwrap()
                .starts_with(b"version https://git-lfs")
        );
    }

    #[test]
    fn missing_object_reads_as_none_and_verifies_as_missing() {
        if !lfs_ready() {
            return;
        }
        let dir = tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        ensure_vault_config(&vault).unwrap();
        let absent = ContentHash::of_bytes(b"never imported");
        assert!(!has(&vault, &absent));
        assert!(read(&vault, &absent).unwrap().is_none());
        assert_eq!(verify(&vault, &absent).unwrap(), Verified::Missing);
    }

    #[test]
    fn fresh_clone_restores_every_full_object_and_verifies_hashes() {
        if !lfs_ready() {
            eprintln!("git-lfs が無いので飛ばす");
            return;
        }
        let dir = tempdir().unwrap();
        let bare = dir.path().join("backup.git");
        let run = |cwd: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_LFS_SKIP_SMUDGE", "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(dir.path(), &["init", "--bare", bare.to_str().unwrap()]);

        let source = Vault::create(dir.path().join("source")).unwrap();
        ensure_vault_config(&source).unwrap();
        crate::connect::set_backup_remote(&source, bare.to_str().unwrap()).unwrap();
        let workspace_id = crate::workspace::stored_workspace_id(&source).unwrap();
        let stores = crate::store::Stores::at(dir.path().join("stores"), &workspace_id);
        let ledger =
            crate::ledger::Ledger::at(source.root.clone(), dir.path().join("source-local-only"));
        let input = dir.path().join("full.bin");
        let bytes: Vec<u8> = (0..350_000).map(|i| (i % 239) as u8).collect();
        fs::write(&input, &bytes).unwrap();
        let taken = crate::intake::take(
            &source,
            &stores,
            &ledger,
            &workspace_id,
            &input,
            crate::intake::Request {
                note_id: None,
                display_name: "full.bin".into(),
                media_type: "application/octet-stream".into(),
                role: crate::artifact::Role::File,
                policy: None,
                ref_name: None,
                supersedes: None,
                origin: "test".into(),
                by: "test/agent".into(),
                at: "2026-08-16T00:00:00Z".into(),
            },
        )
        .unwrap();
        assert_eq!(taken.delivery, crate::intake::DeliveryStatus::Confirmed);
        // 同一プロセスのテストは workspace ID ごとの LFS storage を共有する。
        // 送信後に手元の object を消して、別端末の fresh clone を再現する。
        fs::remove_file(object_path(&source, &taken.manifest.hash).unwrap()).unwrap();
        let branch = git2::Repository::open(&source.root)
            .unwrap()
            .head()
            .unwrap()
            .shorthand()
            .unwrap()
            .to_string();
        run(
            &bare,
            &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")],
        );

        run(dir.path(), &["clone", bare.to_str().unwrap(), "restored"]);
        let restored = Vault::open(dir.path().join("restored")).unwrap();
        ensure_vault_config(&restored).unwrap();
        assert!(!has(&restored, &taken.manifest.hash));
        let report = restore_all(&restored).unwrap();
        assert_eq!(report.total, 1);
        assert_eq!(report.fetched, 1);
        assert_eq!(report.reused, 0);
        assert_eq!(
            verify(&restored, &taken.manifest.hash).unwrap(),
            Verified::Ok
        );

        // 再試行は一時cloneのpathではなくworkspace ID単位のstoreを引き継ぐ。
        // 別のfresh cloneでも検証済みobjectを再取得しない。
        run(
            dir.path(),
            &["clone", bare.to_str().unwrap(), "restored-again"],
        );
        let restored_again = Vault::open(dir.path().join("restored-again")).unwrap();
        ensure_vault_config(&restored_again).unwrap();
        let mut progress = Vec::new();
        let resumed =
            restore_all_with_progress(&restored_again, |state| progress.push(state)).unwrap();
        assert_eq!(resumed.total, 1);
        assert_eq!(resumed.fetched, 0);
        assert_eq!(resumed.reused, 1);
        assert_eq!(progress.first().unwrap().completed, 0);
        assert_eq!(progress.last().unwrap().completed, 1);

        // 中断等でcacheが壊れてもhashだけで再利用せず、そのobjectだけ取り直す。
        let cached = object_path(&restored_again, &taken.manifest.hash).unwrap();
        // file:// LFS remote はlocal objectをhard-linkすることがある。unlinkしてから
        // 別inodeの壊れたcacheを置き、remote側の正本まで書き換えない。
        fs::remove_file(&cached).unwrap();
        fs::write(&cached, b"incomplete transfer").unwrap();
        let repaired = restore_all(&restored_again).unwrap();
        assert_eq!(repaired.fetched, 1);
        assert_eq!(repaired.reused, 0);
        assert_eq!(
            verify(&restored_again, &taken.manifest.hash).unwrap(),
            Verified::Ok
        );
    }

    /// 配布受入。親processでシステムの git-lfs を PATH から外し、同梱候補だけを
    /// `KB_GIT_LFS_BIN` で渡した子processが実際の pointer 化まで完走することを確かめる。
    /// CI だけが明示実行し、通常の unit test ではダウンロード済みsidecarを要求しない。
    #[cfg(unix)]
    #[test]
    #[ignore = "配布用git-lfs sidecarを準備したCIで実行する"]
    fn bundled_git_lfs_works_without_an_ambient_installation() {
        const CHILD_MARKER: &str = "KB_GIT_LFS_ACCEPTANCE_CHILD";
        if std::env::var_os(CHILD_MARKER).is_none() {
            let bundled = std::env::var_os("KB_GIT_LFS_BIN")
                .map(PathBuf::from)
                .expect("KB_GIT_LFS_BINで配布候補を指定する");
            assert!(bundled.is_absolute(), "配布候補は絶対pathで指定する");
            assert!(bundled.is_file(), "配布候補がない: {}", bundled.display());

            let git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|directory| directory.join("git"))
                .find(|candidate| candidate.is_file())
                .expect("gitがPATHにある");
            let isolated = tempdir().unwrap();
            let isolated_git = isolated.path().join("git");
            std::os::unix::fs::symlink(git, &isolated_git).unwrap();
            let isolated_path = std::env::join_paths([isolated.path()]).unwrap();

            let ambient = std::process::Command::new(&isolated_git)
                .args(["lfs", "version"])
                .env("PATH", &isolated_path)
                .output()
                .unwrap();
            assert!(!ambient.status.success(), "隔離PATHにgit-lfsが混入している");

            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "lfs::tests::bundled_git_lfs_works_without_an_ambient_installation",
                    "--ignored",
                    "--nocapture",
                ])
                .env("PATH", isolated_path)
                .env(CHILD_MARKER, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "同梱git-lfs受入に失敗\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let expected = std::fs::canonicalize(std::env::var_os("KB_GIT_LFS_BIN").unwrap()).unwrap();
        let actual = std::fs::canonicalize(crate::external_tools::git_lfs_binary()).unwrap();
        assert_eq!(actual, expected, "同梱候補以外のgit-lfsを解決した");
        assert!(lfs_ready());

        let dir = tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        ensure_vault_config(&vault).unwrap();
        let src = dir.path().join("bundled.bin");
        let bytes: Vec<u8> = (0..900_000).map(|index| (index % 251) as u8).collect();
        fs::write(&src, &bytes).unwrap();
        let (hash, size) = import(&vault, &src).unwrap();

        assert_eq!(size, bytes.len() as u64);
        assert_eq!(verify(&vault, &hash).unwrap(), Verified::Ok);
        assert!(
            fs::metadata(tracked_path(&vault, &hash)).unwrap().len() < 1024,
            "同梱版でpointer化されていない"
        );
    }
}
