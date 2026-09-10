//! ローカル台帳へ確定した語彙操作だけを集計する。拒否や意味判断の品質を成功率に読み替えない。

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

// 運用の近況を固定サイズで返し、過去の原文・理由・操作JSONを毎回読み直さない。
const RECENT_LIMIT: usize = 10;
const RECORDED_RUNS: &str = "
SELECT execution_id,'apply' AS kind,NULL AS target_execution_id,applied_at AS recorded_at,changed_notes
FROM tag_vocabulary_runs
UNION ALL
SELECT rollback_id,'rollback',execution_id,restored_at,restored_notes
FROM tag_vocabulary_rollbacks";

#[derive(Debug, Serialize)]
pub struct RecentRun {
    pub execution_id: String,
    pub kind: String,
    pub target_execution_id: Option<String>,
    pub recorded_at: String,
    pub changed_notes: usize,
}

#[derive(Debug, Serialize)]
pub struct Stats {
    pub measurement_scope: &'static str,
    pub apply_runs: usize,
    pub rollback_runs: usize,
    pub applied_note_changes: usize,
    pub restored_note_changes: usize,
    pub first_recorded_at: Option<String>,
    pub last_recorded_at: Option<String>,
    pub recent: Vec<RecentRun>,
    pub pending_exports_total: usize,
    pub tag_vocabulary_pending_exports: usize,
    pub unmeasured: [&'static str; 4],
}

fn count(value: i64) -> Result<usize> {
    usize::try_from(value).context("語彙運用の集計件数が範囲外")
}

fn validate_time(value: &str) -> Result<()> {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .context("語彙運用の記録時刻が不正")?;
    Ok(())
}

/// 準備済みDBの同じsnapshotから読む。同期・修復・計測用の追加書込みは行わない。
pub fn read(conn: &Connection) -> Result<Stats> {
    if conn.is_autocommit() {
        let transaction = conn.unchecked_transaction()?;
        let result = read(&transaction)?;
        transaction.commit()?;
        return Ok(result);
    }
    let (apply_runs, applied_note_changes): (i64, i64) = conn.query_row(
        "SELECT count(*),coalesce(sum(changed_notes),0) FROM tag_vocabulary_runs",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let (rollback_runs, restored_note_changes): (i64, i64) = conn.query_row(
        "SELECT count(*),coalesce(sum(restored_notes),0) FROM tag_vocabulary_rollbacks",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let first_recorded_at: Option<String> = conn
        .query_row(
            &format!(
                "SELECT recorded_at FROM ({RECORDED_RUNS}) ORDER BY julianday(recorded_at),recorded_at,kind,execution_id LIMIT 1"
            ),
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(value) = &first_recorded_at {
        validate_time(value)?;
    }
    let mut statement = conn.prepare(&format!(
        "SELECT execution_id,kind,target_execution_id,recorded_at,changed_notes FROM ({RECORDED_RUNS}) ORDER BY julianday(recorded_at) DESC,recorded_at DESC,kind DESC,execution_id DESC LIMIT ?1"
    ))?;
    let rows = statement.query_map([i64::try_from(RECENT_LIMIT)?], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;
    let mut recent = Vec::with_capacity(RECENT_LIMIT);
    for row in rows {
        let (execution_id, kind, target_execution_id, recorded_at, changed_notes) = row?;
        validate_time(&recorded_at)?;
        recent.push(RecentRun {
            execution_id,
            kind,
            target_execution_id,
            recorded_at,
            changed_notes: count(changed_notes)?,
        });
    }
    let (pending_exports_total, tag_vocabulary_pending_exports): (i64, i64) = conn.query_row(
        "SELECT count(*),coalesce(sum(CASE WHEN op_id LIKE 'tag-vocabulary:%' THEN 1 ELSE 0 END),0) FROM note_exports",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(Stats {
        measurement_scope: "local_committed_history_all_time",
        apply_runs: count(apply_runs)?,
        rollback_runs: count(rollback_runs)?,
        applied_note_changes: count(applied_note_changes)?,
        restored_note_changes: count(restored_note_changes)?,
        first_recorded_at,
        last_recorded_at: recent.first().map(|run| run.recorded_at.clone()),
        recent,
        pending_exports_total: count(pending_exports_total)?,
        tag_vocabulary_pending_exports: count(tag_vocabulary_pending_exports)?,
        unmeasured: [
            "plan_attempts",
            "rejected_changes",
            "execution_duration",
            "semantic_quality",
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn setup(conn: &Connection) {
        // 原文・操作JSONのないDBでも集計できることを検査する。
        conn.execute_batch(
            "CREATE TABLE tag_vocabulary_runs(execution_id TEXT,applied_at TEXT,changed_notes INTEGER);
             CREATE TABLE tag_vocabulary_rollbacks(rollback_id TEXT,execution_id TEXT,restored_at TEXT,restored_notes INTEGER);
             CREATE TABLE note_exports(op_id TEXT);",
        )
        .unwrap();
    }

    #[test]
    fn empty_history_is_zero_with_explicit_measurement_limits() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        let stats = read(&conn).unwrap();
        assert_eq!(stats.apply_runs, 0);
        assert_eq!(stats.rollback_runs, 0);
        assert_eq!(stats.applied_note_changes, 0);
        assert_eq!(stats.restored_note_changes, 0);
        assert_eq!(stats.pending_exports_total, 0);
        assert_eq!(stats.tag_vocabulary_pending_exports, 0);
        assert_eq!(stats.first_recorded_at, None);
        assert_eq!(stats.last_recorded_at, None);
        assert!(stats.recent.is_empty());
        assert_eq!(stats.measurement_scope, "local_committed_history_all_time");
        assert!(stats.unmeasured.contains(&"rejected_changes"));
        assert!(stats.unmeasured.contains(&"semantic_quality"));
        assert!(conn.is_autocommit());
    }

    #[test]
    fn committed_counts_and_pending_rows_are_read_only_and_do_not_subtract_rollbacks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.sqlite3");
        let conn = Connection::open(&path).unwrap();
        setup(&conn);
        conn.execute_batch(
            "INSERT INTO tag_vocabulary_runs VALUES('a','2026-09-08T09:00:00+09:00',10001),('b','2026-09-08T01:00:00Z',4);
             INSERT INTO tag_vocabulary_rollbacks VALUES('r','a','2026-09-08T02:00:00Z',10001);
             INSERT INTO note_exports VALUES('ordinary'),('tag-vocabulary:b:0'),('tag-vocabulary:r:0');",
        )
        .unwrap();
        drop(conn);
        let conn =
            Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let before = std::fs::read(&path).unwrap();
        let stats = read(&conn).unwrap();
        assert_eq!(stats.apply_runs, 2);
        assert_eq!(stats.rollback_runs, 1);
        assert_eq!(stats.applied_note_changes, 10005);
        assert_eq!(stats.restored_note_changes, 10001);
        assert_eq!(stats.pending_exports_total, 3);
        assert_eq!(stats.tag_vocabulary_pending_exports, 2);
        assert_eq!(
            stats.first_recorded_at.as_deref(),
            Some("2026-09-08T09:00:00+09:00")
        );
        assert_eq!(
            stats.last_recorded_at.as_deref(),
            Some("2026-09-08T02:00:00Z")
        );
        assert_eq!(stats.recent[0].execution_id, "r");
        assert_eq!(stats.recent[0].kind, "rollback");
        assert_eq!(stats.recent[0].target_execution_id.as_deref(), Some("a"));
        assert_eq!(stats.recent[1].execution_id, "b");
        assert_eq!(stats.recent[1].target_execution_id, None);
        assert_eq!(stats.recent[2].execution_id, "a");
        assert_eq!(before, std::fs::read(&path).unwrap());
    }

    #[test]
    fn recent_runs_are_bounded_and_stable_for_equal_times() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        for index in 0..16 {
            conn.execute(
                "INSERT INTO tag_vocabulary_runs VALUES(?1,'2026-09-08T00:00:00Z',1)",
                [format!("apply-{index:02}")],
            )
            .unwrap();
        }
        let stats = read(&conn).unwrap();
        assert_eq!(stats.apply_runs, 16);
        assert_eq!(stats.recent.len(), RECENT_LIMIT);
        assert_eq!(stats.recent[0].execution_id, "apply-15");
        assert_eq!(stats.recent[9].execution_id, "apply-06");
        assert_eq!(
            serde_json::to_value(&stats).unwrap(),
            serde_json::to_value(read(&conn).unwrap()).unwrap()
        );
    }

    #[test]
    fn missing_history_and_invalid_counts_or_times_are_not_reported_as_zero() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(read(&conn).is_err());
        setup(&conn);
        for (at, changed_notes) in [("2026-09-08T00:00:00Z", -1), ("invalid", 1)] {
            conn.execute("DELETE FROM tag_vocabulary_runs", []).unwrap();
            conn.execute(
                "INSERT INTO tag_vocabulary_runs VALUES('a',?1,?2)",
                params![at, changed_notes],
            )
            .unwrap();
            assert!(read(&conn).is_err());
        }
    }
}
