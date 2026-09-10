//! 語彙一括変更の端末ローカル台帳。厳密な復元用の原文はDBだけへ保存する。
//!
//! Gitの内容履歴とは役割が異なるため、logical snapshotやfresh cloneへ台帳を運ばない。
//! 通常の履歴読取は要約だけを返し、現時点で非参照のノートも公開しない。

use std::collections::BTreeSet;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::frontmatter::Note;
use crate::note_id::NoteId;

mod rollback;
pub use rollback::RollbackRecord;
#[cfg(test)]
pub(crate) use rollback::record_for_test as record_rollback_for_test;
pub(crate) use rollback::{
    ROLLBACK_SCHEMA_SQL, ROLLBACK_TABLES, read_for_rollback, record_rollback,
    rollback_table_present, verify_rollback_integrity, verify_rollback_schema,
};

pub(crate) const TABLES: [&str; 2] = ["tag_vocabulary_runs", "tag_vocabulary_run_notes"];
pub(crate) const SCHEMA_SQL: &str = "
CREATE TABLE tag_vocabulary_runs(
    execution_id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL,
    source_note_uid TEXT NOT NULL,
    source_revision TEXT NOT NULL,
    plan_hash TEXT NOT NULL,
    operations_json TEXT NOT NULL,
    reason TEXT NOT NULL,
    client TEXT NOT NULL,
    applied_at TEXT NOT NULL,
    changed_notes INTEGER NOT NULL CHECK(changed_notes > 0)
);
CREATE TABLE tag_vocabulary_run_notes(
    execution_id TEXT NOT NULL REFERENCES tag_vocabulary_runs(execution_id),
    note_id TEXT NOT NULL,
    note_uid TEXT,
    before_hash TEXT NOT NULL,
    after_hash TEXT NOT NULL,
    before_tags TEXT NOT NULL,
    after_tags TEXT NOT NULL,
    before_document TEXT NOT NULL,
    after_document TEXT NOT NULL,
    PRIMARY KEY(execution_id,note_id)
);";

