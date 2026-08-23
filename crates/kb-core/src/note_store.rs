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
    let transaction = conn.unchecked_transaction()?;
    let op_id = crate::authority::NoteUid::new().to_string();
    queue_put(
        vault,
        &transaction,
        raw,
        note,
        &op_id,
        log_entry,
        commit_message,
    )?;
    transaction.commit()?;
    Ok(())
}

/// 複数ノートを同じtransactionへ積むexecutor専用口。commitとexport flushは呼び出し側が行う。
pub(crate) fn queue_put(
    vault: &Vault,
    conn: &Connection,
    raw: &str,
    note: &Note,
    op_id: &str,
    log_entry: &str,
    commit_message: &str,
) -> Result<()> {
    let id = NoteId::parse(raw)?;
    let base_document = conn
        .query_row(
            "SELECT document FROM notes WHERE id = ?1",
            [id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    crate::index::upsert(conn, vault, id.as_str(), now_nanos()?, note)?;
    validate_authority_write(conn, id.as_str(), note)?;
    conn.execute(
        "INSERT INTO note_exports(op_id, note_id, operation, base_document, document, log_entry, commit_message)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            op_id,
            id.as_str(),
            UPSERT,
            base_document,
            note.to_file_string()?,
            log_entry,
            commit_message
        ],
    )?;
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
    let note_uid: Option<String> = transaction
        .query_row(
            "SELECT note_uid FROM notes WHERE id = ?1",
            [id.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    if let Some(note_uid) = &note_uid {
        let source: Option<String> = transaction
            .query_row(
                "SELECT source.id FROM note_relations relation
                 JOIN notes source ON source.note_uid = relation.src_uid
                 WHERE relation.target_uid = ?1 LIMIT 1",
                [note_uid],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(source) = source {
            bail!("typed relationの参照先は削除できない: {source} -> {id}");
        }
    }
    transaction.execute("DELETE FROM notes WHERE id = ?1", [id.as_str()])?;
    transaction.execute(
        "DELETE FROM links WHERE src = ?1 OR dst = ?1",
        [id.as_str()],
    )?;
    transaction.execute("DELETE FROM fts_main WHERE id = ?1", [id.as_str()])?;
    transaction.execute("DELETE FROM fts_tri WHERE id = ?1", [id.as_str()])?;
    transaction.execute(
        "DELETE FROM fts_anchor WHERE src = ?1 OR dst = ?1",
        [id.as_str()],
    )?;
    transaction.execute("DELETE FROM note_vecs WHERE id = ?1", [id.as_str()])?;
    if let Some(note_uid) = note_uid {
        transaction.execute("DELETE FROM note_relations WHERE src_uid = ?1", [&note_uid])?;
    }
    transaction.execute(
        "INSERT INTO note_exports(op_id, note_id, operation, base_document, document, log_entry, commit_message)
         VALUES (lower(hex(randomblob(16))), ?1, ?2, ?3, NULL, ?4, ?5)",
        rusqlite::params![id.as_str(), DELETE, base_document, log_entry, commit_message],
    )?;
    transaction.commit()?;
    Ok(())
}

fn validate_authority_write(conn: &Connection, id: &str, note: &Note) -> Result<()> {
    use crate::authority::{AuthorityRole, AuthorityStatus, RelationKind};

    let Some(uid) = &note.front.note_uid else {
        return Ok(());
    };
    let authority = note
        .front
        .authority
        .as_ref()
        .context("note_uid付きノートにauthorityがない")?;
    for relation in &note.front.relations {
        let target: Option<(String, String, String, String)> = conn
            .query_row(
                "SELECT namespace, authority_role, authority_status, authority_scope
                 FROM notes WHERE note_uid=?1",
                [relation.target.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((target_namespace, target_role, target_status, target_scope)) = target else {
            bail!(
                "typed relationの参照先がない: {id} {} -> {}",
                relation.kind.as_str(),
                relation.target
            );
        };
        if relation.kind == RelationKind::Supersedes
            && (!authority.is_active_canonical()
                || target_role != AuthorityRole::Canonical.as_str()
                || target_status != AuthorityStatus::Superseded.as_str()
                || target_namespace != authority.namespace.as_str()
                || target_scope != authority.scope)
        {
            bail!(
                "supersedesは同じnamespace/scopeのactive canonicalからsuperseded canonicalへ結ぶ"
            );
        }
    }

    let incoming_supersedes: Option<(String, String, String, String)> = conn
        .query_row(
            "SELECT source.namespace, source.authority_role, source.authority_status,
                    source.authority_scope
             FROM note_relations relation
             JOIN notes source ON source.note_uid = relation.src_uid
             WHERE relation.target_uid=?1 AND relation.kind='supersedes' LIMIT 1",
            [uid.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    if authority.role == AuthorityRole::Canonical && authority.status == AuthorityStatus::Superseded
    {
        let Some((namespace, role, status, scope)) = incoming_supersedes else {
            bail!("superseded canonicalに後継のsupersedes relationがない: {id}");
        };
        if namespace != authority.namespace.as_str()
            || role != AuthorityRole::Canonical.as_str()
            || status != AuthorityStatus::Active.as_str()
            || scope != authority.scope
        {
            bail!("superseded canonicalの後継authorityが一致しない: {id}");
        }
    } else if incoming_supersedes.is_some() {
        bail!("supersedesの参照先はsuperseded canonicalに固定する: {id}");
    }
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
    use crate::authority::{Authority, AuthorityRole, AuthorityStatus, NoteNamespace, NoteUid};
    use crate::frontmatter::Frontmatter;
    use crate::vault::NoteProposal;

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

    #[test]
    fn an_existing_note_uid_cannot_be_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let id = vault
            .propose(
                &conn,
                NoteProposal {
                    title: "不変UID",
                    body: "本文",
                    description: None,
                    tags: &["test".into()],
                    authority: Authority {
                        namespace: NoteNamespace::Knowledge,
                        role: AuthorityRole::Canonical,
                        status: AuthorityStatus::Active,
                        scope: "test/immutable-uid".into(),
                    },
                    relations: Vec::new(),
                    allow_new_tags: true,
                    client: "test/client",
                },
            )
            .unwrap();
        let mut changed = read(&conn, &id).unwrap();
        let original = changed.front.note_uid.clone();
        changed.front.note_uid = Some(NoteUid::at(999));

        assert!(put(&vault, &conn, &id, &changed, "update", "update note").is_err());
        assert_eq!(read(&conn, &id).unwrap().front.note_uid, original);
    }
}
