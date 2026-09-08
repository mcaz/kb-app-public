//! ノート数の実測を端末ローカルに残す。履歴は現在のnotesから再導出できないため、
//! 再構築されるindex.db・Git同期から分離し、欠測日の補完はしない。

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};

use crate::vault::Vault;

mod trend;
pub use trend::{NoteCountTrend, NoteCountTrendDay, NoteCountTrendStatus, read};

const APPLICATION_ID: i64 = 0x4b42_4e43;
const SCHEMA_VERSION: i64 = 1;
const TABLE_SQL: &str = "CREATE TABLE daily_note_counts (
    workspace_id TEXT NOT NULL,
    local_date TEXT NOT NULL,
    observed_at_ms INTEGER NOT NULL CHECK(typeof(observed_at_ms) = 'integer' AND observed_at_ms >= 0),
    utc_offset_seconds INTEGER NOT NULL CHECK(typeof(utc_offset_seconds) = 'integer' AND utc_offset_seconds BETWEEN -86399 AND 86399),
    total INTEGER NOT NULL CHECK(typeof(total) = 'integer' AND total >= 0),
    deprecated INTEGER NOT NULL CHECK(typeof(deprecated) = 'integer' AND deprecated BETWEEN 0 AND total),
    PRIMARY KEY(workspace_id, local_date)
) WITHOUT ROWID";

#[derive(Clone, Debug)]
struct Observation {
    workspace_id: String,
    local_date: String,
    observed_at_ms: i64,
    utc_offset_seconds: i32,
    total: i64,
    deprecated: i64,
}

impl Observation {
    fn validate(&self) -> Result<()> {
        ensure!(
            crate::artifact::is_ulid(&self.workspace_id),
            "観測先IDが不正"
        );
        ensure!(self.observed_at_ms >= 0, "観測時刻が不正");
        ensure!(
            self.total >= 0 && (0..=self.total).contains(&self.deprecated),
            "観測したノート数が不正"
        );
        let offset = time::UtcOffset::from_whole_seconds(self.utc_offset_seconds)?;
        let observed = time::OffsetDateTime::from_unix_timestamp_nanos(
            i128::from(self.observed_at_ms) * 1_000_000,
        )?;
        let date = observed
            .checked_to_offset(offset)
            .context("観測日が範囲外")?
            .date();
        ensure!(date.to_string() == self.local_date, "観測日と時刻が不一致");
        Ok(())
    }

    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            workspace_id: row.get(0)?,
            local_date: row.get(1)?,
            observed_at_ms: row.get(2)?,
            utc_offset_seconds: row.get(3)?,
            total: row.get(4)?,
            deprecated: row.get(5)?,
        })
    }
}

/// 呼び出し側は失敗を劣化へ合流し、保守やノート操作の成功を取り消さない。
pub fn observe(vault: &Vault, conn: &Connection) -> Result<()> {
    let observation = capture(conn, crate::workspace::stored_workspace_id(vault)?)?;
    append_at(&runtime_path()?, &observation)
}

fn runtime_path() -> Result<PathBuf> {
    Ok(crate::app_data_dir()?.join("note-count-history/history.sqlite3"))
}

fn capture(conn: &Connection, workspace_id: String) -> Result<Observation> {
    // executor/importの途中状態を別DBへ確定すると、元のrollbackに追従できない。
    ensure!(
        conn.is_autocommit(),
        "未確定transactionのノート数は観測しない"
    );
    let (runtime_store, observation): (Option<String>, Observation) = conn.query_row(
        "SELECT (SELECT value FROM meta WHERE key='runtime_store'),
            date('now', 'localtime'), CAST(unixepoch('now', 'subsec') * 1000 AS INTEGER),
            CAST(strftime('%s', 'now', 'localtime') AS INTEGER) - unixepoch('now'),
            count(*), coalesce(sum(status = 'deprecated'), 0) FROM notes",
        [],
        |row| {
            Ok((
                row.get(0)?,
                Observation {
                    workspace_id,
                    local_date: row.get(1)?,
                    observed_at_ms: row.get(2)?,
                    utc_offset_seconds: row.get(3)?,
                    total: row.get(4)?,
                    deprecated: row.get(5)?,
                },
            ))
        },
    )?;
    ensure!(
        runtime_store.as_deref() == Some("db-v1"),
        "復元未完了のノート数は観測しない"
    );
    observation.validate()?;
    Ok(observation)
}

