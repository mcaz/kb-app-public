//! 元実行の証跡を書き換えず、復元した事実だけを別のdurable tableへ残す。

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use super::{HistoryNote, HistoryRun};

pub(crate) const ROLLBACK_TABLES: [&str; 1] = ["tag_vocabulary_rollbacks"];
pub(crate) const ROLLBACK_SCHEMA_SQL: &str = "
CREATE TABLE tag_vocabulary_rollbacks(
    rollback_id TEXT PRIMARY KEY NOT NULL,
    execution_id TEXT NOT NULL UNIQUE REFERENCES tag_vocabulary_runs(execution_id),
    plan_hash TEXT NOT NULL,
    reason TEXT NOT NULL,
    client TEXT NOT NULL,
    restored_at TEXT NOT NULL,
    restored_notes INTEGER NOT NULL CHECK(restored_notes > 0)
);";

#[derive(Debug, Clone, Serialize)]
pub struct RollbackRecord {
    pub rollback_id: String,
    pub execution_id: String,
    pub plan_hash: String,
    pub reason: String,
    pub client: String,
    pub restored_at: String,
    pub restored_notes: usize,
}

/// 原文をMCP応答へ混ぜないためSerializeを実装しない。
#[derive(Debug)]
pub(crate) struct RollbackHistory {
    pub run: HistoryRun,
    pub notes: Vec<HistoryNote>,
    pub rollback: Option<RollbackRecord>,
}

fn validate_record(record: &RollbackRecord) -> Result<()> {
    ensure!(
        crate::artifact::is_ulid(&record.rollback_id)
            && crate::artifact::is_ulid(&record.execution_id)
            && record.rollback_id != record.execution_id,
        "語彙復元履歴のIDが不正"
    );
    super::validate_line(&record.plan_hash, 200)?;
    super::validate_line(&record.reason, 2000)?;
    super::validate_line(&record.client, 200)?;
    time::OffsetDateTime::parse(
        &record.restored_at,
        &time::format_description::well_known::Rfc3339,
    )
    .context("語彙復元履歴の実行時刻が不正")?;
    ensure!(record.restored_notes > 0, "語彙復元履歴の復元件数がない");
    Ok(())
}

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RollbackRecord> {
    Ok(RollbackRecord {
        rollback_id: row.get(0)?,
        execution_id: row.get(1)?,
        plan_hash: row.get(2)?,
        reason: row.get(3)?,
        client: row.get(4)?,
        restored_at: row.get(5)?,
        restored_notes: usize::try_from(row.get::<_, i64>(6)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Integer,
                Box::new(error),
            )
        })?,
    })
}

pub(super) fn read_record(conn: &Connection, execution_id: &str) -> Result<Option<RollbackRecord>> {
    let record = conn
        .query_row(
            "SELECT rollback_id,execution_id,plan_hash,reason,client,restored_at,restored_notes FROM tag_vocabulary_rollbacks WHERE execution_id=?1",
            [execution_id],
            read_row,
        )
        .optional()?;
    if let Some(record) = &record {
        validate_record(record)?;
    }
    Ok(record)
}

pub(crate) fn record_rollback(conn: &Connection, record: &RollbackRecord) -> Result<()> {
    ensure!(
        !conn.is_autocommit(),
        "語彙復元履歴の保存にはtransactionが必要"
    );
    validate_record(record)?;
    let changed_notes: i64 = conn.query_row(
        "SELECT changed_notes FROM tag_vocabulary_runs WHERE execution_id=?1",
        [&record.execution_id],
        |row| row.get(0),
    )?;
    ensure!(
        changed_notes == i64::try_from(record.restored_notes)?,
        "語彙復元履歴の元実行と復元件数が一致しない"
    );
    let used_by_apply: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM tag_vocabulary_runs WHERE execution_id=?1)",
        [&record.rollback_id],
        |row| row.get(0),
    )?;
    ensure!(
        !used_by_apply,
        "語彙復元履歴のIDは既存の語彙変更実行と重複できない"
    );
    conn.execute(
        "INSERT INTO tag_vocabulary_rollbacks(rollback_id,execution_id,plan_hash,reason,client,restored_at,restored_notes) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![record.rollback_id,record.execution_id,record.plan_hash,record.reason,record.client,record.restored_at,i64::try_from(record.restored_notes)?],
    )?;
    Ok(())
}

