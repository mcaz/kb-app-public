//! 記録時の暦日でノート数を並べる。欠測は0や前日の値にせず、読取では台帳を準備しない。

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, params};
use serde::Serialize;

const PERIOD_DAYS: i64 = 14;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum NoteCountTrendStatus {
    Available,
    NoObservations,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct NoteCountTrendDay {
    pub local_date: String,
    pub count: Option<u64>,
    pub observed_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct NoteCountTrend {
    pub status: NoteCountTrendStatus,
    pub days: Vec<NoteCountTrendDay>,
}

impl NoteCountTrend {
    pub fn unavailable() -> Self {
        Self {
            status: NoteCountTrendStatus::Unavailable,
            days: Vec::new(),
        }
    }
}

/// AI連携の設定には依存せず、選択中workspaceの保存済み観測だけを読む。
pub fn read(workspace_id: &str, local_today: &str) -> NoteCountTrend {
    let Ok(path) = super::runtime_path() else {
        return NoteCountTrend::unavailable();
    };
    let now = (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64;
    read_at(&path, workspace_id, local_today, now)
}

fn read_at(path: &Path, workspace_id: &str, local_today: &str, now: i64) -> NoteCountTrend {
    load_at(path, workspace_id, local_today, now).unwrap_or_else(|_| NoteCountTrend::unavailable())
}

fn load_at(path: &Path, workspace_id: &str, local_today: &str, now: i64) -> Result<NoteCountTrend> {
    ensure!(crate::artifact::is_ulid(workspace_id), "観測先IDが不正");
    ensure!(local_today.len() == 10 && now >= 0, "観測期間が不正");
    let format = time::format_description::parse_borrowed::<3>("[year]-[month]-[day]")?;
    let today = time::Date::parse(local_today, &format)?;
    ensure!(today.to_string() == local_today, "観測日はYYYY-MM-DDが必要");
    let first = today
        .checked_sub(time::Duration::days(PERIOD_DAYS - 1))
        .context("観測期間が範囲外")?;
    ensure!(first.to_string().len() == 10, "観測期間が範囲外");
    let mut trend = NoteCountTrend {
        status: NoteCountTrendStatus::NoObservations,
        days: (0..PERIOD_DAYS)
            .map(|offset| NoteCountTrendDay {
                local_date: (first + time::Duration::days(offset)).to_string(),
                count: None,
                observed_at_ms: None,
            })
            .collect(),
    };
    if !path.try_exists()? {
        return Ok(trend);
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(Duration::from_millis(100))?;
    conn.execute_batch("PRAGMA query_only=ON")?;
    let transaction = conn.unchecked_transaction()?;
    // 正常なhot journalも読取側では回復しない。次の保守での回復まで利用不可を返す。
    super::validate_store(&transaction)?;
    let mut statement = transaction.prepare(
        "SELECT local_date, total - deprecated, observed_at_ms FROM daily_note_counts
         WHERE workspace_id=?1 AND local_date>=?2 AND local_date<=?3 AND observed_at_ms<=?4
         ORDER BY local_date",
    )?;
    for row in statement.query_map(
        params![workspace_id, first.to_string(), local_today, now],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        },
    )? {
        let (date, count, observed_at_ms) = row?;
        let day = trend
            .days
            .iter_mut()
            .find(|day| day.local_date == date)
            .context("観測日が期間外")?;
        day.count = Some(count.try_into()?);
        day.observed_at_ms = Some(observed_at_ms);
        trend.status = NoteCountTrendStatus::Available;
    }
    Ok(trend)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note_count_history::{Observation, append_at};

    const WORKSPACE: &str = "01AAAAAAAAAAAAAAAAAAAAAAAA";
    const OTHER: &str = "01BBBBBBBBBBBBBBBBBBBBBBBB";

    fn observation(iso: &str, offset: i32, total: i64, deprecated: i64) -> Observation {
        let time = time::OffsetDateTime::parse(iso, &time::format_description::well_known::Rfc3339)
            .unwrap();
        Observation {
            workspace_id: WORKSPACE.into(),
            local_date: time
                .to_offset(time::UtcOffset::from_whole_seconds(offset).unwrap())
                .date()
                .to_string(),
            observed_at_ms: (time.unix_timestamp_nanos() / 1_000_000) as i64,
            utc_offset_seconds: offset,
            total,
            deprecated,
        }
    }

    #[test]
    fn missing_history_and_invalid_requests_never_create_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing/history.sqlite3");
        let empty = read_at(&path, WORKSPACE, "2024-03-01", 1);
        assert_eq!(empty.status, NoteCountTrendStatus::NoObservations);
        assert_eq!(empty.days.len(), 14);
        assert_eq!(empty.days[0].local_date, "2024-02-17");
        assert_eq!(empty.days[12].local_date, "2024-02-29");
        assert!(
            empty
                .days
                .iter()
                .all(|day| day.count.is_none() && day.observed_at_ms.is_none())
        );
        for date in [
            "2026-02-29",
            "2026-9-6",
            "2026-09-06Z",
            "+2026-09-06",
            "0000-01-01",
        ] {
            let invalid = read_at(&path, WORKSPACE, date, 1);
            assert_eq!(invalid.status, NoteCountTrendStatus::Unavailable, "{date}");
            assert!(invalid.days.is_empty());
        }
        assert_eq!(
            read_at(&path, "bad-id", "2026-09-06", 1).status,
            NoteCountTrendStatus::Unavailable
        );
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn fixed_dates_preserve_gaps_zero_and_workspace_boundaries_without_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite3");
        // UTCでは前日でも記録時のローカル日を保つ。前年の行は今回の期間外。
        let samples = [
            observation("2025-12-20T00:00:00Z", 0, 20, 0),
            observation("2025-12-31T15:00:00Z", 32_400, 12, 2),
            observation("2026-01-03T00:00:00Z", 0, 2, 2),
            observation("2026-01-04T01:00:00Z", 0, 99, 0),
        ];
        for sample in &samples {
            append_at(&path, sample).unwrap();
        }
        let mut other = samples[1].clone();
        other.workspace_id = OTHER.into();
        other.total = 50;
        append_at(&path, &other).unwrap();
        let before = std::fs::read(&path).unwrap();
        let now = samples[3].observed_at_ms - 1;
        let trend = read_at(&path, WORKSPACE, "2026-01-04", now);
        assert_eq!(trend.status, NoteCountTrendStatus::Available);
        assert_eq!(trend.days.len(), 14);
        assert_eq!(trend.days[0].local_date, "2025-12-22");
        assert_eq!(trend.days[10].count, Some(10));
        assert_eq!(
            trend.days[10].observed_at_ms,
            Some(samples[1].observed_at_ms)
        );
        assert_eq!(trend.days[11].count, None);
        assert_eq!(trend.days[12].count, Some(0));
        assert_eq!(trend.days[13].count, None, "未来の観測時刻は表示しない");
        assert_eq!(
            read_at(&path, OTHER, "2026-01-04", now).days[10].count,
            Some(48)
        );
        assert_eq!(
            read_at(&path, WORKSPACE, "2026-03-01", now).status,
            NoteCountTrendStatus::NoObservations
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "journal等を作らない"
        );
    }

    #[test]
    fn corrupted_unknown_or_locked_history_is_unavailable_and_unchanged() {
        let sample = observation("2026-09-06T00:00:00Z", 0, 4, 1);
        for mutation in [
            "PRAGMA user_version=999",
            "DROP TABLE daily_note_counts",
            "UPDATE daily_note_counts SET local_date='invalid'",
            "PRAGMA ignore_check_constraints=ON; UPDATE daily_note_counts SET deprecated=99",
            "BEGIN EXCLUSIVE",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("history.sqlite3");
            append_at(&path, &sample).unwrap();
            let writer = Connection::open(&path).unwrap();
            writer.execute_batch(mutation).unwrap();
            let before = std::fs::read(&path).unwrap();
            let failed = read_at(&path, WORKSPACE, &sample.local_date, sample.observed_at_ms);
            assert_eq!(
                failed.status,
                NoteCountTrendStatus::Unavailable,
                "{mutation}"
            );
            assert!(failed.days.is_empty());
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite3");
        std::fs::write(&path, b"not sqlite").unwrap();
        assert_eq!(
            read_at(&path, WORKSPACE, &sample.local_date, sample.observed_at_ms).status,
            NoteCountTrendStatus::Unavailable
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"not sqlite");
    }
}