#[derive(Debug, Clone)]
pub(crate) struct HistoryRun {
    pub execution_id: String,
    pub workspace_id: String,
    pub source_note_uid: String,
    pub source_revision: String,
    pub plan_hash: String,
    pub operations_json: String,
    pub reason: String,
    pub client: String,
    pub applied_at: String,
    pub changed_notes: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct HistoryNote {
    pub note_id: String,
    pub note_uid: Option<String>,
    pub before_document: String,
    pub after_document: String,
}

#[derive(Debug, Serialize)]
pub struct HistorySummary {
    pub execution_id: String,
    pub workspace_id: String,
    pub source_note_uid: String,
    pub source_revision: String,
    pub plan_hash: String,
    pub operations: serde_json::Value,
    pub reason: String,
    pub client: String,
    pub applied_at: String,
    pub changed_notes: usize,
    pub rollback: Option<RollbackRecord>,
}

#[derive(Debug, Serialize)]
pub struct HistoryNoteSummary {
    pub note_id: String,
    pub note_uid: Option<String>,
    pub before_hash: String,
    pub after_hash: String,
    pub before_tags: Vec<String>,
    pub after_tags: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct HistoryPage {
    pub items: Vec<HistorySummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HistoryDetail {
    pub run: HistorySummary,
    pub notes: Vec<HistoryNoteSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NotesCursor {
    execution_id: String,
    after: String,
}

fn validate_line(value: &str, maximum: usize) -> Result<()> {
    ensure!(
        !value.trim().is_empty()
            && value.chars().count() <= maximum
            && !value.chars().any(char::is_control),
        "語彙変更履歴の値は空でない制御文字なしの一行にする"
    );
    Ok(())
}

fn validate_run(run: &HistoryRun) -> Result<serde_json::Value> {
    for value in [
        &run.execution_id,
        &run.workspace_id,
        &run.source_note_uid,
        &run.source_revision,
    ] {
        ensure!(crate::artifact::is_ulid(value), "語彙変更履歴のIDが不正");
    }
    validate_line(&run.plan_hash, 200)?;
    validate_line(&run.reason, 2000)?;
    validate_line(&run.client, 200)?;
    time::OffsetDateTime::parse(
        &run.applied_at,
        &time::format_description::well_known::Rfc3339,
    )
    .context("語彙変更履歴の実行時刻が不正")?;
    ensure!(run.changed_notes > 0, "語彙変更履歴の変更件数がない");
    let operations: crate::tag_vocabulary_changes::Changes =
        serde_json::from_str(&run.operations_json).context("語彙変更履歴の操作JSONが不正")?;
    Ok(serde_json::to_value(operations)?)
}

fn document_hash(document: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(document.as_bytes()))
}

fn note_summary(note: &HistoryNote) -> Result<HistoryNoteSummary> {
    NoteId::parse(&note.note_id)?;
    let before = Note::parse(&note.before_document).context("語彙変更履歴の変更前原文が不正")?;
    let after = Note::parse(&note.after_document).context("語彙変更履歴の変更後原文が不正")?;
    for document in [&before, &after] {
        ensure!(
            document.front.note_uid.as_ref().map(|uid| uid.as_str()) == note.note_uid.as_deref(),
            "語彙変更履歴の原文と対象UIDが一致しない"
        );
    }
    ensure!(
        note.before_document != note.after_document,
        "語彙変更履歴には変更されたノートだけを保存する"
    );
    Ok(HistoryNoteSummary {
        note_id: note.note_id.clone(),
        note_uid: note.note_uid.clone(),
        before_hash: document_hash(&note.before_document),
        after_hash: document_hash(&note.after_document),
        before_tags: before.front.tags,
        after_tags: after.front.tags,
    })
}

/// 呼出側が全ノート/outboxと同じtransactionを所有する。保存失敗は呼出側へ返す。
pub(crate) fn record(conn: &Connection, run: &HistoryRun, notes: &[HistoryNote]) -> Result<()> {
    ensure!(
        !conn.is_autocommit(),
        "語彙変更履歴の保存にはtransactionが必要"
    );
    validate_run(run)?;
    ensure!(
        run.changed_notes == notes.len(),
        "語彙変更履歴の変更件数が一致しない"
    );
    let mut ids = BTreeSet::new();
    let mut uids = BTreeSet::new();
    let summaries = notes
        .iter()
        .map(|note| {
            ensure!(
                ids.insert(&note.note_id),
                "語彙変更履歴のノートIDが重複している"
            );
            if let Some(uid) = &note.note_uid {
                ensure!(uids.insert(uid), "語彙変更履歴のUIDが重複している");
            }
            note_summary(note)
        })
        .collect::<Result<Vec<_>>>()?;
    conn.execute(
        "INSERT INTO tag_vocabulary_runs(execution_id,workspace_id,source_note_uid,source_revision,plan_hash,operations_json,reason,client,applied_at,changed_notes) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![run.execution_id,run.workspace_id,run.source_note_uid,run.source_revision,run.plan_hash,run.operations_json,run.reason,run.client,run.applied_at,i64::try_from(run.changed_notes)?],
    )?;
    let mut insert = conn.prepare("INSERT INTO tag_vocabulary_run_notes(execution_id,note_id,note_uid,before_hash,after_hash,before_tags,after_tags,before_document,after_document) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)")?;
    for (note, summary) in notes.iter().zip(summaries) {
        insert.execute(params![
            run.execution_id,
            note.note_id,
            note.note_uid,
            summary.before_hash,
            summary.after_hash,
            serde_json::to_string(&summary.before_tags)?,
            serde_json::to_string(&summary.after_tags)?,
            note.before_document,
            note.after_document
        ])?;
    }
    Ok(())
}

fn page_limit(limit: Option<usize>) -> Result<usize> {
    let limit = limit.unwrap_or(20);
    ensure!(
        (1..=100).contains(&limit),
        "語彙変更履歴のlimitは1〜100にする"
    );
    Ok(limit)
}

fn read_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryRun> {
    Ok(HistoryRun {
        execution_id: row.get(0)?,
        workspace_id: row.get(1)?,
        source_note_uid: row.get(2)?,
        source_revision: row.get(3)?,
        plan_hash: row.get(4)?,
        operations_json: row.get(5)?,
        reason: row.get(6)?,
        client: row.get(7)?,
        applied_at: row.get(8)?,
        changed_notes: usize::try_from(row.get::<_, i64>(9)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                9,
                rusqlite::types::Type::Integer,
                Box::new(error),
            )
        })?,
    })
}

fn summary(conn: &Connection, run: HistoryRun) -> Result<HistorySummary> {
    let operations = validate_run(&run)?;
    let rollback = rollback::read_record(conn, &run.execution_id)?;
    Ok(HistorySummary {
        execution_id: run.execution_id,
        workspace_id: run.workspace_id,
        source_note_uid: run.source_note_uid,
        source_revision: run.source_revision,
        plan_hash: run.plan_hash,
        operations,
        reason: run.reason,
        client: run.client,
        applied_at: run.applied_at,
        changed_notes: run.changed_notes,
        rollback,
    })
}

pub fn list(conn: &Connection, limit: Option<usize>, cursor: Option<&str>) -> Result<HistoryPage> {
    if conn.is_autocommit() {
        let transaction = conn.unchecked_transaction()?;
        let result = list(&transaction, limit, cursor)?;
        transaction.commit()?;
        return Ok(result);
    }
    let limit = page_limit(limit)?;
    let before = cursor
        .map(|value| -> Result<i64> {
            let value = value
                .strip_prefix("tag-history-v1:")
                .context("語彙変更履歴のcursorが不正")?;
            let before: i64 = value.parse().context("語彙変更履歴のcursorが不正")?;
            ensure!(before > 0, "語彙変更履歴のcursorが不正");
            Ok(before)
        })
        .transpose()?
        .unwrap_or(i64::MAX);
    let mut statement = conn.prepare("SELECT execution_id,workspace_id,source_note_uid,source_revision,plan_hash,operations_json,reason,client,applied_at,changed_notes,rowid FROM tag_vocabulary_runs WHERE rowid < ?1 ORDER BY rowid DESC LIMIT ?2")?;
    let mut rows = statement
        .query_map(params![before, i64::try_from(limit + 1)?], |row| {
            Ok((read_run(row)?, row.get::<_, i64>(10)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = if truncated {
        rows.last()
            .map(|(_, rowid)| format!("tag-history-v1:{rowid}"))
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(|(run, _)| summary(conn, run))
        .collect::<Result<_>>()?;
    Ok(HistoryPage { items, next_cursor })
}

pub fn get(
    conn: &Connection,
    execution_id: &str,
    limit: Option<usize>,
    cursor: Option<&str>,
) -> Result<Option<HistoryDetail>> {
    ensure!(
        crate::artifact::is_ulid(execution_id),
        "語彙変更履歴の実行IDが不正"
    );
    let limit = page_limit(limit)?;
    let after = if let Some(cursor) = cursor {
        ensure!(cursor.len() <= 8192, "語彙変更履歴のcursorが長すぎる");
        let cursor: NotesCursor =
            serde_json::from_str(cursor).context("語彙変更履歴のcursorが不正")?;
        ensure!(
            cursor.execution_id == execution_id,
            "語彙変更履歴のcursorが別の実行を指している"
        );
        NoteId::parse(&cursor.after)?;
        cursor.after
    } else {
        String::new()
    };
    // 要約と現ノートの参照可否を、同じ読取snapshotに固定する。
    if conn.is_autocommit() {
        let transaction = conn.unchecked_transaction()?;
        let result = get(&transaction, execution_id, Some(limit), cursor)?;
        transaction.commit()?;
        return Ok(result);
    }
    let run = conn.query_row("SELECT execution_id,workspace_id,source_note_uid,source_revision,plan_hash,operations_json,reason,client,applied_at,changed_notes FROM tag_vocabulary_runs WHERE execution_id=?1",[execution_id],read_run).optional()?;
    let Some(run) = run else { return Ok(None) };
    // UIDによる改名追跡と元IDの再利用の両方で、現時点の非参照ノートを除外する。
    let mut statement = conn.prepare("SELECT h.note_id,h.note_uid,h.before_hash,h.after_hash,h.before_tags,h.after_tags FROM tag_vocabulary_run_notes h WHERE h.execution_id=?1 AND h.note_id>?2 AND NOT EXISTS (SELECT 1 FROM notes n WHERE (n.id=h.note_id OR (h.note_uid IS NOT NULL AND n.note_uid=h.note_uid)) AND n.normal_reference_allowed=0) ORDER BY h.note_id LIMIT ?3")?;
    let mut notes = statement
        .query_map(
            params![execution_id, after, i64::try_from(limit + 1)?],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )?
        .map(|row| -> Result<HistoryNoteSummary> {
            let (note_id, note_uid, before_hash, after_hash, before_tags, after_tags) = row?;
            NoteId::parse(&note_id)?;
            Ok(HistoryNoteSummary {
                note_id,
                note_uid,
                before_hash,
                after_hash,
                before_tags: serde_json::from_str(&before_tags)?,
                after_tags: serde_json::from_str(&after_tags)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let truncated = notes.len() > limit;
    notes.truncate(limit);
    let next_cursor = if truncated {
        notes
            .last()
            .map(|note| {
                serde_json::to_string(&NotesCursor {
                    execution_id: execution_id.into(),
                    after: note.note_id.clone(),
                })
            })
            .transpose()?
    } else {
        None
    };
    Ok(Some(HistoryDetail {
        run: summary(conn, run)?,
        notes,
        next_cursor,
    }))
}

pub(crate) fn tables_present(conn: &Connection) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN (?1,?2)",
        TABLES,
        |row| row.get(0),
    )?;
    ensure!(
        count != 1,
        "語彙変更履歴のdurable tableが片方だけ欠けている"
    );
    Ok(count == 2)
}

/// 通常openで原文全件を読み直さず、durable tableの制約だけを確かめる。
pub(crate) fn verify_schema(conn: &Connection) -> Result<()> {
    for table in TABLES {
        let expected = SCHEMA_SQL
            .split(';')
            .find(|sql| {
                sql.trim_start()
                    .starts_with(&format!("CREATE TABLE {table}("))
            })
            .context("語彙変更履歴のDDL定義がない")?;
        let actual: String = conn.query_row(
            "SELECT sql FROM sqlite_schema WHERE type='table' AND name=?1",
            [table],
            |row| row.get(0),
        )?;
        let normalize = |sql: &str| {
            sql.chars()
                .filter(|character| !character.is_whitespace())
                .collect::<String>()
                .to_ascii_lowercase()
        };
        ensure!(
            normalize(expected) == normalize(&actual),
            "語彙変更履歴のdurable table制約が不正"
        );
    }
    Ok(())
}

/// 診断・限定復旧だけが原文とhashまで検証する。本文はエラーへ埋め込まない。
pub(crate) fn verify_integrity(conn: &Connection) -> Result<()> {
    verify_schema(conn)?;
    let mut runs=conn.prepare("SELECT execution_id,workspace_id,source_note_uid,source_revision,plan_hash,operations_json,reason,client,applied_at,changed_notes FROM tag_vocabulary_runs")?;
    for run in runs.query_map([], read_run)? {
        let run = run?;
        validate_run(&run)?;
        let count: i64 = conn.query_row(
            "SELECT count(*) FROM tag_vocabulary_run_notes WHERE execution_id=?1",
            [&run.execution_id],
            |row| row.get(0),
        )?;
        ensure!(
            count == i64::try_from(run.changed_notes)?,
            "語彙変更履歴の変更件数が一致しない"
        );
    }
    let orphan:i64=conn.query_row("SELECT count(*) FROM tag_vocabulary_run_notes h WHERE NOT EXISTS(SELECT 1 FROM tag_vocabulary_runs r WHERE r.execution_id=h.execution_id)",[],|row| row.get(0))?;
    ensure!(orphan == 0, "語彙変更履歴の実行への参照がない");
    let mut rows=conn.prepare("SELECT note_id,note_uid,before_document,after_document,before_hash,after_hash,before_tags,after_tags FROM tag_vocabulary_run_notes")?;
    for row in rows.query_map([], |row| {
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
        let summary = note_summary(&note)?;
        ensure!(
            summary.before_hash == before_hash
                && summary.after_hash == after_hash
                && summary.before_tags == serde_json::from_str::<Vec<String>>(&before_tags)?
                && summary.after_tags == serde_json::from_str::<Vec<String>>(&after_tags)?,
            "語彙変更履歴の原文と要約が一致しない"
        );
    }
    Ok(())
}

pub(crate) fn initialize_recovery_schema(conn: &Connection) -> Result<()> {
    ensure!(
        !conn.is_autocommit(),
        "語彙変更履歴の復旧準備にはtransactionが必要"
    );
    if !tables_present(conn)? {
        let declared: String =
            conn.query_row("SELECT value FROM meta WHERE key='schema'", [], |row| {
                row.get(0)
            })?;
        ensure!(
            matches!(declared.as_str(), "7" | "9"),
            "語彙変更履歴の不在を補える限定復旧schemaではない"
        );
        conn.execute_batch(SCHEMA_SQL)?;
    }
    verify_schema(conn)?;
    rollback::initialize_recovery_schema(conn)
}

#[cfg(test)]
pub(crate) fn record_for_test(conn: &Connection) -> String {
    let mut front = crate::frontmatter::Frontmatter::new_note("履歴fixture");
    front.tags = vec!["test".into()];
    let before = Note {
        front: front.clone(),
        body: "private original content\n".into(),
    }
    .to_file_string()
    .unwrap();
    front.tags = vec!["changed".into()];
    let after = Note {
        front,
        body: "private original content\n".into(),
    }
    .to_file_string()
    .unwrap();
    let run = HistoryRun {
        execution_id: crate::authority::NoteUid::new().to_string(),
        workspace_id: crate::authority::NoteUid::new().to_string(),
        source_note_uid: crate::authority::NoteUid::new().to_string(),
        source_revision: crate::authority::NoteUid::new().to_string(),
        plan_hash: format!("sha256:{}", "a".repeat(64)),
        operations_json: r#"{"upsert":{},"remove":["test"],"replace":{"test":"changed"}}"#.into(),
        reason: "語彙を揃える".into(),
        client: "test/client".into(),
        applied_at: crate::frontmatter::now_iso(),
        changed_notes: 1,
    };
    let transaction = conn.unchecked_transaction().unwrap();
    record(
        &transaction,
        &run,
        &[HistoryNote {
            note_id: "notes/history".into(),
            note_uid: None,
            before_document: before,
            after_document: after,
        }],
    )
    .unwrap();
    transaction.commit().unwrap();
    run.execution_id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA_SQL).unwrap();
        conn.execute_batch(ROLLBACK_SCHEMA_SQL).unwrap();
        conn.execute_batch("CREATE TABLE notes(id TEXT PRIMARY KEY,note_uid TEXT,normal_reference_allowed INTEGER NOT NULL)").unwrap();
        conn
    }

    fn run_and_notes(conn: &Connection) -> (HistoryRun, Vec<HistoryNote>) {
        let id = record_for_test(conn);
        let run=conn.query_row("SELECT execution_id,workspace_id,source_note_uid,source_revision,plan_hash,operations_json,reason,client,applied_at,changed_notes FROM tag_vocabulary_runs WHERE execution_id=?1",[&id],read_run).unwrap();
        let note=conn.query_row("SELECT note_id,note_uid,before_document,after_document FROM tag_vocabulary_run_notes WHERE execution_id=?1",[id],|row| Ok(HistoryNote {note_id:row.get(0)?,note_uid:row.get(1)?,before_document:row.get(2)?,after_document:row.get(3)?})).unwrap();
        (run, vec![note])
    }

    #[test]
    fn original_bytes_stay_in_database_and_read_apis_return_only_metadata() {
        let conn = setup();
        let id = record_for_test(&conn);
        verify_integrity(&conn).unwrap();
        let detail = get(&conn, &id, None, None).unwrap().unwrap();
        assert_eq!(detail.notes[0].before_tags, vec!["test"]);
        assert_eq!(detail.notes[0].after_tags, vec!["changed"]);
        let output = serde_json::to_string(&detail).unwrap();
        assert!(!output.contains("private original content"));
        assert!(!output.contains("before_document"));
        assert!(
            !serde_json::to_string(&list(&conn, None, None).unwrap())
                .unwrap()
                .contains("private original content")
        );
        let original: String = conn
            .query_row(
                "SELECT before_document FROM tag_vocabulary_run_notes",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(original.contains("private original content"));
        assert_eq!(detail.notes[0].before_hash, document_hash(&original));
    }

    #[test]
    fn record_failure_rolls_back_with_its_callers_other_writes() {
        let conn = setup();
        let (mut run, mut notes) = run_and_notes(&conn);
        run.execution_id = crate::authority::NoteUid::new().to_string();
        let mut second = notes[0].clone();
        second.note_id = "notes/fail".into();
        notes.push(second);
        run.changed_notes = 2;
        assert!(record(&conn, &run, &notes).is_err());
        conn.execute_batch("CREATE TRIGGER history_failure BEFORE INSERT ON tag_vocabulary_run_notes WHEN NEW.note_id='notes/fail' BEGIN SELECT RAISE(ABORT,'failure fixture'); END").unwrap();
        {
            let transaction = conn.unchecked_transaction().unwrap();
            transaction
                .execute("INSERT INTO notes VALUES('notes/transient',NULL,1)", [])
                .unwrap();
            assert!(record(&transaction, &run, &notes).is_err());
        }
        assert_eq!(
            conn.query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row("SELECT count(*) FROM tag_vocabulary_runs", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("SELECT count(*) FROM tag_vocabulary_run_notes", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            1
        );
    }

    #[test]
    fn duplicate_execution_and_invalid_note_identity_are_rejected() {
        let conn = setup();
        let (run, notes) = run_and_notes(&conn);
        let transaction = conn.unchecked_transaction().unwrap();
        assert!(record(&transaction, &run, &notes).is_err());
        drop(transaction);
        for mismatch in [true, false] {
            let mut run = run.clone();
            run.execution_id = crate::authority::NoteUid::new().to_string();
            let mut notes = notes.clone();
            if mismatch {
                notes[0].note_uid = Some(crate::authority::NoteUid::new().to_string());
            } else {
                notes[0].before_document = notes[0].after_document.clone();
            }
            let transaction = conn.unchecked_transaction().unwrap();
            assert!(record(&transaction, &run, &notes).is_err());
        }
        assert_eq!(list(&conn, None, None).unwrap().items.len(), 1);
    }

    #[test]
    fn pages_have_bounded_sizes_and_cursors_do_not_cross_executions() {
        let conn = setup();
        for _ in 0..23 {
            record_for_test(&conn);
        }
        let first = list(&conn, None, None).unwrap();
        assert_eq!(first.items.len(), 20);
        let second = list(&conn, None, first.next_cursor.as_deref()).unwrap();
        assert_eq!(second.items.len(), 3);
        assert!(second.next_cursor.is_none());
        assert!(!second.items.iter().any(|item| {
            first
                .items
                .iter()
                .any(|old| old.execution_id == item.execution_id)
        }));
        let (mut run, notes) = run_and_notes(&conn);
        run.execution_id = crate::authority::NoteUid::new().to_string();
        run.changed_notes = 23;
        let notes: Vec<_> = (0..23)
            .map(|index| {
                let mut note = notes[0].clone();
                note.note_id = format!("notes/item-{index:02}");
                note
            })
            .collect();
        let transaction = conn.unchecked_transaction().unwrap();
        record(&transaction, &run, &notes).unwrap();
        transaction.commit().unwrap();
        let page = get(&conn, &run.execution_id, None, None).unwrap().unwrap();
        assert_eq!(page.notes.len(), 20);
        let next = get(&conn, &run.execution_id, None, page.next_cursor.as_deref())
            .unwrap()
            .unwrap();
        assert_eq!(next.notes.len(), 3);
        assert!(next.next_cursor.is_none());
        assert!(
            get(
                &conn,
                &first.items[0].execution_id,
                None,
                page.next_cursor.as_deref()
            )
            .is_err()
        );
        for limit in [0, 101] {
            assert!(list(&conn, Some(limit), None).is_err());
            assert!(get(&conn, &run.execution_id, Some(limit), None).is_err());
        }
        assert!(list(&conn, None, Some("-1")).is_err());
        assert!(get(&conn, "bad-id", None, None).is_err());
    }

    /// 2026-09-08: 一括変更後に非参照となったノートの過去metadataもreadで漏らさない。
    #[test]
    fn subsequently_hidden_notes_are_filtered_by_original_id_and_stable_uid() {
        let conn = setup();
        let id = record_for_test(&conn);
        conn.execute("INSERT INTO notes VALUES('notes/history',NULL,0)", [])
            .unwrap();
        let hidden = get(&conn, &id, None, None).unwrap().unwrap();
        assert_eq!(hidden.run.changed_notes, 1);
        assert!(hidden.notes.is_empty());
        conn.execute("UPDATE notes SET normal_reference_allowed=1", [])
            .unwrap();
        assert_eq!(get(&conn, &id, None, None).unwrap().unwrap().notes.len(), 1);
        let (mut run, mut notes) = run_and_notes(&conn);
        run.execution_id = crate::authority::NoteUid::new().to_string();
        let uid = crate::authority::NoteUid::new();
        notes[0].note_uid = Some(uid.to_string());
        let attach_uid = |raw: &str| {
            let mut note = Note::parse(raw).unwrap();
            note.front.note_uid = Some(uid.clone());
            note.front.authority = Some(crate::authority::Authority {
                namespace: crate::authority::NoteNamespace::Records,
                role: crate::authority::AuthorityRole::Record,
                status: crate::authority::AuthorityStatus::Active,
                scope: "history-fixture".into(),
            });
            note.to_file_string().unwrap()
        };
        notes[0].before_document = attach_uid(&notes[0].before_document);
        notes[0].after_document = attach_uid(&notes[0].after_document);
        let transaction = conn.unchecked_transaction().unwrap();
        record(&transaction, &run, &notes).unwrap();
        transaction.commit().unwrap();
        verify_integrity(&conn).unwrap();
        conn.execute(
            "UPDATE notes SET id='notes/renamed',note_uid=?1,normal_reference_allowed=0",
            [uid.as_str()],
        )
        .unwrap();
        assert!(
            get(&conn, &run.execution_id, None, None)
                .unwrap()
                .unwrap()
                .notes
                .is_empty()
        );
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM tag_vocabulary_run_notes WHERE execution_id=?1",
                [&run.execution_id],
                |row| { row.get::<_, i64>(0) }
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn integrity_rejects_missing_notes_tampered_documents_and_partial_schema() {
        for alteration in [
            "DELETE FROM tag_vocabulary_run_notes",
            "UPDATE tag_vocabulary_run_notes SET before_document='broken private content'",
            "UPDATE tag_vocabulary_run_notes SET after_hash='wrong'",
            "UPDATE tag_vocabulary_runs SET operations_json='null'",
            "UPDATE tag_vocabulary_runs SET operations_json='[]'",
            "UPDATE tag_vocabulary_runs SET operations_json='{\"upsert\":{},\"remove\":[]}'",
            "UPDATE tag_vocabulary_runs SET operations_json='{\"upsert\":{},\"remove\":[],\"replace\":{},\"unknown\":true}'",
            "DELETE FROM tag_vocabulary_runs",
        ] {
            let conn = setup();
            record_for_test(&conn);
            conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
            conn.execute_batch(alteration).unwrap();
            assert!(verify_integrity(&conn).is_err());
        }
        let conn = setup();
        conn.execute_batch("DROP TABLE tag_vocabulary_run_notes")
            .unwrap();
        assert!(tables_present(&conn).is_err());
        assert!(verify_schema(&conn).is_err());
    }

    #[test]
    fn local_history_survives_import_but_does_not_change_snapshot_or_travel_with_clone() {
        let dir = tempfile::tempdir().unwrap();
        let vault = crate::vault::Vault::create(dir.path().join("source")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let snapshot = crate::storage_contract::export(&vault).unwrap();
        let id = record_for_test(&conn);
        let rollback_id = record_rollback_for_test(&conn, &id);
        assert_eq!(
            snapshot.digest,
            crate::storage_contract::export(&vault).unwrap().digest
        );
        let imported = crate::index::import_markdown_snapshot(&vault, &conn).unwrap();
        assert!(imported.degraded.is_empty());
        assert_eq!(
            get(&conn, &id, None, None)
                .unwrap()
                .unwrap()
                .run
                .rollback
                .unwrap()
                .rollback_id,
            rollback_id
        );
        let clone_path = dir.path().join("clone");
        let status = std::process::Command::new("git")
            .arg("clone")
            .arg("--quiet")
            .arg(&vault.root)
            .arg(&clone_path)
            .status()
            .unwrap();
        assert!(status.success());
        let cloned = crate::vault::Vault::open(clone_path).unwrap();
        assert_eq!(
            snapshot.digest,
            crate::storage_contract::export(&cloned).unwrap().digest
        );
        let clone_db = crate::index::open_db(&cloned).unwrap();
        assert!(list(&clone_db, None, None).unwrap().items.is_empty());
    }
}
