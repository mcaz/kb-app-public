//! 旧添付(`<ノート ID>.files/`)を Artifact の台帳へ載せる — ADR-0003 決定4・実装順8。
//!
//! **実体を動かさない。** 旧ファイルは保管庫の中にそのまま残り、locator は
//! `LegacyGit` を指す。増えるのは台帳・参照・旧リンクの対応表だけなので、
//! 受入条件「**移行前に取得できた添付を、移行によって取得不能にしない**」は
//! 実体に触れないことで満たす — 移行しても Git の中の同じファイルを読み続ける。
//!
//! ## 実体を LFS へ移す段はここに無い
//!
//! 決定4 の二段目(LFS へ上げて locator を切り替える)は入れていない。旧ファイルは
//! fallback として残り続ける(MVP に削除が無い)ので**急ぐ理由が無く**、実行するには
//! 次の2つが要る:
//!
//! - locator を差し替える手段。いま「直せる field」を表す [`crate::artifact::Change`] に
//!   locator は無い(内容の同一性を保ったまま置き場だけ変える操作が、まだモデルに無い)
//! - upload の成功確認。決定4 は「**上げ切ってから**切り替える」と決めている
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

use anyhow::{Context, Result};

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

/// 棚卸し。**何も書かない。**
pub fn survey(vault: &Vault, ledger: &Ledger) -> Vec<Pending> {
    let done: HashSet<(String, String)> = ledger
        .list()
        .iter()
        .filter_map(|m| match &m.locator {
            Locator::LegacyGit { note_id, file_name } => Some((note_id.clone(), file_name.clone())),
            _ => None,
        })
        .collect();

    let mut out = Vec::new();
    for (note_id, _) in vault.list_note_files() {
        for (file_name, size) in vault.list_attachments(&note_id) {
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
    out
}

/// 台帳・参照・対応表を重ねる。**冪等** — 既に載っているものは飛ばす。
pub fn migrate(
    vault: &Vault,
    ledger: &Ledger,
    workspace_id: &str,
    at: &str,
) -> Result<Vec<Migrated>> {
    let mut out = Vec::new();
    for pending in survey(vault, ledger) {
        let path = vault.attach_dir(&pending.note_id).join(&pending.file_name);
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
            .propose(title, "本文。", None, &["test".into()], "test/client")
            .unwrap();
        let dir = vault.attach_dir(&id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(file_name), bytes).unwrap();
        id
    }

    const AT: &str = "2026-08-13T09:00:00Z";

    #[test]
    fn survey_reads_without_writing() {
        let e = env();
        let id = legacy(&e.vault, "設計メモ", "図.png", b"png bytes");

        let found = survey(&e.vault, &e.ledger);
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
        let path = e.vault.attach_dir(&id).join("図.png");

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
        assert!(survey(&e.vault, &e.ledger).is_empty());
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
}