/// 対象runだけを完全検証する。全履歴の読み直しをplanごとに要求しない。
pub(crate) fn read_for_rollback(
    conn: &Connection,
    execution_id: &str,
    max_notes: usize,
    max_document_bytes: usize,
) -> Result<Option<RollbackHistory>> {
    // 破損した原文やnote IDをserde/parse由来のcause経由で外へ公開しない。
    read_for_rollback_inner(conn, execution_id, max_notes, max_document_bytes).map_err(|_| {
        anyhow::anyhow!("語彙復元用の履歴が不正、または件数・原文容量の上限を超えている")
    })
}

fn read_for_rollback_inner(
    conn: &Connection,
    execution_id: &str,
    max_notes: usize,
    max_document_bytes: usize,
) -> Result<Option<RollbackHistory>> {
    ensure!(
        crate::artifact::is_ulid(execution_id),
        "語彙変更履歴の実行IDが不正"
    );
    if conn.is_autocommit() {
        let transaction = conn.unchecked_transaction()?;
        let result =
            read_for_rollback_inner(&transaction, execution_id, max_notes, max_document_bytes)?;
        transaction.commit()?;
        return Ok(result);
    }
    super::verify_schema(conn)?;
    verify_rollback_schema(conn)?;
    let run = conn.query_row("SELECT execution_id,workspace_id,source_note_uid,source_revision,plan_hash,operations_json,reason,client,applied_at,changed_notes FROM tag_vocabulary_runs WHERE execution_id=?1",[execution_id],super::read_run).optional()?;
    let Some(run) = run else { return Ok(None) };
    super::validate_run(&run)?;
    let (count, bytes): (i64, i64) = conn.query_row(
        "SELECT count(*),coalesce(sum(length(CAST(before_document AS BLOB))+length(CAST(after_document AS BLOB))),0) FROM tag_vocabulary_run_notes WHERE execution_id=?1",
        [execution_id], |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(
        usize::try_from(count)? == run.changed_notes && run.changed_notes <= max_notes,
        "語彙変更履歴の件数が不正"
    );
    ensure!(
        usize::try_from(bytes)? <= max_document_bytes,
        "語彙変更履歴の原文容量が上限を超えている"
    );
    let mut query = conn.prepare("SELECT note_id,note_uid,before_document,after_document,before_hash,after_hash,before_tags,after_tags FROM tag_vocabulary_run_notes WHERE execution_id=?1 ORDER BY note_id")?;
    let mut notes = Vec::with_capacity(run.changed_notes);
    let mut uids = std::collections::BTreeSet::new();
    for row in query.query_map([execution_id], |row| {
        Ok((
            HistoryNote {
                note_id: row.get(0)?,
                note_uid: row.get(1)?,
                before_document: row.get(2)?,
                after_document: row.get(3)?,
            },
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
        ))
    })? {
        let (note, before_hash, after_hash, before_tags, after_tags) = row?;
        let summary = super::note_summary(&note)?;
        ensure!(
            summary.before_hash == before_hash
                && summary.after_hash == after_hash
                && summary.before_tags == serde_json::from_str::<Vec<String>>(&before_tags)?
                && summary.after_tags == serde_json::from_str::<Vec<String>>(&after_tags)?,
            "語彙変更履歴の原文と要約が一致しない"
        );
        if let Some(uid) = &note.note_uid {
            ensure!(uids.insert(uid.clone()), "語彙変更履歴のUIDが重複している");
        }
        notes.push(note);
    }
    let rollback = read_record(conn, execution_id)?;
    if let Some(record) = &rollback {
        ensure!(
            record.restored_notes == run.changed_notes,
            "語彙復元履歴の件数が一致しない"
        );
    }
    Ok(Some(RollbackHistory {
        run,
        notes,
        rollback,
    }))
}

pub(crate) fn rollback_table_present(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='tag_vocabulary_rollbacks')", [], |row| row.get(0))?)
}

pub(crate) fn verify_rollback_schema(conn: &Connection) -> Result<()> {
    let actual: String = conn.query_row(
        "SELECT sql FROM sqlite_schema WHERE type='table' AND name='tag_vocabulary_rollbacks'",
        [],
        |row| row.get(0),
    )?;
    let normalize = |sql: &str| {
        sql.chars()
            .filter(|character| !character.is_whitespace() && *character != ';')
            .collect::<String>()
            .to_ascii_lowercase()
    };
    ensure!(
        normalize(ROLLBACK_SCHEMA_SQL) == normalize(&actual),
        "語彙復元履歴のdurable table制約が不正"
    );
    Ok(())
}

