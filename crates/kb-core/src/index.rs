//! 派生索引(SQLite 1ファイル)。二本立て FTS(lindera 主索引+trigram レスキュー)+
//! リンクテーブル。mtime ベースの増分 sync で「書いてすぐ引ける」を保証する。
//! 接続規律(busy_timeout 必須・WAL)は docs/poc-report.md の PoC ③ 由来。

use std::fs;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rusqlite::Connection;

use crate::frontmatter::Note;
use crate::tokenize::wakati;
use crate::vault::Vault;

const SCHEMA_VERSION: &str = "3";

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
        .query_row("SELECT value FROM meta WHERE key='schema'", [], |r| {
            r.get(0)
        })
        .ok();
    if ver.as_deref() == Some(SCHEMA_VERSION) {
        // 追加カラムの後方互換マイグレーション(破壊的な作り直しをしない —
        // 全テーブル再作成は埋め込みの再計算嵐を起こすため)
        for (col, ddl) in [
            ("tags", "ALTER TABLE notes ADD COLUMN tags TEXT DEFAULT ''"),
            ("created", "ALTER TABLE notes ADD COLUMN created TEXT"),
        ] {
            if conn
                .prepare(&format!("SELECT {col} FROM notes LIMIT 0"))
                .is_err()
            {
                // 破壊的な作り直しをしない(埋め込み再計算の嵐を避ける)
                conn.execute_batch(&format!("{ddl}; UPDATE notes SET mtime = -1;"))?;
            }
        }
        conn.execute_batch("CREATE INDEX IF NOT EXISTS links_dst ON links(dst);")?;
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
            mtime INTEGER, body TEXT, tags TEXT DEFAULT '', created TEXT
        );
        CREATE TABLE links(src TEXT, dst TEXT, PRIMARY KEY(src, dst));
        CREATE INDEX links_dst ON links(dst);
        DROP TABLE IF EXISTS note_vecs;
        CREATE TABLE note_vecs(id TEXT PRIMARY KEY, stamp TEXT, embedding BLOB);
        CREATE VIRTUAL TABLE fts_main USING fts5(id UNINDEXED, text, tokenize='unicode61');
        CREATE VIRTUAL TABLE fts_tri  USING fts5(id UNINDEXED, text, tokenize='trigram');
        "
    ))?;
    Ok(())
}

/// GUI/MCPが本体データと一緒に返す、増分syncの結果。
#[derive(Debug, Default)]
#[must_use = "degradedを捨てると索引失敗を正常に見せるため、必ず処理する"]
pub struct SyncReport {
    pub updated: usize,
    pub degraded: Vec<crate::degradation::Degradation>,
}

/// 部分失敗を続行できない呼び出し元向け。失敗を黙殺せず従来の件数を返す。
pub fn sync(vault: &Vault, conn: &Connection) -> Result<usize> {
    let report = sync_with_degradations(vault, conn)?;
    if !report.degraded.is_empty() {
        let detail = report
            .degraded
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" / ");
        bail!("索引へ取り込めないノートがある: {detail}");
    }
    Ok(report.updated)
}

/// 増分syncのfail-open入口。読めないノートは既存rowを残し、正常なノートだけを
/// 同じtransactionで反映する。呼び出し元は`degraded`をデータと一緒に返すこと。
pub fn sync_with_degradations(vault: &Vault, conn: &Connection) -> Result<SyncReport> {
    let files = vault.list_note_files()?;
    sync_files(vault, conn, files)
}

