//! Homeの利用推移。暦日の境界だけをUIから受け取り、台帳の意味づけはコアに置く。

use serde::Serialize;

use crate::observation_health::ObservationHealthStatus;
use crate::session_ledger::{self, trend::DailySummary};
use crate::settings::Settings;

pub use crate::session_ledger::trend::ObservationTrendFilter;

const PERIOD_DAYS: usize = 14;
const HOUR_MS: i64 = 3_600_000;

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ObservationTrendDay {
    pub start_ms: i64,
    pub end_ms: i64,
    pub hook_output_emitted: u64,
    pub propose_successes: u64,
    pub update_successes: u64,
    pub errors: u64,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ObservationTrend {
    pub status: ObservationHealthStatus,
    pub days: Vec<ObservationTrendDay>,
}

impl ObservationTrend {
    pub fn unavailable() -> Self {
        Self::empty(ObservationHealthStatus::Unavailable)
    }

    fn empty(status: ObservationHealthStatus) -> Self {
        Self {
            status,
            days: Vec::new(),
        }
    }
}

/// OFFではworkspaceも台帳も解決しない。既存の14日間合計とは別の取得口とする。
pub fn read(
    settings: &Settings,
    day_boundaries_ms: &[i64],
    filter: ObservationTrendFilter,
    workspace: impl FnOnce() -> anyhow::Result<String>,
) -> ObservationTrend {
    read_with(
        settings,
        day_boundaries_ms,
        filter,
        workspace,
        session_ledger::now_ms(),
        session_ledger::trend::summarize,
    )
}

fn read_with(
    settings: &Settings,
    day_boundaries_ms: &[i64],
    filter: ObservationTrendFilter,
    workspace: impl FnOnce() -> anyhow::Result<String>,
    now: i64,
    summarize: impl FnOnce(
        &[i64; 15],
        i64,
        ObservationTrendFilter,
        &str,
    ) -> anyhow::Result<DailySummary>,
) -> ObservationTrend {
    if !settings.ai_kb_enabled || (!settings.claude_kb_enabled && !settings.gpt_kb_enabled) {
        return ObservationTrend::empty(ObservationHealthStatus::Disabled);
    }
    let Ok(boundaries) = <&[i64; PERIOD_DAYS + 1]>::try_from(day_boundaries_ms) else {
        return ObservationTrend::unavailable();
    };
    // 端末の暦日は夏時間で23/25時間になる。今日の途中までを上限にして未来を数えない。
    if boundaries[0] < 0
        || boundaries.windows(2).any(|pair| {
            !pair[1]
                .checked_sub(pair[0])
                .is_some_and(|duration| (23 * HOUR_MS..=25 * HOUR_MS).contains(&duration))
        })
        || now < boundaries[PERIOD_DAYS - 1]
        || now >= boundaries[PERIOD_DAYS]
    {
        return ObservationTrend::unavailable();
    }
    let Ok(workspace_id) = workspace() else {
        return ObservationTrend::unavailable();
    };
    let Ok(summary) = summarize(boundaries, now, filter, &workspace_id) else {
        return ObservationTrend::unavailable();
    };
    if !summary.has_observations {
        return ObservationTrend::empty(ObservationHealthStatus::NoObservations);
    }
    let days = summary
        .days
        .into_iter()
        .zip(boundaries.windows(2))
        .map(|(counts, range)| ObservationTrendDay {
            start_ms: range[0],
            end_ms: range[1].min(now),
            hook_output_emitted: counts.hook_output_emitted,
            propose_successes: counts.propose_successes,
            update_successes: counts.update_successes,
            // 拒否codeや未分類件数はエラー内訳なので、再加算しない。
            errors: counts.hook_stdout_failed
                + counts.hook_errors
                + counts.propose_errors
                + counts.update_errors,
        })
        .collect();
    ObservationTrend {
        status: ObservationHealthStatus::Available,
        days,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_surface::ClientSurface;
    use crate::session_ledger::{
        CapAssumption, EventContext, HookEmissionOutcome, HookErrorStage, HookFilterReason,
        HookObservation, HookTimings, LedgerEvent, WriteOutcome, WriteTool,
    };
    use crate::write_rejection::WriteRejection;
    use rusqlite::Connection;
    use std::path::Path;

    const DAY_MS: i64 = 24 * HOUR_MS;
    const WORKSPACE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const OTHER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
    const FILTERS: [ObservationTrendFilter; 3] = [
        ObservationTrendFilter::All,
        ObservationTrendFilter::Claude,
        ObservationTrendFilter::Gpt,
    ];

    fn window() -> ([i64; 15], i64) {
        let today = (session_ledger::now_ms() / DAY_MS - 1) * DAY_MS;
        let now = today + 12 * HOUR_MS;
        (
            std::array::from_fn(|day| today + (day as i64 - 13) * DAY_MS),
            now,
        )
    }

    fn context(workspace: Option<&str>, surface: ClientSurface) -> EventContext<'_> {
        EventContext {
            surface,
            workspace_id: workspace,
            session_id: None,
            prompt_id: None,
            turn_id: None,
            permission_mode: None,
        }
    }

    fn write(path: &Path, at: i64, workspace: Option<&str>, surface: ClientSurface) {
        append_write(
            path,
            at,
            context(workspace, surface),
            WriteTool::Propose,
            WriteOutcome::Success,
            None,
        );
    }

    fn append_write(
        path: &Path,
        at: i64,
        context: EventContext<'_>,
        tool: WriteTool,
        outcome: WriteOutcome,
        code: Option<WriteRejection>,
    ) {
        let event =
            LedgerEvent::write(context, at, &format!("call-{at}"), tool, outcome, code).unwrap();
        session_ledger::append_at(path, &event).unwrap();
    }

    fn read_at(path: &Path, boundaries: &[i64], now: i64) -> ObservationTrend {
        read_filtered_at(path, boundaries, now, ObservationTrendFilter::All)
    }

    fn read_filtered_at(
        path: &Path,
        boundaries: &[i64],
        now: i64,
        filter: ObservationTrendFilter,
    ) -> ObservationTrend {
        read_with(
            &Settings::default(),
            boundaries,
            filter,
            || Ok(WORKSPACE.into()),
            now,
            |boundaries, until, filter, workspace| {
                session_ledger::trend::summarize_at(path, boundaries, until, filter, workspace)
            },
        )
    }

    fn prepared(path: &Path, at: i64) -> session_ledger::HookReceipt {
        prepared_for_surface(path, at, ClientSurface::CodexCli)
    }

    fn prepared_for_surface(
        path: &Path,
        at: i64,
        surface: ClientSurface,
    ) -> session_ledger::HookReceipt {
        let delivery = crate::hook_delivery::render_hook_delivery(
            &serde_json::json!({
                "hits": [{"id": "notes/fixture"}],
                "documents": [{"id": "notes/fixture", "text": "合成データ"}]
            }),
            surface.hook_output_budget(),
        )
        .unwrap();
        let event = LedgerEvent::hook(
            context(Some(WORKSPACE), surface),
            at,
            HookObservation::OutputPrepared {
                stats: delivery.stats,
                timings: HookTimings::default(),
                cap_assumption: CapAssumption::for_surface(surface),
            },
        )
        .unwrap();
        session_ledger::append_at(path, &event)
            .unwrap()
            .receipt
            .unwrap()
    }

    #[test]
    fn filter_defaults_to_all_and_uses_explicit_wire_values() {
        assert_eq!(
            ObservationTrendFilter::default(),
            ObservationTrendFilter::All
        );
        for (filter, value) in FILTERS.into_iter().zip(["all", "claude", "gpt"]) {
            assert_eq!(serde_json::to_value(filter).unwrap(), value);
            assert_eq!(
                serde_json::from_value::<ObservationTrendFilter>(serde_json::json!(value)).unwrap(),
                filter
            );
        }
        assert!(serde_json::from_str::<ObservationTrendFilter>("\"unknown\"").is_err());
    }

    #[test]
    fn disabled_skips_boundaries_workspace_and_ledger_io() {
        for settings in [
            Settings {
                ai_kb_enabled: false,
                ..Settings::default()
            },
            Settings {
                claude_kb_enabled: false,
                gpt_kb_enabled: false,
                ..Settings::default()
            },
        ] {
            for filter in FILTERS {
                let result = read_with(
                    &settings,
                    &[],
                    filter,
                    || panic!("OFFではworkspaceを読まない"),
                    0,
                    |_, _, _, _| panic!("OFFでは台帳を開かない"),
                );
                assert_eq!(result.status, ObservationHealthStatus::Disabled);
                assert!(result.days.is_empty());
            }
        }
    }

    #[test]
    fn invalid_windows_are_unavailable_before_io() {
        let (boundaries, now) = window();
        let mut cases = vec![
            Vec::new(),
            boundaries[..14].to_vec(),
            vec![0; 15],
            vec![i64::MAX; 15],
        ];
        let mut extra = boundaries.to_vec();
        extra.push(boundaries[14] + DAY_MS);
        cases.push(extra);
        let mut negative = boundaries.to_vec();
        negative[0] = -1;
        cases.push(negative);
        for offset in [-2 * HOUR_MS, 2 * HOUR_MS] {
            let mut invalid = boundaries.to_vec();
            invalid[1] += offset;
            cases.push(invalid);
        }
        cases.push(boundaries.iter().map(|at| at + DAY_MS).collect());
        cases.push(boundaries.iter().map(|at| at - DAY_MS).collect());
        for invalid in cases {
            let result = read_with(
                &Settings::default(),
                &invalid,
                ObservationTrendFilter::All,
                || panic!("不正な期間ではworkspaceを読まない"),
                now,
                |_, _, _, _| panic!("不正な期間では台帳を開かない"),
            );
            assert_eq!(result.status, ObservationHealthStatus::Unavailable);
            assert!(result.days.is_empty());
        }
    }

    #[test]
    fn half_open_days_include_midnight_once_and_stop_at_now() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("ledger.sqlite3");
        let (boundaries, now) = window();
        for at in [
            boundaries[0] - 1,
            boundaries[0],
            boundaries[1] - 1,
            boundaries[1],
            now - 1,
            now,
            now + 1,
        ] {
            write(&path, at, Some(WORKSPACE), ClientSurface::CodexCli);
        }
        let result = read_at(&path, &boundaries, now);
        assert_eq!(result.status, ObservationHealthStatus::Available);
        assert_eq!(result.days.len(), 14);
        assert_eq!(result.days[0].propose_successes, 2);
        assert_eq!(result.days[1].propose_successes, 1);
        assert_eq!(result.days[13].propose_successes, 1);
        assert_eq!(
            result
                .days
                .iter()
                .map(|day| day.propose_successes)
                .sum::<u64>(),
            4
        );
        assert_eq!(result.days[13].end_ms, now);
        assert!(
            result.days[2..13]
                .iter()
                .all(|day| day.propose_successes == 0)
        );
    }

    #[test]
    fn local_days_can_have_23_or_25_hours_and_today_can_be_empty_at_midnight() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("ledger.sqlite3");
        let (mut boundaries, _) = window();
        boundaries[4] -= HOUR_MS;
        let now = boundaries[13];
        for at in [boundaries[4] - 1, boundaries[4], now] {
            write(&path, at, Some(WORKSPACE), ClientSurface::CodexCli);
        }
        let result = read_at(&path, &boundaries, now);
        assert_eq!(result.status, ObservationHealthStatus::Available);
        assert_eq!(
            result.days[3].end_ms - result.days[3].start_ms,
            23 * HOUR_MS
        );
        assert_eq!(
            result.days[4].end_ms - result.days[4].start_ms,
            25 * HOUR_MS
        );
        assert_eq!(result.days[3].propose_successes, 1);
        assert_eq!(result.days[4].propose_successes, 1);
        assert_eq!(result.days[13].start_ms, result.days[13].end_ms);
        assert_eq!(result.days[13].propose_successes, 0);
    }

    #[test]
    fn selected_workspace_excludes_unassigned_and_preserves_disabled_family_history() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("ledger.sqlite3");
        let (boundaries, now) = window();
        write(&path, now - 10, Some(OTHER), ClientSurface::CodexCli);
        write(&path, now - 9, None, ClientSurface::CodexCli);
        for filter in FILTERS {
            assert_eq!(
                read_filtered_at(&path, &boundaries, now, filter).status,
                ObservationHealthStatus::NoObservations
            );
        }
        write(&path, now - 8, Some(WORKSPACE), ClientSurface::ClaudeCode);
        assert_eq!(
            read_filtered_at(&path, &boundaries, now, ObservationTrendFilter::Gpt).status,
            ObservationHealthStatus::NoObservations
        );
        write(&path, now - 7, Some(WORKSPACE), ClientSurface::CodexCli);
        for settings in [
            Settings {
                claude_kb_enabled: false,
                ..Settings::default()
            },
            Settings {
                gpt_kb_enabled: false,
                ..Settings::default()
            },
        ] {
            for filter in FILTERS {
                let result = read_with(
                    &settings,
                    &boundaries,
                    filter,
                    || Ok(WORKSPACE.into()),
                    now,
                    |boundaries, until, filter, workspace| {
                        session_ledger::trend::summarize_at(
                            &path, boundaries, until, filter, workspace,
                        )
                    },
                );
                assert_eq!(result.status, ObservationHealthStatus::Available);
                assert_eq!(
                    result
                        .days
                        .iter()
                        .map(|day| day.propose_successes)
                        .sum::<u64>(),
                    if filter == ObservationTrendFilter::All {
                        2
                    } else {
                        1
                    }
                );
            }
        }
    }

    #[test]
    fn family_filters_preserve_all_daily_metrics_and_only_all_includes_other_surfaces() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("ledger.sqlite3");
        let (boundaries, now) = window();
        for (day, surface) in [
            ClientSurface::ClaudeCode,
            ClientSurface::ClaudeDesktop,
            ClientSurface::CodexCli,
            ClientSurface::ChatGpt,
            ClientSurface::RuleDeliveryEvaluation,
        ]
        .into_iter()
        .enumerate()
        {
            let at = boundaries[day] + 1;
            let emitted = prepared_for_surface(&path, at, surface);
            session_ledger::finalize_hook_at(&path, &emitted, HookEmissionOutcome::Emitted)
                .unwrap();
            let failed = prepared_for_surface(&path, at + 1, surface);
            session_ledger::finalize_hook_at(&path, &failed, HookEmissionOutcome::StdoutFailed)
                .unwrap();
            let ctx = context(Some(WORKSPACE), surface);
            session_ledger::append_at(
                &path,
                &LedgerEvent::hook(
                    ctx,
                    at + 2,
                    HookObservation::Error {
                        stage: HookErrorStage::Search,
                        timings: HookTimings::default(),
                    },
                )
                .unwrap(),
            )
            .unwrap();
            for (offset, tool, outcome, code) in [
                (3, WriteTool::Propose, WriteOutcome::Success, None),
                (4, WriteTool::Update, WriteOutcome::Success, None),
                (
                    5,
                    WriteTool::Propose,
                    WriteOutcome::Error,
                    Some(WriteRejection::TagVocabulary),
                ),
                (6, WriteTool::Update, WriteOutcome::Error, None),
            ] {
                append_write(&path, at + offset, ctx, tool, outcome, code);
            }
        }
        let bytes = std::fs::read(&path).unwrap();
        for (filter, included_days) in [
            (ObservationTrendFilter::All, 0..5),
            (ObservationTrendFilter::Claude, 0..2),
            (ObservationTrendFilter::Gpt, 2..4),
        ] {
            let result = read_filtered_at(&path, &boundaries, now, filter);
            assert_eq!(result.status, ObservationHealthStatus::Available);
            assert_eq!(result.days.len(), 14);
            for (index, day) in result.days.iter().enumerate() {
                let count = u64::from(included_days.contains(&index));
                assert_eq!(
                    [
                        day.hook_output_emitted,
                        day.propose_successes,
                        day.update_successes,
                        day.errors
                    ],
                    [count, count, count, 4 * count],
                    "{filter:?} day {index}"
                );
            }
        }
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn outcomes_keep_prepared_separate_and_do_not_double_count_rejections() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("ledger.sqlite3");
        let (boundaries, now) = window();
        let at = boundaries[12] + 1;
        prepared(&path, at);
        for filter in [ObservationTrendFilter::All, ObservationTrendFilter::Gpt] {
            let pending = read_filtered_at(&path, &boundaries, now, filter);
            assert_eq!(pending.status, ObservationHealthStatus::Available);
            assert!(pending.days.iter().all(|day| day.hook_output_emitted
                + day.propose_successes
                + day.update_successes
                + day.errors
                == 0));
        }
        assert_eq!(
            read_filtered_at(&path, &boundaries, now, ObservationTrendFilter::Claude).status,
            ObservationHealthStatus::NoObservations
        );
        let emitted = prepared(&path, at + 1);
        session_ledger::finalize_hook_at(&path, &emitted, HookEmissionOutcome::Emitted).unwrap();
        let failed = prepared(&path, at + 2);
        session_ledger::finalize_hook_at(&path, &failed, HookEmissionOutcome::StdoutFailed)
            .unwrap();
        let ctx = context(Some(WORKSPACE), ClientSurface::CodexCli);
        for (offset, observation) in [
            (
                3,
                HookObservation::Filtered {
                    reason: HookFilterReason::ShortPrompt,
                    timings: HookTimings::default(),
                },
            ),
            (
                4,
                HookObservation::Error {
                    stage: HookErrorStage::Search,
                    timings: HookTimings::default(),
                },
            ),
        ] {
            session_ledger::append_at(
                &path,
                &LedgerEvent::hook(ctx, at + offset, observation).unwrap(),
            )
            .unwrap();
        }
        append_write(
            &path,
            at + 5,
            ctx,
            WriteTool::Propose,
            WriteOutcome::Error,
            Some(WriteRejection::TagVocabulary),
        );
        append_write(
            &path,
            at + 6,
            ctx,
            WriteTool::Update,
            WriteOutcome::Error,
            None,
        );
        append_write(
            &path,
            at + 7,
            ctx,
            WriteTool::Update,
            WriteOutcome::Success,
            None,
        );
        append_write(
            &path,
            at + 8,
            ctx,
            WriteTool::Propose,
            WriteOutcome::Success,
            None,
        );
        let result = read_at(&path, &boundaries, now);
        let day = &result.days[12];
        assert_eq!(day.hook_output_emitted, 1);
        assert_eq!(day.propose_successes, 1);
        assert_eq!(day.update_successes, 1);
        assert_eq!(day.errors, 4);
    }

    #[test]
    fn filtered_activity_is_observed_even_when_every_displayed_count_is_zero() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("ledger.sqlite3");
        let (boundaries, now) = window();
        session_ledger::append_at(
            &path,
            &LedgerEvent::hook(
                context(Some(WORKSPACE), ClientSurface::ClaudeCode),
                now - 1,
                HookObservation::Filtered {
                    reason: HookFilterReason::ShortPrompt,
                    timings: HookTimings::default(),
                },
            )
            .unwrap(),
        )
        .unwrap();
        for filter in [ObservationTrendFilter::All, ObservationTrendFilter::Claude] {
            let result = read_filtered_at(&path, &boundaries, now, filter);
            assert_eq!(result.status, ObservationHealthStatus::Available);
            assert_eq!(result.days.len(), 14);
            assert!(result.days.iter().all(|day| day.errors
                + day.hook_output_emitted
                + day.propose_successes
                + day.update_successes
                == 0));
        }
        let other = read_filtered_at(&path, &boundaries, now, ObservationTrendFilter::Gpt);
        assert_eq!(other.status, ObservationHealthStatus::NoObservations);
        assert!(other.days.is_empty());
    }

    #[test]
    fn session_start_only_and_missing_ledger_are_unobserved_without_creating_files() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("absent/ledger.sqlite3");
        let (boundaries, now) = window();
        let missing = read_at(&path, &boundaries, now);
        assert_eq!(missing.status, ObservationHealthStatus::NoObservations);
        assert!(missing.days.is_empty());
        assert!(!path.parent().unwrap().exists());
        session_ledger::record_session_start_at(
            &temp.path().join("session-starts.sqlite3"),
            WORKSPACE,
            Some("fixture"),
            Some("startup"),
            now - 1,
            now,
        )
        .unwrap();
        assert_eq!(
            read_at(&path, &boundaries, now).status,
            ObservationHealthStatus::NoObservations
        );
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn corrupt_schema_payload_and_receipt_are_unavailable_without_modification() {
        let temp = tempfile::tempdir().unwrap();
        let (boundaries, now) = window();
        for kind in [
            "corrupt", "future", "foreign", "payload", "index", "receipt", "unknown",
        ] {
            let path = temp.path().join(format!("{kind}.sqlite3"));
            if kind == "corrupt" {
                std::fs::write(&path, "not a database").unwrap();
            } else if kind == "foreign" {
                Connection::open(&path)
                    .unwrap()
                    .execute_batch("CREATE TABLE unrelated(data TEXT)")
                    .unwrap();
            } else {
                prepared(&path, now - 1);
                let sql = match kind {
                    "future" => "PRAGMA user_version = 999",
                    "payload" => "UPDATE ledger_events SET payload = '{}'",
                    "index" => "UPDATE ledger_events SET event_id = 'incorrect'",
                    "receipt" => "UPDATE ledger_events SET receipt_hash = 'incorrect'",
                    "unknown" => {
                        "UPDATE ledger_events SET payload = json_set(payload, '$.surface', 'unknown')"
                    }
                    _ => unreachable!(),
                };
                Connection::open(&path).unwrap().execute_batch(sql).unwrap();
            }
            let bytes = std::fs::read(&path).unwrap();
            for filter in FILTERS {
                let result = read_filtered_at(&path, &boundaries, now, filter);
                assert_eq!(
                    result.status,
                    ObservationHealthStatus::Unavailable,
                    "{kind} {filter:?}"
                );
                assert!(result.days.is_empty());
            }
            assert_eq!(std::fs::read(&path).unwrap(), bytes, "{kind}");
        }
    }

    #[test]
    fn legacy_schema_and_valid_counts_remain_read_only() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("ledger.sqlite3");
        let (boundaries, now) = window();
        write(&path, now - 1, Some(WORKSPACE), ClientSurface::CodexCli);
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", 1)
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(
            read_at(&path, &boundaries, now).status,
            ObservationHealthStatus::Available
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified
        );
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[test]
    fn unresolved_workspace_does_not_open_ledger() {
        let (boundaries, now) = window();
        let result = read_with(
            &Settings::default(),
            &boundaries,
            ObservationTrendFilter::All,
            || anyhow::bail!("workspace unavailable"),
            now,
            |_, _, _, _| panic!("workspaceなしでは台帳を読まない"),
        );
        assert_eq!(result.status, ObservationHealthStatus::Unavailable);
        assert!(result.days.is_empty());
    }
}