pub(crate) fn verify_rollback_integrity(conn: &Connection) -> Result<()> {
    verify_rollback_schema(conn)?;
    super::verify_schema(conn)?;
    let orphan: i64 = conn.query_row("SELECT count(*) FROM tag_vocabulary_rollbacks b LEFT JOIN tag_vocabulary_runs r ON r.execution_id=b.execution_id WHERE r.execution_id IS NULL OR r.changed_notes!=b.restored_notes OR EXISTS(SELECT 1 FROM tag_vocabulary_runs x WHERE x.execution_id=b.rollback_id)", [], |row| row.get(0))?;
    ensure!(orphan == 0, "語彙復元履歴の元実行・件数・IDが一致しない");
    let mut rows = conn.prepare("SELECT rollback_id,execution_id,plan_hash,reason,client,restored_at,restored_notes FROM tag_vocabulary_rollbacks")?;
    for row in rows.query_map([], read_row)? {
        validate_record(&row?)?;
    }
    Ok(())
}

pub(super) fn initialize_recovery_schema(conn: &Connection) -> Result<()> {
    ensure!(
        !conn.is_autocommit(),
        "語彙復元履歴の復旧準備にはtransactionが必要"
    );
    if !rollback_table_present(conn)? {
        let declared: String =
            conn.query_row("SELECT value FROM meta WHERE key='schema'", [], |row| {
                row.get(0)
            })?;
        ensure!(
            matches!(declared.as_str(), "7" | "9"),
            "語彙復元履歴の不在を補える限定復旧schemaではない"
        );
        conn.execute_batch(ROLLBACK_SCHEMA_SQL)?;
    }
    verify_rollback_schema(conn)
}