fn sync_files(
    vault: &Vault,
    conn: &Connection,
    files: Vec<(String, std::path::PathBuf)>,
) -> Result<SyncReport> {
    let mut updated = 0usize;
    let mut degraded = Vec::new();

    let mut known: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT id, mtime FROM notes")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (id, mtime) = row?;
            known.insert(id, mtime);
        }
    }

    // 2026-08-16の10k fixtureでは1件ごとのautocommitが再構築20秒の大半を占めた。
    // 全件を同じ派生索引versionとして反映し、途中失敗も半端な索引を残さない。
    let transaction = conn.unchecked_transaction()?;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (id, path) in files {
        seen.insert(id.clone());
        if let Err(error) = crate::note_id::NoteId::parse(&id) {
            degraded.push(crate::degradation::Degradation::IndexParse {
                note: id,
                detail: error.to_string(),
            });
            continue;
        }
        // ナノ秒精度 — 秒精度だと同一秒内の連続保存が再索引されない(実測で露呈)
        let mtime = match fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .and_then(|modified| {
                modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(std::io::Error::other)
            }) {
            Ok(modified) => modified.as_nanos() as i64,
            Err(error) => {
                degraded.push(crate::degradation::Degradation::IndexMetadata {
                    note: id,
                    detail: error.to_string(),
                });
                continue;
            }
        };
        if known.get(&id) == Some(&mtime) {
            continue;
        }
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) => {
                degraded.push(crate::degradation::Degradation::IndexRead {
                    note: id,
                    detail: error.to_string(),
                });
                continue;
            }
        };
        let note = match Note::parse(&content) {
            Ok(note) => note,
            Err(error) => {
                degraded.push(crate::degradation::Degradation::IndexParse {
                    note: id,
                    detail: error.to_string(),
                });
                continue;
            }
        };
        upsert(&transaction, vault, &id, mtime, &note)?;
        updated += 1;
    }

    for gone in known.keys().filter(|k| !seen.contains(*k)) {
        transaction.execute("DELETE FROM notes WHERE id=?1", [gone])?;
        transaction.execute("DELETE FROM links WHERE src=?1", [gone])?;
        transaction.execute("DELETE FROM fts_main WHERE id=?1", [gone])?;
        transaction.execute("DELETE FROM fts_tri WHERE id=?1", [gone])?;
        transaction.execute("DELETE FROM note_vecs WHERE id=?1", [gone])?;
        updated += 1;
    }
    transaction.commit()?;
    Ok(SyncReport { updated, degraded })
}

/// sync の後段: 未埋め込みノートの追い付き(1回あたり少数に制限し、残は劣化情報で見せる)。
/// モデル未導入なら None(段0 の正常形 — 劣化ではない)。
pub fn embed_step(conn: &Connection) -> Option<crate::degradation::Degradation> {
    if !crate::embed::model_installed() {
        return None;
    }
    match crate::embed::embed_pending(conn, 5) {
        Ok(0) => None,
        Ok(remaining) => Some(crate::degradation::Degradation::EmbeddingIndexPending { remaining }),
        Err(error) => Some(crate::degradation::Degradation::EmbeddingIndex {
            detail: error.to_string(),
        }),
    }
}

