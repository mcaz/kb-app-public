//! ファイルの検索 — ADR-0003・正本の受入条件。
//!
//! **検索対象は台帳まで。実体(blob)の中身は既定で索引に入れない。**
//! ここで扱うのは表示名・役割・出どころといった台帳の情報だけで、
//! 中身を読む処理はこの module に存在しない。
//!
//! 2つの規律を索引の側で守る:
//!
//! - **会話の原本は既定で出さない。** 台帳の `retrievable` が false のものは
//!   索引に入れない(検索の後で弾くのではなく、そもそも入れない)
//! - **機械が作った索引は原本より下。** 順位付けで必ず後ろに回る
//!
//! 既存のノート索引には触らない。テーブルを足すだけで、埋め込みの再計算を
//! 起こさない(`index.rs` の「破壊的な作り直しをしない」と同じ理由)。

use anyhow::Result;
use rusqlite::Connection;

use crate::artifact::Role;
use crate::ledger::Ledger;
use crate::tokenize::{match_expr, wakati};

/// 検索結果1件。**中身は持たない**(台帳の情報だけ)。
#[derive(Debug, Clone, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ArtifactHit {
    pub id: String,
    pub display_name: String,
    /// `file` | `transcript` | `dataset` | `derived-index`
    pub role: String,
    /// 機械が作った索引。UI は「原本との一致を確認できません」を添える
    pub machine_made: bool,
}

/// 索引テーブルを用意する(既存のノート索引には触らない)。
pub fn init(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS artifacts(
            id TEXT PRIMARY KEY, display_name TEXT, role TEXT, machine_made INTEGER
         );
         CREATE VIRTUAL TABLE IF NOT EXISTS fts_artifacts
            USING fts5(id UNINDEXED, text, tokenize='unicode61');",
    )?;
    Ok(())
}

/// 台帳から索引を作り直す。件数は少ないので全消し・全入れでよい。
pub fn sync(conn: &Connection, ledger: &Ledger) -> Result<usize> {
    init(conn)?;
    conn.execute_batch("DELETE FROM artifacts; DELETE FROM fts_artifacts;")?;
    let mut n = 0;
    for m in ledger.list() {
        // **ここで弾く。** 検索の後で除外すると、経路が増えたときに漏れる
        if !m.retrievable {
            continue;
        }
        let role = role_key(m.role);
        let machine_made = m.role == Role::DerivedIndex;
        conn.execute(
            "INSERT INTO artifacts(id, display_name, role, machine_made) VALUES(?1,?2,?3,?4)",
            rusqlite::params![m.id.as_str(), m.display_name, role, machine_made as i64],
        )?;
        // 索引に入れるのは台帳の情報だけ。実体は読まない
        let text = format!("{} {}", wakati(&m.display_name), wakati(&m.created.origin));
        conn.execute(
            "INSERT INTO fts_artifacts(id, text) VALUES(?1,?2)",
            rusqlite::params![m.id.as_str(), text],
        )?;
        n += 1;
    }
    Ok(n)
}

fn role_key(role: Role) -> &'static str {
    match role {
        Role::File => "file",
        Role::Transcript => "transcript",
        Role::Dataset => "dataset",
        Role::DerivedIndex => "derived-index",
    }
}

/// 引く。**機械が作った索引は必ず後ろ**に回す。
pub fn search(conn: &Connection, query: &str, limit: usize) -> Result<Vec<ArtifactHit>> {
    init(conn)?;
    let expr = match_expr(query);
    if expr.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT a.id, a.display_name, a.role, a.machine_made
           FROM fts_artifacts f JOIN artifacts a ON a.id = f.id
          WHERE f.text MATCH ?1
          ORDER BY a.machine_made ASC, bm25(fts_artifacts) ASC
          LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![expr, limit as i64], |r| {
        Ok(ArtifactHit {
            id: r.get(0)?,
            display_name: r.get(1)?,
            role: r.get(2)?,
            machine_made: r.get::<_, i64>(3)? != 0,
        })
    })?;
    Ok(rows.filter_map(std::result::Result::ok).collect())
}

