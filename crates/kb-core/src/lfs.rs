//! 「本体も同期」の実体を運ぶ層 — ADR-0003 決定2、PoC `poc/lfs-transport` の実測。
//!
//! ## なぜ自前の CAS を使わないのか
//!
//! LFS は「作業ツリーにファイルがある」ことを前提に動く。実体を運ばせるには
//! 保管庫の中にファイルが要るが、自前の CAS([`crate::store`])にも同じ実体を
//! 置くと**二重保存**になる。したがって `full` の実体は **LFS の置き場が持つ**
//! (自前 CAS が持つのは「情報のみ」と「同期しない」の2境界だけ)。
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
    std::process::Command::new("git")
        .args(args)
        .current_dir(&vault.root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes")
        .output()
        .context("git 実行")
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
    // 内容は不変。同じ hash が既にあるなら置き直さない
    if !has(vault, &hash) {
        fs::copy(src, &dest).with_context(|| format!("複製できない: {}", dest.display()))?;
        let rel = rel(&hash);
        // **ここは git CLI で通す。** `Vault::commit` は libgit2 で、libgit2 は
        // LFS のフィルタを走らせない。CLI で staged した pointer を、libgit2 の
        // add_path が実体で上書きしてしまう(実測で踏んだ)
        let add = git(vault, &["add", "--", &rel])?;
        if !add.status.success() {
            bail!("git add: {}", String::from_utf8_lossy(&add.stderr).trim());
        }
        commit_via_cli(vault, "vault: ファイルの実体を追加")?;
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
    // pointer のままなら何もしない(2度目の呼び出し)
    if fs::metadata(&path)?.len() < 1024 {
        return Ok(());
    }
    fs::remove_file(&path)?;
    let rel = rel(hash);
    let out = std::process::Command::new("git")
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

/// 実体を取り寄せる。
///
/// **`-X ""` が要る。** `.lfsconfig` の `fetchexclude = *` は `-I` だけでは
/// 上書きされず、指定しても黙って何も起きない(PoC で実測)。
/// この組み立てをコアに閉じ込めて、UI から `git lfs` を直接叩かせない。
pub fn fetch(vault: &Vault, hash: &ContentHash) -> Result<()> {
    let rel = rel(hash);
    let out = git(vault, &["lfs", "pull", "-I", &rel, "-X", ""])?;
    if !out.status.success() {
        bail!(
            "取り寄せに失敗: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    if !has(vault, hash) {
        bail!("取り寄せたが実体が見つからない");
    }
    Ok(())
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
        std::process::Command::new("git")
            .args(["lfs", "version"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
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
}