fn append_at(path: &Path, observation: &Observation) -> Result<()> {
    observation.validate()?;
    if !path.try_exists()? && publish_new(prepare_new_store(path, observation)?, path)? {
        return Ok(());
    }
    // 正常なrollback journalの回復は許す。CREATEを付けず、検証前に空表へ作り直さない。
    let mut conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    conn.busy_timeout(Duration::from_millis(100))?;
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    write_observation(&transaction, observation)?;
    transaction.commit()?;
    Ok(())
}

fn prepare_new_store(path: &Path, observation: &Observation) -> Result<tempfile::NamedTempFile> {
    let parent = path.parent().context("履歴保存先の親がない")?;
    std::fs::create_dir_all(parent)?;
    // 初期化途中で停止しても、最終pathに空の台帳を残して以後の観測を止めない。
    let temporary = tempfile::NamedTempFile::new_in(parent)?;
    let mut conn =
        Connection::open_with_flags(temporary.path(), OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    conn.busy_timeout(Duration::from_millis(100))?;
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch(TABLE_SQL)?;
    transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
    transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    write_observation(&transaction, observation)?;
    transaction.commit()?;
    conn.close().map_err(|(_, error)| error)?;
    temporary.as_file().sync_all()?;
    Ok(temporary)
}

fn publish_new(temporary: tempfile::NamedTempFile, path: &Path) -> Result<bool> {
    match temporary.persist_noclobber(path) {
        Ok(_) => Ok(true),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.error.into()),
    }
}

fn write_observation(conn: &Connection, observation: &Observation) -> Result<()> {
    validate_store(conn)?;
    conn.execute(
        "INSERT INTO daily_note_counts(workspace_id, local_date, observed_at_ms, utc_offset_seconds, total, deprecated)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(workspace_id, local_date) DO UPDATE SET
            observed_at_ms=excluded.observed_at_ms, utc_offset_seconds=excluded.utc_offset_seconds,
            total=excluded.total, deprecated=excluded.deprecated
         WHERE excluded.observed_at_ms > daily_note_counts.observed_at_ms",
        params![observation.workspace_id, observation.local_date, observation.observed_at_ms,
            observation.utc_offset_seconds, observation.total, observation.deprecated],
    )?;
    Ok(())
}

