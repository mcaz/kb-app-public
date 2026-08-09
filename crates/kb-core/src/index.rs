//! 派生索引(SQLite 1ファイル)。二本立て FTS(lindera 主索引+trigram レスキュー)+
//! リンクテーブル。mtime ベースの増分 sync で「書いてすぐ引ける」を保証する。
//! 接続規律(busy_timeout 必須・WAL)は docs/poc-report.md の PoC ③ 由来。

use std::fs;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::frontmatter::Note;
use crate::tokenize::wakati;
use crate::vault::Vault;

const SCHEMA_VERSION: &str = "1";

pub fn open_db(vault: &Vault) -> Result<Connection> {
    let path = vault.index_db_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(&path).context("index.db open")?;
    conn.busy_timeout(Duration::from_secs(5))?; // 全接続で必須(PoC ③)
    let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    debug_assert_eq!(mode.to_lowercase(), "wal");
    init_schema(&conn)?;
    Ok(conn)
}

fn init_schema(conn: &Connection) -> Result<()> {
    let ver: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key='schema'", [], |r| r.get(0))
        .ok();
    if ver.as_deref() == Some(SCHEMA_VERSION) {
        return Ok(());
    }
    conn.execute_batch(&format!(
        "
        DROP TABLE IF EXISTS notes; DROP TABLE IF EXISTS links;
        DROP TABLE IF EXISTS fts_main; DROP TABLE IF EXISTS fts_tri;
        CREATE TABLE meta_new(key TEXT PRIMARY KEY, value TEXT);
        DROP TABLE IF EXISTS meta;
        ALTER TABLE meta_new RENAME TO meta;
        INSERT INTO meta(key, value) VALUES('schema', '{SCHEMA_VERSION}');
        CREATE TABLE notes(
            id TEXT PRIMARY KEY, title TEXT, description TEXT, status TEXT,
            origin TEXT, generated_by TEXT, generated_at TEXT,
            mtime INTEGER, body TEXT
        );
        CREATE TABLE links(src TEXT, dst TEXT, PRIMARY KEY(src, dst));
        CREATE VIRTUAL TABLE fts_main USING fts5(id UNINDEXED, text, tokenize='unicode61');
        CREATE VIRTUAL TABLE fts_tri  USING fts5(id UNINDEXED, text, tokenize='trigram');
        "
    ))?;
    Ok(())
}

/// 増分 sync。変更・新規・削除を検出して索引を追随させる。戻り値は更新ノート数。
pub fn sync(vault: &Vault, conn: &Connection) -> Result<usize> {
    let files = vault.list_note_files();
    let mut updated = 0usize;

    let mut known: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT id, mtime FROM notes")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (id, mtime) = row?;
            known.insert(id, mtime);
        }
    }

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (id, path) in files {
        seen.insert(id.clone());
        let mtime = fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if known.get(&id) == Some(&mtime) {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else { continue };
        // 適合違反ファイルは索引せずスキップ(conformance 検査は別途 check で可視化予定)
        let Ok(note) = Note::parse(&content) else { continue };
        upsert(conn, vault, &id, mtime, &note)?;
        updated += 1;
    }

    for gone in known.keys().filter(|k| !seen.contains(*k)) {
        conn.execute("DELETE FROM notes WHERE id=?1", [gone])?;
        conn.execute("DELETE FROM links WHERE src=?1", [gone])?;
        conn.execute("DELETE FROM fts_main WHERE id=?1", [gone])?;
        conn.execute("DELETE FROM fts_tri WHERE id=?1", [gone])?;
        updated += 1;
    }
    Ok(updated)
}

fn upsert(conn: &Connection, vault: &Vault, id: &str, mtime: i64, note: &Note) -> Result<()> {
    let f = &note.front;
    let search_text = format!(
        "{} {} {} {}",
        f.title.as_deref().unwrap_or(""),
        f.description.as_deref().unwrap_or(""),
        f.tags.join(" "),
        note.body
    );
    conn.execute(
        "INSERT OR REPLACE INTO notes(id, title, description, status, origin, generated_by, generated_at, mtime, body)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        rusqlite::params![
            id,
            f.title,
            f.description,
            f.effective_status(),
            f.origin,
            f.generated.as_ref().map(|g| g.by.clone()),
            f.generated.as_ref().map(|g| g.at.clone()),
            mtime,
            note.body,
        ],
    )?;
    conn.execute("DELETE FROM fts_main WHERE id=?1", [id])?;
    conn.execute("DELETE FROM fts_tri WHERE id=?1", [id])?;
    conn.execute(
        "INSERT INTO fts_main(id, text) VALUES(?1, ?2)",
        rusqlite::params![id, wakati(&search_text)],
    )?;
    conn.execute(
        "INSERT INTO fts_tri(id, text) VALUES(?1, ?2)",
        rusqlite::params![id, search_text],
    )?;
    conn.execute("DELETE FROM links WHERE src=?1", [id])?;
    for dst in extract_links(id, &note.body, vault) {
        conn.execute(
            "INSERT OR IGNORE INTO links(src, dst) VALUES(?1, ?2)",
            rusqlite::params![id, dst],
        )?;
    }
    Ok(())
}

/// 標準 markdown リンクから .md 宛先をノート ID へ解決(OKF §6.1)。
/// バンドル相対(/x.md)と相対(./x.md, ../x.md)の両形を受ける。
fn extract_links(src_id: &str, body: &str, _vault: &Vault) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(pos) = rest.find("](") {
        rest = &rest[pos + 2..];
        let Some(end) = rest.find(')') else { break };
        let target = &rest[..end];
        rest = &rest[end..];
        if !target.ends_with(".md") || target.starts_with("http") {
            continue;
        }
        let resolved = if let Some(abs) = target.strip_prefix('/') {
            abs.to_string()
        } else {
            // 相対: src の親ディレクトリから解決
            let base = std::path::Path::new(src_id).parent().unwrap_or(std::path::Path::new(""));
            let mut parts: Vec<&str> = base.iter().filter_map(|c| c.to_str()).collect();
            for comp in target.split('/') {
                match comp {
                    "." | "" => {}
                    ".." => {
                        parts.pop();
                    }
                    c => parts.push(c),
                }
            }
            parts.join("/")
        };
        out.push(resolved.trim_end_matches(".md").to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_and_incremental() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        vault.new_human_note("認証 メモ", "認証フローの見直し。[設計](/notes/設計.md) 参照。", "human:o").unwrap();
        let conn = open_db(&vault).unwrap();
        assert_eq!(sync(&vault, &conn).unwrap(), 1);
        assert_eq!(sync(&vault, &conn).unwrap(), 0); // 変更なしなら 0
        let n: i64 = conn.query_row("SELECT count(*) FROM links", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }
}