/// 索引から外れている件数(「会話の原本 N 件は検索から外しています」用ではなく、
/// **ホームの内訳**に出す。検索結果の中で件数を見せると、そこから中身へ辿る
/// 迂回路になりうるため — レビューで削った)。
pub fn excluded_count(ledger: &Ledger) -> usize {
    ledger.list().iter().filter(|m| !m.retrievable).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{
        ArtifactId, ContentHash, Created, Locator, Manifest, Policy, Role, Sensitivity, SyncPolicy,
    };
    use crate::vault::Vault;
    use tempfile::{TempDir, tempdir};

    struct Env {
        _dir: TempDir,
        vault: Vault,
        ledger: Ledger,
        conn: Connection,
    }

    fn env() -> Env {
        let dir = tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let ledger = Ledger::at(vault.root.clone(), dir.path().join("sidecar"));
        let conn = Connection::open_in_memory().unwrap();
        init(&conn).unwrap();
        Env {
            _dir: dir,
            vault,
            ledger,
            conn,
        }
    }

    fn put(e: &Env, name: &str, role: Role, ms: u64) -> Manifest {
        let hash = ContentHash::of_bytes(name.as_bytes());
        let m = Manifest::new(
            ArtifactId::new(ms),
            hash.clone(),
            Created {
                media_type: "application/octet-stream".into(),
                size: 1,
                at: "2026-08-12T09:04:00Z".into(),
                origin: "conversation".into(),
                by: crate::OWNER_ACTOR.into(),
            },
            name.into(),
            Locator::Managed { hash },
            Policy {
                sensitivity: Sensitivity::Private,
                sync: SyncPolicy::LocalOnly,
                client_repo: false,
            },
            role,
        );
        e.ledger.put(&e.vault, &m).unwrap();
        m
    }

    #[test]
    fn finds_by_display_name() {
        let e = env();
        put(&e, "実測ログ.parquet", Role::File, 1_755_000_000_000);
        sync(&e.conn, &e.ledger).unwrap();
        let hits = search(&e.conn, "実測ログ", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].display_name, "実測ログ.parquet");
        assert!(!hits[0].machine_made);
    }

    #[test]
    fn a_transcript_never_enters_the_index() {
        let e = env();
        put(&e, "設計会話.md", Role::Transcript, 1_755_000_000_000);
        let n = sync(&e.conn, &e.ledger).unwrap();
        assert_eq!(n, 0, "索引に入れてはいけない");
        assert!(search(&e.conn, "設計会話", 10).unwrap().is_empty());
        // 件数は別経路で数える(検索結果の中には出さない)
        assert_eq!(excluded_count(&e.ledger), 1);
    }

    #[test]
    fn a_transcript_can_be_opted_in_through_the_ledger() {
        let e = env();
        let mut m = put(&e, "設計会話.md", Role::Transcript, 1_755_000_000_000);
        assert!(!m.retrievable);
        // 方針そのものを変えたときだけ入る(検索側の一時的な迂回路は作らない)
        m.retrievable = true;
        e.ledger.put(&e.vault, &m).unwrap();
        sync(&e.conn, &e.ledger).unwrap();
        assert_eq!(search(&e.conn, "設計会話", 10).unwrap().len(), 1);
    }

    #[test]
    fn machine_made_ranks_below_the_original() {
        let e = env();
        put(&e, "スケッチ 図面", Role::File, 1_755_000_000_000);
        put(
            &e,
            "スケッチ 図面 の読み取り",
            Role::DerivedIndex,
            1_755_000_100_000,
        );
        sync(&e.conn, &e.ledger).unwrap();

        let hits = search(&e.conn, "スケッチ", 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(!hits[0].machine_made, "原本が先に来ること");
        assert!(hits[1].machine_made, "機械が作った索引は後ろ");
    }

    #[test]
    fn the_contents_of_a_file_are_not_searchable() {
        let e = env();
        // 表示名には出てこない語を「中身」に見立てる。台帳しか索引しないので当たらない
        let mut m = put(&e, "秘密.txt", Role::File, 1_755_000_000_000);
        m.display_name = "秘密.txt".into();
        e.ledger.put(&e.vault, &m).unwrap();
        sync(&e.conn, &e.ledger).unwrap();
        assert!(
            search(&e.conn, "パスワード", 10).unwrap().is_empty(),
            "実体の中身が索引に入っている"
        );
    }

    #[test]
    fn sync_reflects_removal() {
        let e = env();
        let m = put(&e, "消える.bin", Role::File, 1_755_000_000_000);
        sync(&e.conn, &e.ledger).unwrap();
        assert_eq!(search(&e.conn, "消える", 10).unwrap().len(), 1);

        // 台帳から外れたら索引からも消える(作り直しなので取りこぼさない)
        let path = e
            ._dir
            .path()
            .join("sidecar")
            .join("manifests")
            .join(format!("{}.json", m.id));
        std::fs::remove_file(path).unwrap();
        sync(&e.conn, &e.ledger).unwrap();
        assert!(search(&e.conn, "消える", 10).unwrap().is_empty());
    }
}
