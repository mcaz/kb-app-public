//! 全件探索。条件をページ分割前に適用し、カテゴリや最近500件へ範囲を狭めない。

use anyhow::{Result, ensure};
use rusqlite::{Connection, params_from_iter, types::Value};
use serde::{Deserialize, Serialize};

use super::{Hit, split_tags};
use crate::degradation::Degradation;
use crate::error::CoreError;

const VISIBLE_NOTES: &str = "status != 'deprecated' AND normal_reference_allowed = 1";
// ページ間で条件や同値の並び順を持ち越す。ノート本文はcursorへ含めない。
const MAX_CURSOR_BYTES: usize = 65_536;
const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum NoteBrowsePeriod {
    #[serde(rename = "all")]
    All,
    #[serde(rename = "7")]
    SevenDays,
    #[serde(rename = "30")]
    ThirtyDays,
    #[serde(rename = "90")]
    NinetyDays,
}

impl NoteBrowsePeriod {
    fn days(self) -> Option<i64> {
        match self {
            Self::All => None,
            Self::SevenDays => Some(7),
            Self::ThirtyDays => Some(30),
            Self::NinetyDays => Some(90),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum NoteBrowseSort {
    Updated,
    Created,
    Title,
}

impl NoteBrowseSort {
    fn column(self) -> &'static str {
        match self {
            Self::Updated => "coalesce(generated_at, '')",
            Self::Created => "coalesce(created, '')",
            Self::Title => "coalesce(title, id)",
        }
    }

    fn ascending(self) -> bool {
        self == Self::Title
    }
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct NoteBrowsePage {
    pub hits: Vec<Hit>,
    pub total: usize,
    pub next_cursor: Option<String>,
    pub degraded: Vec<Degradation>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    tags: Vec<String>,
    period: NoteBrowsePeriod,
    sort: NoteBrowseSort,
    as_of_ms: i64,
    value: String,
    id: String,
}

/// ホームの件数と全件探索の母集団を揃える。保守用Statsの総数は変更しない。
pub fn browsable_note_count(conn: &Connection) -> Result<usize> {
    let count = conn.query_row(
        &format!("SELECT count(*) FROM notes WHERE {VISIBLE_NOTES}"),
        [],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(usize::try_from(count)?)
}

/// 全通常参照ノートをDBで絞り込み、最大100件のkeyset pageを返す。
/// 同一ページのtotalとhitsは同じsnapshotから取得する。ページを跨ぐ間の更新は
/// snapshotで固定せず、再取得時の状態へ追従する。タイトル順はDBのBINARY順。
pub fn browse_notes(
    conn: &Connection,
    tags: &[String],
    period: NoteBrowsePeriod,
    sort: NoteBrowseSort,
    after: Option<&str>,
    limit: usize,
) -> std::result::Result<NoteBrowsePage, CoreError> {
    let now_ms = i64::try_from(time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000)
        .map_err(CoreError::unexpected)?;
    browse_notes_at(conn, tags, period, sort, after, limit, now_ms)
}

fn browse_notes_at(
    conn: &Connection,
    tags: &[String],
    period: NoteBrowsePeriod,
    sort: NoteBrowseSort,
    after: Option<&str>,
    limit: usize,
    now_ms: i64,
) -> std::result::Result<NoteBrowsePage, CoreError> {
    let (tags, cursor) =
        validate_input(tags, period, sort, after).map_err(CoreError::invalid_input)?;
    let as_of_ms = cursor.as_ref().map_or(now_ms, |cursor| cursor.as_of_ms);
    browse_page(
        conn,
        tags,
        period,
        sort,
        cursor,
        limit.clamp(1, 100),
        as_of_ms,
    )
    .map_err(CoreError::index)
}

fn validate_input(
    tags: &[String],
    period: NoteBrowsePeriod,
    sort: NoteBrowseSort,
    after: Option<&str>,
) -> Result<(Vec<String>, Option<Cursor>)> {
    // 通常ノートは最大4タグだが、絞り込みの選択数は保存契約と別に扱う。
    ensure!(tags.len() <= 64, "タグ条件が多すぎる");
    for tag in tags {
        ensure!(
            !tag.is_empty() && tag.len() <= 1024 && !tag.chars().any(char::is_whitespace),
            "タグ条件に空白または過大な値がある"
        );
    }
    let mut tags = tags.to_vec();
    tags.sort();
    tags.dedup();
    let cursor = after
        .map(|after| {
            ensure!(after.len() <= MAX_CURSOR_BYTES, "cursorが大きすぎる");
            let cursor: Cursor = serde_json::from_str(after)?;
            ensure!(cursor.version == 1, "cursorの版が一致しない");
            ensure!(
                cursor.tags == tags && cursor.period == period && cursor.sort == sort,
                "cursorと今回の絞り込み条件が一致しない"
            );
            ensure!(!cursor.id.is_empty(), "cursorのノートIDが空");
            // 計算のoverflowを入力経由で起こさない。端末時計はこの範囲にある。
            ensure!(
                cursor.as_of_ms.checked_sub(90 * DAY_MS).is_some(),
                "cursorの基準時刻が不正"
            );
            Ok(cursor)
        })
        .transpose()?;
    Ok((tags, cursor))
}

fn browse_page(
    conn: &Connection,
    tags: Vec<String>,
    period: NoteBrowsePeriod,
    sort: NoteBrowseSort,
    cursor: Option<Cursor>,
    limit: usize,
    as_of_ms: i64,
) -> Result<NoteBrowsePage> {
    let snapshot = conn
        .is_autocommit()
        .then(|| conn.unchecked_transaction())
        .transpose()?;
    let mut conditions = vec![VISIBLE_NOTES.to_owned()];
    let mut values = Vec::<Value>::new();
    for tag in &tags {
        // 索引は空白区切り。LIKEのwildcardや隣のタグを部分一致させない。
        conditions.push("instr(' ' || coalesce(tags, '') || ' ', ?) > 0".into());
        values.push(format!(" {tag} ").into());
    }
    if let Some(days) = period.days() {
        // matchesFilter同様、未来は含む。不明日付とepoch(0ms)だけを期間条件から除く。
        // unixepochのsubsecを使い、日数境界の1msを秒へ丸め落とさない。
        // ISO日付だけならUTC、時刻付きでzone省略なら端末localというJS Dateの規則。
        let updated_ms = "CAST(round((CASE
            WHEN substr(generated_at, 11, 1) IN ('T', ' ')
             AND instr(substr(generated_at, 12), '+') = 0
             AND instr(substr(generated_at, 12), '-') = 0
             AND lower(substr(generated_at, -1)) != 'z'
            THEN unixepoch(generated_at, 'utc', 'subsec')
            ELSE unixepoch(generated_at, 'subsec') END) * 1000) AS INTEGER)";
        conditions.push(format!("{updated_ms} != 0 AND {updated_ms} >= ?"));
        values.push(
            as_of_ms
                .checked_sub(days * DAY_MS)
                .ok_or_else(|| anyhow::anyhow!("基準時刻が不正"))?
                .into(),
        );
    }
    let conditions = conditions.join(" AND ");
    let total = conn.query_row(
        &format!("SELECT count(*) FROM notes WHERE {conditions}"),
        params_from_iter(&values),
        |row| row.get::<_, i64>(0),
    )?;
    let mut next_condition = String::new();
    if let Some(cursor) = cursor {
        let comparison = if sort.ascending() { ">" } else { "<" };
        next_condition =
            format!("WHERE browse_sort {comparison} ? OR (browse_sort = ? AND id > ?)");
        values.push(cursor.value.clone().into());
        values.push(cursor.value.into());
        values.push(cursor.id.into());
    }
    values.push(i64::try_from(limit + 1)?.into());
    let direction = if sort.ascending() { "ASC" } else { "DESC" };
    let sql = format!(
        "WITH eligible AS (
           SELECT id, title, status, coalesce(description, substr(body, 1, 120)), origin, tags,
                  created, generated_at, note_uid, namespace, authority_role, authority_status,
                  authority_scope, {} COLLATE BINARY AS browse_sort
           FROM notes WHERE {conditions}
         ) SELECT * FROM eligible {next_condition}
         ORDER BY browse_sort {direction}, id ASC LIMIT ?",
        sort.column()
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(&values), |row| {
        Ok((
            Hit {
                id: row.get(0)?,
                title: row.get(1)?,
                status: row.get(2)?,
                snippet: row.get::<_, String>(3)?.replace('\n', " "),
                via: "browse",
                distance: None,
                origin: row.get(4)?,
                tags: split_tags(row.get(5)?),
                created: row.get(6)?,
                updated: row.get(7)?,
                note_uid: row.get(8)?,
                namespace: row.get(9)?,
                authority_role: row.get(10)?,
                authority_status: row.get(11)?,
                authority_scope: row.get(12)?,
            },
            row.get::<_, String>(13)?,
        ))
    })?;
    let mut rows = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = if has_more {
        rows.last()
            .map(|(hit, value)| {
                serde_json::to_string(&Cursor {
                    version: 1,
                    tags,
                    period,
                    sort,
                    as_of_ms,
                    value: value.clone(),
                    id: hit.id.clone(),
                })
            })
            .transpose()?
    } else {
        None
    };
    drop(stmt);
    if let Some(snapshot) = snapshot {
        snapshot.commit()?;
    }
    Ok(NoteBrowsePage {
        hits: rows.into_iter().map(|(hit, _)| hit).collect(),
        total: usize::try_from(total)?,
        next_cursor,
        degraded: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::error::CoreErrorKind;
    use crate::vault::Vault;

    fn setup() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        (dir, conn)
    }

    fn insert(conn: &Connection, id: &str, title: Option<&str>, tags: &str, updated: Option<&str>) {
        conn.execute(
            "INSERT INTO notes(id,title,status,body,tags,generated_at,created,normal_reference_allowed)
             VALUES (?1,?2,'stable','本文',?3,?4,?4,1)",
            rusqlite::params![id, title, tags, updated],
        )
        .unwrap();
    }

    fn millis(date: &str) -> i64 {
        i64::try_from(
            time::OffsetDateTime::parse(date, &time::format_description::well_known::Rfc3339)
                .unwrap()
                .unix_timestamp_nanos()
                / 1_000_000,
        )
        .unwrap()
    }

    /// 2026-09-08: ホームの全件導線をrecent(500件)やルート直下へ接続すると欠落する。
    #[test]
    fn every_category_and_root_is_paged_once_beyond_five_hundred_with_tied_sorts() {
        let (_dir, conn) = setup();
        let mut expected = Vec::new();
        for index in 0..537 {
            let id = match index % 3 {
                0 => format!("root-{index:04}"),
                1 => format!("notes/{index:04}"),
                _ => format!("team/deep/{index:04}"),
            };
            insert(
                &conn,
                &id,
                Some("同じタイトル"),
                "test",
                Some("2026-09-08T00:00:00Z"),
            );
            expected.push(id);
        }
        expected.sort();
        for sort in [
            NoteBrowseSort::Updated,
            NoteBrowseSort::Created,
            NoteBrowseSort::Title,
        ] {
            let mut actual = Vec::new();
            let mut after = None;
            loop {
                let page = browse_notes(
                    &conn,
                    &[],
                    NoteBrowsePeriod::All,
                    sort,
                    after.as_deref(),
                    1000,
                )
                .unwrap();
                assert_eq!(page.total, 537);
                assert!(page.hits.len() <= 100);
                assert!(page.degraded.is_empty());
                actual.extend(page.hits.into_iter().map(|hit| hit.id));
                after = page.next_cursor;
                if after.is_none() {
                    break;
                }
                assert!(actual.len() <= 537, "cursorが進まない");
            }
            assert_eq!(actual, expected);
            assert_eq!(actual.iter().collect::<BTreeSet<_>>().len(), 537);
        }
        assert_eq!(browsable_note_count(&conn).unwrap(), 537);
        let root_only = super::super::notes_in_category(&conn, "", None, 100).unwrap();
        assert_eq!(
            root_only.total, 179,
            "既存カテゴリAPIのルート直下という意味は保つ"
        );
    }

    #[test]
    fn filters_apply_before_paging_and_date_boundaries_match_the_search_filter() {
        let (_dir, conn) = setup();
        for index in 0..505 {
            insert(
                &conn,
                &format!("noise/{index:04}"),
                None,
                "other",
                Some("2026-09-08T00:00:00Z"),
            );
        }
        for (id, tags, updated) in [
            ("a-boundary", "ai ops", Some("2026-09-01T00:00:00.999Z")),
            ("b-offset", "ai ops", Some("2026-09-01T09:00:00.999+09:00")),
            ("c-future", "ai ops", Some("2026-09-20T00:00:00Z")),
            ("too-old", "ai ops", Some("2026-09-01T00:00:00.998Z")),
            ("partial-tag", "ai-agent ops", Some("2026-09-08T00:00:00Z")),
            ("missing-tag", "ai", Some("2026-09-08T00:00:00Z")),
            ("missing-date", "ai ops", None),
            ("invalid-date", "ai ops", Some("unknown")),
        ] {
            insert(&conn, id, None, tags, updated);
        }
        let tags = vec!["ops".into(), "ai".into(), "ai".into()];
        let first = browse_notes_at(
            &conn,
            &tags,
            NoteBrowsePeriod::SevenDays,
            NoteBrowseSort::Title,
            None,
            1,
            millis("2026-09-08T00:00:00.999Z"),
        )
        .unwrap();
        assert_eq!(first.total, 3);
        assert_eq!(first.hits[0].id, "a-boundary");
        // ページ間に時間が進んでも、期間の基準時刻を動かして途中行を消さない。
        let next = browse_notes_at(
            &conn,
            &["ai".into(), "ops".into()],
            NoteBrowsePeriod::SevenDays,
            NoteBrowseSort::Title,
            first.next_cursor.as_deref(),
            100,
            millis("2026-09-12T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(next.total, 3);
        assert_eq!(
            next.hits
                .iter()
                .map(|hit| hit.id.as_str())
                .collect::<Vec<_>>(),
            ["b-offset", "c-future"]
        );
        assert!(next.next_cursor.is_none());
        let all = browse_notes(
            &conn,
            &tags,
            NoteBrowsePeriod::All,
            NoteBrowseSort::Updated,
            None,
            100,
        )
        .unwrap();
        assert_eq!(all.total, 6, "allでは欠損・不明日付を除外しない");
    }

    #[test]
    fn epoch_is_excluded_but_future_and_pre_epoch_dates_follow_the_lower_bound() {
        let (_dir, conn) = setup();
        insert(
            &conn,
            "epoch",
            None,
            "test",
            Some("1970-01-01T00:00:00.000Z"),
        );
        insert(
            &conn,
            "before",
            None,
            "test",
            Some("1969-12-31T23:59:59.999Z"),
        );
        insert(
            &conn,
            "future",
            None,
            "test",
            Some("2030-01-01T00:00:00.001Z"),
        );
        for period in [
            NoteBrowsePeriod::SevenDays,
            NoteBrowsePeriod::ThirtyDays,
            NoteBrowsePeriod::NinetyDays,
        ] {
            let page =
                browse_notes_at(&conn, &[], period, NoteBrowseSort::Title, None, 100, DAY_MS)
                    .unwrap();
            assert_eq!(
                page.hits
                    .iter()
                    .map(|hit| hit.id.as_str())
                    .collect::<Vec<_>>(),
                ["before", "future"]
            );
        }
    }

    #[test]
    fn hidden_proposals_and_deprecated_notes_do_not_leak_into_hits_or_total() {
        let (_dir, conn) = setup();
        for id in [
            "root",
            "notes/visible",
            "team/deprecated",
            "other/undecided",
        ] {
            insert(&conn, id, None, "test", None);
        }
        conn.execute(
            "UPDATE notes SET status='deprecated' WHERE id='team/deprecated'",
            [],
        )
        .unwrap();
        conn.execute("UPDATE notes SET normal_reference_allowed=0, authority_role='proposal' WHERE id='other/undecided'", []).unwrap();
        let page = browse_notes(
            &conn,
            &[],
            NoteBrowsePeriod::All,
            NoteBrowseSort::Title,
            None,
            100,
        )
        .unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.total, browsable_note_count(&conn).unwrap());
        assert_eq!(
            page.hits
                .iter()
                .map(|hit| hit.id.as_str())
                .collect::<Vec<_>>(),
            ["notes/visible", "root"]
        );
    }

    #[test]
    fn cursor_rejects_changed_filters_and_malformed_input_and_binds_values() {
        let (_dir, conn) = setup();
        insert(&conn, "a", Some("a' OR 1=1 --"), "x'quoted", None);
        insert(&conn, "b", Some("b"), "x'quoted", None);
        let first = browse_notes(
            &conn,
            &["x'quoted".into()],
            NoteBrowsePeriod::All,
            NoteBrowseSort::Title,
            None,
            0,
        )
        .unwrap();
        assert_eq!(first.hits.len(), 1, "0件要求も既存一覧同様1件へ丸める");
        let cursor = first.next_cursor.unwrap();
        for invalid in ["{", "{}", &"x".repeat(MAX_CURSOR_BYTES + 1)] {
            let error = browse_notes(
                &conn,
                &[],
                NoteBrowsePeriod::All,
                NoteBrowseSort::Title,
                Some(invalid),
                1,
            )
            .unwrap_err();
            assert_eq!(error.kind(), Some(CoreErrorKind::InvalidInput));
        }
        for (tags, period, sort) in [
            (Vec::new(), NoteBrowsePeriod::All, NoteBrowseSort::Title),
            (
                vec!["x'quoted".into()],
                NoteBrowsePeriod::SevenDays,
                NoteBrowseSort::Title,
            ),
            (
                vec!["x'quoted".into()],
                NoteBrowsePeriod::All,
                NoteBrowseSort::Created,
            ),
        ] {
            assert_eq!(
                browse_notes(&conn, &tags, period, sort, Some(&cursor), 1)
                    .unwrap_err()
                    .kind(),
                Some(CoreErrorKind::InvalidInput)
            );
        }
        let next = browse_notes(
            &conn,
            &["x'quoted".into()],
            NoteBrowsePeriod::All,
            NoteBrowseSort::Title,
            Some(&cursor),
            1,
        )
        .unwrap();
        assert_eq!(next.hits[0].id, "b");
        assert_eq!(next.total, 2);
    }

    #[test]
    fn null_sort_values_and_deleted_cursor_row_do_not_restart_the_page() {
        let (_dir, conn) = setup();
        insert(&conn, "a", None, "test", None);
        insert(&conn, "b", None, "test", None);
        insert(&conn, "c", None, "test", None);
        let first = browse_notes(
            &conn,
            &[],
            NoteBrowsePeriod::All,
            NoteBrowseSort::Updated,
            None,
            1,
        )
        .unwrap();
        assert_eq!(first.hits[0].id, "a");
        conn.execute("DELETE FROM notes WHERE id='a'", []).unwrap();
        let next = browse_notes(
            &conn,
            &[],
            NoteBrowsePeriod::All,
            NoteBrowseSort::Updated,
            first.next_cursor.as_deref(),
            100,
        )
        .unwrap();
        assert_eq!(
            next.hits
                .iter()
                .map(|hit| hit.id.as_str())
                .collect::<Vec<_>>(),
            ["b", "c"]
        );
        assert_eq!(next.total, 2);
    }

    #[test]
    fn each_sort_uses_its_own_value_then_id_across_single_item_pages() {
        let (_dir, conn) = setup();
        for (id, title, updated, created) in [
            ("a", Some("Z"), Some("2026-09-01"), Some("2026-01-01")),
            ("b", Some("A"), Some("2026-09-02"), Some("2026-01-03")),
            ("c", Some("A"), Some("2026-09-03"), Some("2026-01-02")),
            ("d", None, None, None),
        ] {
            insert(&conn, id, title, "test", updated);
            conn.execute(
                "UPDATE notes SET created=?1 WHERE id=?2",
                rusqlite::params![created, id],
            )
            .unwrap();
        }
        for (sort, expected) in [
            (NoteBrowseSort::Updated, ["c", "b", "a", "d"]),
            (NoteBrowseSort::Created, ["b", "c", "a", "d"]),
            (NoteBrowseSort::Title, ["b", "c", "a", "d"]),
        ] {
            let mut after = None;
            let mut actual = Vec::new();
            loop {
                let page =
                    browse_notes(&conn, &[], NoteBrowsePeriod::All, sort, after.as_deref(), 1)
                        .unwrap();
                actual.extend(page.hits.into_iter().map(|hit| hit.id));
                after = page.next_cursor;
                if after.is_none() {
                    break;
                }
                assert!(actual.len() <= 4);
            }
            assert_eq!(actual, expected);
        }
    }
}