#[cfg(test)]
pub(crate) fn record_for_test(conn: &Connection, execution_id: &str) -> String {
    let changed_notes: i64 = conn
        .query_row(
            "SELECT changed_notes FROM tag_vocabulary_runs WHERE execution_id=?1",
            [execution_id],
            |row| row.get(0),
        )
        .unwrap();
    let record = RollbackRecord {
        rollback_id: crate::authority::NoteUid::new().to_string(),
        execution_id: execution_id.into(),
        plan_hash: format!("sha256:{}", "b".repeat(64)),
        reason: "元の語彙へ戻す".into(),
        client: "test/client".into(),
        restored_at: crate::frontmatter::now_iso(),
        restored_notes: usize::try_from(changed_notes).unwrap(),
    };
    let transaction = conn.unchecked_transaction().unwrap();
    record_rollback(&transaction, &record).unwrap();
    transaction.commit().unwrap();
    record.rollback_id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Connection, String) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(super::super::SCHEMA_SQL).unwrap();
        conn.execute_batch(ROLLBACK_SCHEMA_SQL).unwrap();
        conn.execute_batch("CREATE TABLE notes(id TEXT PRIMARY KEY,note_uid TEXT,normal_reference_allowed INTEGER NOT NULL)").unwrap();
        let id = super::super::record_for_test(&conn);
        (conn, id)
    }

    #[test]
    fn rollback_is_append_only_and_is_visible_without_original_documents() {
        let (conn, id) = setup();
        let before = read_for_rollback(&conn, &id, 1, 8192).unwrap().unwrap();
        let rollback_id = record_for_test(&conn, &id);
        let after = read_for_rollback(&conn, &id, 1, 8192).unwrap().unwrap();
        assert_eq!(before.run.execution_id, after.run.execution_id);
        assert_eq!(before.run.plan_hash, after.run.plan_hash);
        assert_eq!(
            before.notes[0].before_document,
            after.notes[0].before_document
        );
        assert_eq!(
            before.notes[0].after_document,
            after.notes[0].after_document
        );
        assert!(before.rollback.is_none());
        assert_eq!(after.rollback.unwrap().rollback_id, rollback_id);
        verify_rollback_integrity(&conn).unwrap();
        let detail = super::super::get(&conn, &id, None, None).unwrap().unwrap();
        assert_eq!(
            detail.run.rollback.as_ref().unwrap().rollback_id,
            rollback_id
        );
        let list = super::super::list(&conn, None, None).unwrap();
        assert_eq!(list.items[0].rollback.as_ref().unwrap().restored_notes, 1);
        assert!(
            !serde_json::to_string(&detail)
                .unwrap()
                .contains("private original content")
        );
    }

    #[test]
    fn rollback_record_requires_transaction_and_original_run_count_and_unique_ids() {
        let (conn, id) = setup();
        record_for_test(&conn, &id);
        let record = read_record(&conn, &id).unwrap().unwrap();
        assert!(record_rollback(&conn, &record).is_err());
        {
            let mut altered = record.clone();
            altered.rollback_id = crate::authority::NoteUid::new().to_string();
            let transaction = conn.unchecked_transaction().unwrap();
            assert!(record_rollback(&transaction, &altered).is_err());
        }
        let second = super::super::record_for_test(&conn);
        for mismatch in 0..4 {
            let mut altered = record.clone();
            altered.execution_id = second.clone();
            altered.rollback_id = crate::authority::NoteUid::new().to_string();
            match mismatch {
                0 => altered.restored_notes += 1,
                1 => altered.rollback_id = id.clone(),
                2 => altered.execution_id = crate::authority::NoteUid::new().to_string(),
                _ => altered.rollback_id = record.rollback_id.clone(),
            }
            let transaction = conn.unchecked_transaction().unwrap();
            assert!(record_rollback(&transaction, &altered).is_err());
        }
        assert_eq!(
            conn.query_row("SELECT count(*) FROM tag_vocabulary_rollbacks", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            1
        );
    }

    /// 2026-09-08: 復元の容量判定はUTF-8の前後両方を数え、上限までの部分読取を返さない。
    #[test]
    fn rollback_read_checks_both_documents_before_allocating_and_requires_every_note() {
        let (conn, id) = setup();
        let history = read_for_rollback(&conn, &id, 1, 8192).unwrap().unwrap();
        let bytes = history.notes[0].before_document.len() + history.notes[0].after_document.len();
        assert!(read_for_rollback(&conn, &id, 0, bytes).is_err());
        assert!(read_for_rollback(&conn, &id, 1, bytes - 1).is_err());
        assert!(read_for_rollback(&conn, &id, 1, bytes).unwrap().is_some());
        conn.execute("DELETE FROM tag_vocabulary_run_notes", [])
            .unwrap();
        assert!(read_for_rollback(&conn, &id, 1, bytes).is_err());
        assert!(
            read_for_rollback(
                &conn,
                &crate::authority::NoteUid::new().to_string(),
                1,
                bytes
            )
            .unwrap()
            .is_none()
        );
    }

    /// 2026-09-08: 元原文のhash・タグ・UID・件数の不整合を拒否し、破損内容をエラーへ出さない。
    #[test]
    fn rollback_read_rejects_corrupt_originals_and_summary_without_exposing_private_data() {
        for alteration in [
            "UPDATE tag_vocabulary_run_notes SET before_document='private malformed original'",
            "UPDATE tag_vocabulary_run_notes SET before_hash='sha256:wrong'",
            "UPDATE tag_vocabulary_run_notes SET after_tags='[\"private tag\"]'",
            "UPDATE tag_vocabulary_run_notes SET note_uid='private-uid'",
            "UPDATE tag_vocabulary_runs SET changed_notes=2",
            "UPDATE tag_vocabulary_runs SET operations_json='private malformed operations'",
        ] {
            let (conn, id) = setup();
            conn.execute_batch(alteration).unwrap();
            let error = read_for_rollback(&conn, &id, 10, 8192).unwrap_err();
            assert!(!format!("{error:#}").contains("private"));
        }
    }

    #[test]
    fn invalid_rollback_rows_and_partial_ddl_fail_integrity() {
        for alteration in [
            "UPDATE tag_vocabulary_rollbacks SET restored_notes=2",
            "UPDATE tag_vocabulary_rollbacks SET execution_id='01ARZ3NDEKTSV4RRFFQ69G5FAV'",
            "UPDATE tag_vocabulary_rollbacks SET rollback_id='invalid'",
            "UPDATE tag_vocabulary_rollbacks SET restored_at='private time'",
            "UPDATE tag_vocabulary_rollbacks SET reason=''",
            "DROP TABLE tag_vocabulary_runs",
            "DROP TABLE tag_vocabulary_rollbacks",
        ] {
            let (conn, id) = setup();
            record_for_test(&conn, &id);
            conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
            conn.execute_batch(alteration).unwrap();
            assert!(verify_rollback_integrity(&conn).is_err());
        }
    }
}