fn upsert(conn: &Connection, vault: &Vault, id: &str, mtime: i64, note: &Note) -> Result<()> {
    let f = &note.front;
    // 添付ファイル名も検索対象に(「あの PDF どこだっけ」を引けるように。FR-C8)
    let attach_names: String = vault
        .list_attachments(id)?
        .iter()
        .map(|(n, _)| n.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let search_text = format!(
        "{} {} {} {} {}",
        f.title.as_deref().unwrap_or(""),
        f.description.as_deref().unwrap_or(""),
        f.tags.join(" "),
        attach_names,
        note.body
    );
    // 旧本文は notes を上書きする前に取っておく(埋め込み保持判定に使う)
    let old_body: Option<String> = conn
        .query_row("SELECT body FROM notes WHERE id=?1", [id], |r| r.get(0))
        .ok();
    conn.execute(
        "INSERT OR REPLACE INTO notes(id, title, description, status, origin, generated_by, generated_at, mtime, body, tags, created)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
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
            f.tags.join(" "),
            f.created_at(),
        ],
    )?;
    conn.execute("DELETE FROM fts_main WHERE id=?1", [id])?;
    conn.execute("DELETE FROM fts_tri WHERE id=?1", [id])?;
    // 本文が実際に変わったときだけ埋め込みを捨てる(メタ変更や mtime 精度移行で
    // 全ノート再埋め込みの嵐を起こさない)
    if old_body.as_deref() != Some(note.body.as_str()) {
        conn.execute("DELETE FROM note_vecs WHERE id=?1", [id])?;
    }
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
            let base = std::path::Path::new(src_id)
                .parent()
                .unwrap_or(std::path::Path::new(""));
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
    use crate::frontmatter::Frontmatter;

    #[test]
    fn sync_and_incremental() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        vault
            .propose_for_test(
                "認証 メモ",
                "認証フローの見直し。[設計](/notes/設計.md) 参照。",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let conn = open_db(&vault).unwrap();
        assert_eq!(sync(&vault, &conn).unwrap(), 1);
        assert_eq!(sync(&vault, &conn).unwrap(), 0); // 変更なしなら 0
        let n: i64 = conn
            .query_row("SELECT count(*) FROM links", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    /// 2026-08-16の10k高速化で追加したtransactionを外す退行と、半端な索引を防ぐ。
    #[test]
    fn sync_rolls_back_every_note_when_one_upsert_fails() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        for id in ["notes/a", "notes/b"] {
            let mut front = Frontmatter::new_note(id);
            front.tags = vec!["test".into()];
            vault
                .write_note_fixture(
                    id,
                    &Note {
                        front,
                        body: "本文".into(),
                    },
                )
                .unwrap();
        }
        let conn = open_db(&vault).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_second_note BEFORE INSERT ON notes
             WHEN NEW.id = 'notes/b'
             BEGIN SELECT RAISE(FAIL, 'fixture failure'); END;",
        )
        .unwrap();

        assert!(sync(&vault, &conn).is_err());
        let indexed: i64 = conn
            .query_row("SELECT count(*) FROM notes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(indexed, 0);
    }

    #[test]
    fn malformed_note_keeps_the_stale_row_and_reports_the_degradation() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        vault
            .propose_for_test(
                "壊れる前",
                "残す本文",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let conn = open_db(&vault).unwrap();
        sync(&vault, &conn).unwrap();
        let (id, path) = vault.list_note_files().unwrap().remove(0);
        fs::write(&path, "frontmatterではない").unwrap();

        let report = sync_with_degradations(&vault, &conn).unwrap();
        assert_eq!(report.updated, 0);
        assert!(report.degraded.iter().any(|item| matches!(
            item,
            crate::degradation::Degradation::IndexParse { note, .. } if note == &id
        )));
        let indexed_body: String = conn
            .query_row("SELECT body FROM notes WHERE id=?1", [&id], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(indexed_body, "残す本文\n");
        assert!(sync(&vault, &conn).is_err(), "strict入口は黙って続行しない");
    }

    #[test]
    fn metadata_and_read_failures_are_separately_typed() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        let missing = dir.path().join("missing.md");
        let directory = dir.path().join("directory.md");
        fs::create_dir(&directory).unwrap();

        let report = sync_files(
            &vault,
            &conn,
            vec![
                ("notes/missing".into(), missing),
                ("notes/directory".into(), directory),
                ("notes/a.files/inside".into(), dir.path().join("unused")),
            ],
        )
        .unwrap();
        assert!(report.degraded.iter().any(|item| matches!(
            item,
            crate::degradation::Degradation::IndexMetadata { note, .. }
                if note == "notes/missing"
        )));
        assert!(report.degraded.iter().any(|item| matches!(
            item,
            crate::degradation::Degradation::IndexRead { note, .. }
                if note == "notes/directory"
        )));
        assert!(report.degraded.iter().any(|item| matches!(
            item,
            crate::degradation::Degradation::IndexParse { note, .. }
                if note == "notes/a.files/inside"
        )));
    }
}