fn validate_store(conn: &Connection) -> Result<()> {
    ensure!(
        conn.pragma_query_value(None, "application_id", |r| r.get::<_, i64>(0))? == APPLICATION_ID
            && conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?
                == SCHEMA_VERSION,
        "未対応または不正なノート数履歴schema"
    );
    let mut statement = conn.prepare(
        "SELECT name, sql FROM sqlite_schema WHERE substr(name, 1, 7) != 'sqlite_' ORDER BY name",
    )?;
    let objects = statement
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        objects == [("daily_note_counts".into(), TABLE_SQL.into())],
        "ノート数履歴の構造が不正"
    );
    let mut statement = conn.prepare("SELECT workspace_id, local_date, observed_at_ms, utc_offset_seconds, total, deprecated FROM daily_note_counts")?;
    for observation in statement.query_map([], Observation::from_row)? {
        observation?.validate()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "01AAAAAAAAAAAAAAAAAAAAAAAA";

    fn observation(iso: &str, offset: i32, total: i64) -> Observation {
        let at = time::OffsetDateTime::parse(iso, &time::format_description::well_known::Rfc3339)
            .unwrap();
        Observation {
            workspace_id: WORKSPACE.into(),
            local_date: at
                .to_offset(time::UtcOffset::from_whole_seconds(offset).unwrap())
                .date()
                .to_string(),
            observed_at_ms: (at.unix_timestamp_nanos() / 1_000_000) as i64,
            utc_offset_seconds: offset,
            total,
            deprecated: 0,
        }
    }

    fn count_source() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
            INSERT INTO meta VALUES('runtime_store', 'db-v1'); CREATE TABLE notes(status TEXT);",
        )
        .unwrap();
        conn
    }

    fn rows(path: &Path) -> Vec<(String, String, i64, i64)> {
        Connection::open(path).unwrap().prepare("SELECT workspace_id, local_date, total, deprecated FROM daily_note_counts ORDER BY workspace_id, local_date").unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap()
            .collect::<rusqlite::Result<_>>().unwrap()
    }

    #[test]
    fn capture_matches_home_counts_including_null_and_empty_notes() {
        let conn = count_source();
        let empty = capture(&conn, WORKSPACE.into()).unwrap();
        assert_eq!((empty.total, empty.deprecated), (0, 0));
        conn.execute_batch(
            "INSERT INTO notes VALUES(NULL), ('active'), ('deprecated'), ('draft');",
        )
        .unwrap();
        let sample = capture(&conn, WORKSPACE.into()).unwrap();
        assert_eq!((sample.total, sample.deprecated), (4, 1));
        sample.validate().unwrap();
        assert_eq!(conn.total_changes(), 5, "captureは元DBを変更しない");
    }

    #[test]
    fn incomplete_restore_and_uncommitted_counts_are_rejected() {
        let conn = count_source();
        for marker in [None, Some("unknown")] {
            conn.execute("UPDATE meta SET value=?1", [marker]).unwrap();
            assert!(capture(&conn, WORKSPACE.into()).is_err());
        }
        conn.execute("UPDATE meta SET value='db-v1'", []).unwrap();
        let transaction = conn.unchecked_transaction().unwrap();
        transaction
            .execute("INSERT INTO notes VALUES('active')", [])
            .unwrap();
        assert!(capture(&transaction, WORKSPACE.into()).is_err());
    }

    #[test]
    fn newest_daily_observation_wins_without_filling_gaps_or_other_workspaces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite3");
        let older = observation("2026-09-01T00:00:00Z", 32_400, 4);
        let newer = observation("2026-09-01T01:00:00Z", 32_400, 2);
        for sample in [&older, &newer, &older] {
            append_at(&path, sample).unwrap();
        }
        let mut equal = newer.clone();
        equal.total = 50;
        append_at(&path, &equal).unwrap();
        append_at(&path, &observation("2026-09-03T00:00:00Z", 32_400, 0)).unwrap();
        let mut other = older.clone();
        other.workspace_id = "01BBBBBBBBBBBBBBBBBBBBBBBB".into();
        append_at(&path, &other).unwrap();
        assert_eq!(
            rows(&path),
            vec![
                (WORKSPACE.into(), "2026-09-01".into(), 2, 0),
                (WORKSPACE.into(), "2026-09-03".into(), 0, 0),
                (other.workspace_id, "2026-09-01".into(), 4, 0),
            ]
        );
    }

    #[test]
    fn recorded_calendar_date_survives_midnight_leap_day_and_offset_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite3");
        for (iso, offset, expected) in [
            ("2024-02-28T15:00:00Z", 32_400, "2024-02-29"),
            ("2026-03-08T06:30:00Z", -18_000, "2026-03-08"),
            ("2026-03-08T07:30:00Z", -14_400, "2026-03-08"),
            ("2026-11-01T05:30:00Z", -14_400, "2026-11-01"),
            ("2026-11-01T06:30:00Z", -18_000, "2026-11-01"),
        ] {
            let sample = observation(iso, offset, 1);
            assert_eq!(sample.local_date, expected);
            append_at(&path, &sample).unwrap();
        }
        assert_eq!(rows(&path).len(), 3, "夏冬時間の切替で同日行を増やさない");
    }

    #[test]
    fn invalid_observations_do_not_create_a_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite3");
        let valid = observation("2026-09-01T00:00:00Z", 0, 4);
        for variant in 0..7 {
            let mut bad = valid.clone();
            match variant {
                0 => bad.workspace_id = "../../workspace".into(),
                1 => bad.local_date = "2026-09-02".into(),
                2 => bad.observed_at_ms = -1,
                3 => bad.utc_offset_seconds = 86_400,
                4 => bad.total = -1,
                5 => bad.deprecated = 5,
                _ => bad.deprecated = -1,
            }
            assert!(append_at(&path, &bad).is_err());
            assert!(!path.exists());
        }
    }

    /// 2026-09-06: 初回CREATE直後の失敗で空の最終pathが残り、以後の観測が止まっていた。
    #[test]
    fn interrupted_initialization_does_not_publish_an_incomplete_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite3");
        let mut sample = observation("2026-09-01T00:00:00Z", 0, -1);
        assert!(prepare_new_store(&path, &sample).is_err());
        assert!(
            !path.exists(),
            "schema作成後のINSERT失敗でも最終pathを残さない"
        );
        sample.total = 4;
        let prepared = prepare_new_store(&path, &sample).unwrap();
        assert!(!path.exists(), "commit済みでも公開前は最終pathに現れない");
        drop(prepared);
        append_at(&path, &sample).unwrap();
        assert_eq!(rows(&path)[0].2, 4);
    }

    #[test]
    fn initial_publication_never_overwrites_a_concurrent_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite3");
        let sample = observation("2026-09-01T00:00:00Z", 0, 4);
        let prepared = prepare_new_store(&path, &sample).unwrap();
        std::fs::write(&path, b"existing unknown file").unwrap();
        assert!(!publish_new(prepared, &path).unwrap());
        assert!(append_at(&path, &sample).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"existing unknown file");
    }

    /// 2026-09-06: read-only先行検証が正常なhot journalの回復を阻み、観測を止めていた。
    #[test]
    fn hot_journal_is_recovered_before_appending() {
        const CHILD_PATH: &str = "KB_NOTE_COUNT_HISTORY_CRASH_FIXTURE";
        if let Some(path) = std::env::var_os(CHILD_PATH) {
            let conn =
                Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
            conn.execute_batch(
                "PRAGMA cache_size=1; PRAGMA cache_spill=ON; BEGIN IMMEDIATE;
                UPDATE daily_note_counts SET total=999; CREATE TABLE crash_padding(value BLOB);
                WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<64)
                INSERT INTO crash_padding SELECT zeroblob(4096) FROM n;",
            )
            .unwrap();
            // dropでrollbackせず、未確定のpageとjournalを残す。
            std::process::exit(73);
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite3");
        append_at(&path, &observation("2026-09-01T00:00:00Z", 0, 4)).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "note_count_history::tests::hot_journal_is_recovered_before_appending",
            ])
            .env(CHILD_PATH, &path)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(73), "{output:?}");
        assert!(
            dir.path()
                .join("history.sqlite3-journal")
                .metadata()
                .unwrap()
                .len()
                > 512
        );
        append_at(&path, &observation("2026-09-02T00:00:00Z", 0, 3)).unwrap();
        assert_eq!(
            rows(&path),
            vec![
                (WORKSPACE.into(), "2026-09-01".into(), 4, 0),
                (WORKSPACE.into(), "2026-09-02".into(), 3, 0),
            ]
        );
    }

    #[test]
    fn damaged_or_unknown_stores_are_rejected_before_modifying_bytes() {
        let sample = observation("2026-09-01T00:00:00Z", 0, 4);
        for mutation in [
            "PRAGMA user_version=999",
            "PRAGMA application_id=0",
            "DROP TABLE daily_note_counts",
            "CREATE TABLE unexpected(value)",
            "UPDATE daily_note_counts SET local_date='2020-01-01'",
            "PRAGMA ignore_check_constraints=ON; UPDATE daily_note_counts SET total=-1",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("history.sqlite3");
            append_at(&path, &sample).unwrap();
            Connection::open(&path)
                .unwrap()
                .execute_batch(mutation)
                .unwrap();
            let before = std::fs::read(&path).unwrap();
            assert!(append_at(&path, &sample).is_err(), "{mutation}");
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        for bytes in [b"".as_slice(), b"not a SQLite database".as_slice()] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("history.sqlite3");
            std::fs::write(&path, bytes).unwrap();
            assert!(append_at(&path, &sample).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn busy_history_leaves_the_previous_observation_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite3");
        append_at(&path, &observation("2026-09-01T00:00:00Z", 0, 4)).unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert!(append_at(&path, &observation("2026-09-01T01:00:00Z", 0, 3)).is_err());
        conn.execute_batch("ROLLBACK").unwrap();
        assert_eq!(rows(&path)[0].2, 4);
    }
}
