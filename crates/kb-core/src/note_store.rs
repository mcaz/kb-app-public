//! ノートの実行時正本と、Markdown export の durable outbox。
//!
//! SQLite 自体は Git へ入れない。DB transaction を日常の読み書き境界にし、同じ
//! transaction で outbox を積むことで、Markdown が一時的に古くても再出力できる。

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension};

use crate::frontmatter::Note;
use crate::note_id::NoteId;
use crate::vault::Vault;

const UPSERT: &str = "upsert";
const DELETE: &str = "delete";

#[derive(Debug)]
pub(crate) struct PendingExport {
    pub seq: i64,
    pub op_id: String,
    pub note_id: String,
    pub operation: ExportOperation,
    pub base_document: Option<String>,
    pub document: Option<String>,
    pub log_entry: String,
    pub commit_message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExportOperation {
    Upsert,
    Delete,
}

pub fn read(conn: &Connection, raw: &str) -> Result<Note> {
    let id = NoteId::parse(raw)?;
    let document = conn
        .query_row(
            "SELECT document FROM notes WHERE id = ?1",
            [id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .with_context(|| format!("ノートが見つからない: {id}"))?;
    if document.is_empty() {
        bail!("ノートのDB本文がまだ復元されていない: {id}");
    }
    Note::parse(&document).with_context(|| format!("DB内ノートのparse失敗: {id}"))
}

pub fn contains(conn: &Connection, raw: &str) -> Result<bool> {
    let id = NoteId::parse(raw)?;
    Ok(conn
        .query_row("SELECT 1 FROM notes WHERE id = ?1", [id.as_str()], |_| {
            Ok(())
        })
        .optional()?
        .is_some())
}

pub(crate) fn put(
    vault: &Vault,
    conn: &Connection,
    raw: &str,
    note: &Note,
    log_entry: &str,
    commit_message: &str,
) -> Result<()> {
    let id = NoteId::parse(raw)?;
    let transaction = conn.unchecked_transaction()?;
    let base_document = transaction
        .query_row(
            "SELECT document FROM notes WHERE id = ?1",
            [id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    crate::index::upsert(&transaction, vault, id.as_str(), now_nanos()?, note)?;
    transaction.execute(
        "INSERT INTO note_exports(op_id, note_id, operation, base_document, document, log_entry, commit_message)
         VALUES (lower(hex(randomblob(16))), ?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            id.as_str(),
            UPSERT,
            base_document,
            note.to_file_string()?,
            log_entry,
            commit_message
        ],
    )?;
    transaction.commit()?;
    Ok(())
}

pub(crate) fn delete(
    conn: &Connection,
    raw: &str,
    log_entry: &str,
    commit_message: &str,
) -> Result<()> {
    let id = NoteId::parse(raw)?;
    let transaction = conn.unchecked_transaction()?;
    let base_document = transaction
        .query_row(
            "SELECT document FROM notes WHERE id = ?1",
            [id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .with_context(|| format!("削除するノートが見つからない: {id}"))?;
    transaction.execute("DELETE FROM notes WHERE id = ?1", [id.as_str()])?;
    transaction.execute(
        "DELETE FROM links WHERE src = ?1 OR dst = ?1",
        [id.as_str()],
    )?;
    transaction.execute("DELETE FROM fts_main WHERE id = ?1", [id.as_str()])?;
    transaction.execute("DELETE FROM fts_tri WHERE id = ?1", [id.as_str()])?;
    transaction.execute("DELETE FROM note_vecs WHERE id = ?1", [id.as_str()])?;
    transaction.execute(
        "INSERT INTO note_exports(op_id, note_id, operation, base_document, document, log_entry, commit_message)
         VALUES (lower(hex(randomblob(16))), ?1, ?2, ?3, NULL, ?4, ?5)",
        rusqlite::params![id.as_str(), DELETE, base_document, log_entry, commit_message],
    )?;
    transaction.commit()?;
    Ok(())
}

pub(crate) fn pending(conn: &Connection) -> Result<Vec<PendingExport>> {
    let mut statement = conn.prepare(
        "SELECT seq, op_id, note_id, operation, base_document, document, log_entry, commit_message
         FROM note_exports ORDER BY seq",
    )?;
    let rows = statement.query_map([], |row| {
        let operation = match row.get::<_, String>(3)?.as_str() {
            UPSERT => ExportOperation::Upsert,
            DELETE => ExportOperation::Delete,
            other => {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    format!("unknown note export operation: {other}").into(),
                ));
            }
        };
        Ok(PendingExport {
            seq: row.get(0)?,
            op_id: row.get(1)?,
            note_id: row.get(2)?,
            operation,
            base_document: row.get(4)?,
            document: row.get(5)?,
            log_entry: row.get(6)?,
            commit_message: row.get(7)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub(crate) fn complete(conn: &Connection, seq: i64) -> Result<()> {
    conn.execute("DELETE FROM note_exports WHERE seq = ?1", [seq])?;
    Ok(())
}

pub fn pending_count(conn: &Connection) -> Result<usize> {
    let count: i64 = conn.query_row("SELECT count(*) FROM note_exports", [], |row| row.get(0))?;
    usize::try_from(count).context("negative note export count")
}

fn now_nanos() -> Result<i64> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_nanos();
    i64::try_from(nanos).context("timestamp does not fit in SQLite INTEGER")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontmatter::Frontmatter;

    fn note(body: &str) -> Note {
        let mut front = Frontmatter::new_note("DB first");
        front.origin = Some("agent".into());
        front.tags = vec!["test".into()];
        Note {
            front,
            body: body.into(),
        }
    }

    /// 2026-08-18本人決定: AIの確定状態はMarkdown出力の成否に依存させない。
    #[test]
    fn db_commit_precedes_markdown_export_and_outbox_recovers_it() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let id = "notes/db-first";

        put(&vault, &conn, id, &note("DB本文"), "export", "export note").unwrap();

        assert_eq!(read(&conn, id).unwrap().body, "DB本文\n");
        assert_eq!(pending_count(&conn).unwrap(), 1);
        assert!(!vault.note_path(id).unwrap().exists());

        assert_eq!(vault.flush_note_exports(&conn).unwrap(), 1);
        assert_eq!(pending_count(&conn).unwrap(), 0);
        assert_eq!(vault.read_note(id).unwrap().body, "DB本文\n");
    }

    #[test]
    fn ordinary_sync_ignores_external_markdown_until_explicit_import() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let id = "notes/db-first";
        put(&vault, &conn, id, &note("DB本文"), "export", "export note").unwrap();
        vault.flush_note_exports(&conn).unwrap();

        vault.write_note_fixture(id, &note("外部編集")).unwrap();
        crate::index::sync(&vault, &conn).unwrap();
        assert_eq!(read(&conn, id).unwrap().body, "DB本文\n");

        let report = crate::index::import_markdown_snapshot(&vault, &conn).unwrap();
        assert!(report.degraded.is_empty());
        assert_eq!(read(&conn, id).unwrap().body, "外部編集\n");
    }

    #[test]
    fn markdown_export_never_overwrites_an_external_edit() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let id = "notes/db-first";
        put(
            &vault,
            &conn,
            id,
            &note("元の本文"),
            "export",
            "export note",
        )
        .unwrap();
        vault.flush_note_exports(&conn).unwrap();
        vault.write_note_fixture(id, &note("外部編集")).unwrap();

        put(
            &vault,
            &conn,
            id,
            &note("DBの更新"),
            "update",
            "update note",
        )
        .unwrap();

        assert!(vault.flush_note_exports(&conn).is_err());
        let sync = crate::index::sync_with_degradations(&vault, &conn).unwrap();
        assert!(
            sync.degraded
                .iter()
                .any(|item| matches!(item, crate::degradation::Degradation::MarkdownExport { .. }))
        );
        assert_eq!(read(&conn, id).unwrap().body, "DBの更新\n");
        assert_eq!(vault.read_note(id).unwrap().body, "外部編集\n");
        assert_eq!(pending_count(&conn).unwrap(), 1);
    }

    #[test]
    fn delete_is_visible_in_db_before_markdown_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let id = "notes/db-first";
        put(&vault, &conn, id, &note("本文"), "export", "export note").unwrap();
        vault.flush_note_exports(&conn).unwrap();

        delete(&conn, id, "delete", "delete note").unwrap();
        assert!(read(&conn, id).is_err());
        assert!(vault.note_path(id).unwrap().exists());

        vault.flush_note_exports(&conn).unwrap();
        assert!(!vault.note_path(id).unwrap().exists());
    }

    #[test]
    fn fresh_runtime_db_restores_full_note_from_markdown_export() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let id = "notes/db-first";
        put(
            &vault,
            &conn,
            id,
            &note("復元する本文"),
            "export",
            "export note",
        )
        .unwrap();
        vault.flush_note_exports(&conn).unwrap();
        drop(conn);

        std::fs::remove_dir_all(vault.root.join(".kb")).unwrap();
        let restored = crate::index::open_db(&vault).unwrap();
        assert_eq!(read(&restored, id).unwrap().body, "復元する本文\n");
    }
}
