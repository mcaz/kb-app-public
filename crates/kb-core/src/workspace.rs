//! 保管庫(workspace)の永続 ID — ADR-0003 決定6 の前提。
//!
//! 参照名(`artifact_ref`)は「保管庫の中で一意」なので、その保管庫を**名前でもパスでもなく**
//! 指せる不変の ID が要る。名前は変えられるし、パスは端末ごとに違う。
//!
//! ## なぜレジストリではなく保管庫の中に置くか
//!
//! ADR-0003 の残課題は `registry.rs` の改定として書いたが、実装してみると**置き場所が違う**。
//! `registry.json` は設定ディレクトリにある**端末ローカル**の台帳で、同期されない。
//! そこに ID を置くと、同じ保管庫が端末ごとに別の ID を持ってしまい、
//! Git で同期されてきた台帳の参照先と噛み合わなくなる。
//! ID は保管庫と一緒に運ばれる必要があるので、**Git で追跡されるファイル**へ置く。
//!
//! `.kb/` は `.gitignore` 済み(索引 DB を置く場所)なので使えない。
//! 保管庫直下の `.kb-workspace` を追跡ファイルとして持つ。
//!
//! ## 同時に作られたときの直し方
//!
//! 2台が同時に初回起動すると、両方が別の ID を書いて競合する。
//! そこで**1行1 ID の素朴な形式**にし、`.gitattributes` で `merge=union` を張る。
//! 競合しても行が並ぶだけで壊れず、読むときに**古い方(= 文字列として小さい方)**へ
//! 寄せて書き戻す。index.md の「競合したら pull 後に再生成で自己修復」と同じ発想。

use std::fs;

use anyhow::{Context, Result};

use crate::artifact::{is_ulid, new_ulid};
use crate::vault::Vault;

/// 保管庫直下の ID ファイル。**追跡する**(`.kb/` は ignore 済みなので使わない)。
pub const ID_FILE: &str = ".kb-workspace";

/// 保管庫の永続 ID を返す。無ければ作って commit する(冪等)。
///
/// 競合して複数行になっていたら、古い方へ寄せて書き戻す。
pub fn workspace_id(vault: &Vault) -> Result<String> {
    let path = vault.root.join(ID_FILE);
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let mut ids: Vec<&str> = existing
        .lines()
        .map(str::trim)
        .filter(|line| is_ulid(line))
        .collect();
    ids.sort_unstable();
    ids.dedup();

    match ids.split_first() {
        // 通常。既に1つだけある
        Some((oldest, [])) => Ok((*oldest).to_string()),
        // 競合した跡。古い方へ寄せて書き戻す(新しい方を使うと、
        // 先に配られた台帳の参照先が全部ずれる)
        Some((oldest, _)) => {
            let oldest = (*oldest).to_string();
            write_and_commit(vault, &oldest, "vault: 保管庫 ID の重複を解消")?;
            Ok(oldest)
        }
        // 初回、または壊れていた
        None => {
            let id = new_ulid(now_ms());
            write_and_commit(vault, &id, "vault: 保管庫 ID を発行")?;
            Ok(id)
        }
    }
}

fn write_and_commit(vault: &Vault, id: &str, message: &str) -> Result<()> {
    fs::write(vault.root.join(ID_FILE), format!("{id}\n")).context("保管庫 ID の書き込み")?;
    // commit に失敗しても ID 自体は手元で有効。同期は派生なので落とさない(契約4)
    let _ = vault.commit(&[ID_FILE], message);
    Ok(())
}

fn now_ms() -> u64 {
    let nanos = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
    (nanos / 1_000_000).max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn vault() -> (tempfile::TempDir, Vault) {
        let dir = tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        (dir, vault)
    }

    #[test]
    fn issues_once_and_stays_the_same() {
        let (_dir, vault) = vault();
        let first = workspace_id(&vault).unwrap();
        assert!(is_ulid(&first), "{first}");
        assert_eq!(workspace_id(&vault).unwrap(), first);
    }

    #[test]
    fn id_file_is_tracked_not_ignored() {
        let (_dir, vault) = vault();
        workspace_id(&vault).unwrap();
        let ignore = fs::read_to_string(vault.root.join(".gitignore")).unwrap();
        assert!(
            !ignore.contains(ID_FILE),
            "ID は同期されないと意味がない: {ignore}"
        );
        assert!(vault.root.join(ID_FILE).exists());
    }

    #[test]
    fn duplicate_lines_collapse_to_the_older_one() {
        let (_dir, vault) = vault();
        // union merge の跡(2台が同時に初回起動した状態)を作る
        let older = "01AAAAAAAAAAAAAAAAAAAAAAAA";
        let newer = "01ZZZZZZZZZZZZZZZZZZZZZZZZ";
        fs::write(vault.root.join(ID_FILE), format!("{newer}\n{older}\n")).unwrap();

        assert_eq!(workspace_id(&vault).unwrap(), older);
        // 書き戻して1行になっている
        let after = fs::read_to_string(vault.root.join(ID_FILE)).unwrap();
        assert_eq!(after, format!("{older}\n"));
    }

    #[test]
    fn broken_content_is_replaced_not_propagated() {
        let (_dir, vault) = vault();
        fs::write(vault.root.join(ID_FILE), "not-a-ulid\n<<<<<<< HEAD\n").unwrap();
        let id = workspace_id(&vault).unwrap();
        assert!(is_ulid(&id), "{id}");
        assert_eq!(
            fs::read_to_string(vault.root.join(ID_FILE)).unwrap(),
            format!("{id}\n")
        );
    }

    #[test]
    fn two_vaults_get_different_ids() {
        let (_a, va) = vault();
        let (_b, vb) = vault();
        assert_ne!(workspace_id(&va).unwrap(), workspace_id(&vb).unwrap());
    }
}
