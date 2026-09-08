//! Homeは観測できた数を表示する。未取得・OFF・未分類をゼロや健康判定へ潰さない。

use serde::Serialize;

use crate::client_surface::{ClientFamily, ClientSurface};
use crate::session_ledger::{self, LedgerSummary, RETENTION_DAYS, SummaryQuery, SurfaceSummary};
use crate::settings::Settings;

const PERIOD_DAYS: i64 = 14;
const DAY_MS: i64 = 86_400_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ObservationHealthStatus {
    Available,
    NoObservations,
    Disabled,
    Unavailable,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ObservationSurfaceHealth {
    pub surface: ClientSurface,
    pub kb_enabled: bool,
    pub counts: SurfaceSummary,
    pub last_propose_days: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ObservationHealth {
    pub status: ObservationHealthStatus,
    pub period_days: i64,
    pub retention_days: i64,
    pub surfaces: Vec<ObservationSurfaceHealth>,
    pub unassigned: Vec<ObservationSurfaceHealth>,
}

impl ObservationHealth {
    pub fn unavailable() -> Self {
        Self::empty(ObservationHealthStatus::Unavailable)
    }

    fn empty(status: ObservationHealthStatus) -> Self {
        Self {
            status,
            period_days: PERIOD_DAYS,
            retention_days: RETENTION_DAYS,
            surfaces: Vec::new(),
            unassigned: Vec::new(),
        }
    }
}

/// workspace解決も遅延する。OFF時にVaultや台帳を開く呼出しを口側へ要求しない。
pub fn read(
    settings: &Settings,
    workspace: impl FnOnce() -> anyhow::Result<String>,
) -> ObservationHealth {
    read_with(
        settings,
        workspace,
        session_ledger::now_ms(),
        session_ledger::summary,
    )
}

fn read_with(
    settings: &Settings,
    workspace: impl FnOnce() -> anyhow::Result<String>,
    now: i64,
    mut summarize: impl FnMut(&SummaryQuery) -> anyhow::Result<LedgerSummary>,
) -> ObservationHealth {
    if !settings.ai_kb_enabled || (!settings.claude_kb_enabled && !settings.gpt_kb_enabled) {
        return ObservationHealth::empty(ObservationHealthStatus::Disabled);
    }
    let Ok(workspace_id) = workspace() else {
        return ObservationHealth::unavailable();
    };
    let query = |days| SummaryQuery {
        since_ms: now.saturating_sub(days * DAY_MS).max(0),
        until_ms: now,
        workspace_id: Some(workspace_id.clone()),
    };
    let Ok(history) = summarize(&query(RETENTION_DAYS)) else {
        return ObservationHealth::unavailable();
    };
    if !history.exists {
        return ObservationHealth::empty(ObservationHealthStatus::NoObservations);
    }
    let Ok(window) = summarize(&query(PERIOD_DAYS)) else {
        return ObservationHealth::unavailable();
    };
    if !window.exists {
        return ObservationHealth::unavailable();
    }
    // 台帳のworkspace絞り込みに加えここでも照合し、他のKBの集計を混ぜない。
    let selected = |summary: LedgerSummary| {
        summary
            .workspaces
            .into_iter()
            .find(|entry| entry.workspace_id == workspace_id)
            .map(|entry| entry.surfaces)
            .unwrap_or_default()
    };
    let unassigned = combine(
        window.unassigned.clone(),
        &history.unassigned,
        settings,
        now,
    );
    let surfaces = combine(selected(window), &selected(history), settings, now);
    ObservationHealth {
        status: if surfaces.is_empty() && unassigned.is_empty() {
            ObservationHealthStatus::NoObservations
        } else {
            ObservationHealthStatus::Available
        },
        surfaces,
        unassigned,
        ..ObservationHealth::empty(ObservationHealthStatus::NoObservations)
    }
}

fn combine(
    mut window: Vec<SurfaceSummary>,
    history: &[SurfaceSummary],
    settings: &Settings,
    now: i64,
) -> Vec<ObservationSurfaceHealth> {
    // 最近14日に書込がなくても、保持期間内の最後の成功時刻は消さない。
    for previous in history
        .iter()
        .filter(|row| row.last_successful_propose_at_ms.is_some())
    {
        if !window.iter().any(|row| row.surface == previous.surface) {
            window.push(SurfaceSummary::empty(previous.surface));
        }
    }
    window
        .into_iter()
        .map(|counts| {
            let surface = counts.surface;
            let kb_enabled = match surface.family() {
                ClientFamily::Claude => settings.claude_kb_enabled,
                ClientFamily::Gpt => settings.gpt_kb_enabled,
                ClientFamily::Other => false,
            };
            let last = history
                .iter()
                .find(|row| row.surface == surface)
                .and_then(|row| row.last_successful_propose_at_ms)
                // 2回の集計の間にcommitされた成功を、古いsnapshotで打ち消さない。
                .max(counts.last_successful_propose_at_ms);
            ObservationSurfaceHealth {
                surface,
                kb_enabled,
                last_propose_days: last
                    .map(|at| now.saturating_sub(at).max(0) as u64 / DAY_MS as u64),
                counts,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_ledger::{EventContext, LedgerEvent, WriteOutcome, WriteTool};
    use std::path::Path;

    const WORKSPACE_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const WORKSPACE_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

    fn append(path: &Path, surface: ClientSurface, workspace: Option<&str>, now: i64, days: i64) {
        let context = EventContext {
            surface,
            workspace_id: workspace,
            session_id: None,
            prompt_id: None,
            turn_id: None,
            permission_mode: None,
        };
        session_ledger::append_at(
            path,
            &LedgerEvent::write(
                context,
                now - days * DAY_MS,
                "fixture-call",
                WriteTool::Propose,
                WriteOutcome::Success,
                None,
            )
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn disabled_does_not_resolve_workspace_or_open_ledger() {
        let now = session_ledger::now_ms();
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
            let result = read_with(
                &settings,
                || panic!("OFF時にworkspaceを読まない"),
                now,
                |_| panic!("OFF時に台帳を開かない"),
            );
            assert_eq!(result.status, ObservationHealthStatus::Disabled);
            assert!(result.surfaces.is_empty());
        }
    }

    #[test]
    fn missing_and_broken_ledgers_are_distinct_and_read_only() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("observations.sqlite3");
        let now = session_ledger::now_ms();
        let read = || {
            read_with(
                &Settings::default(),
                || Ok(WORKSPACE_A.into()),
                now,
                |query| session_ledger::summary_at(&path, query),
            )
        };
        assert_eq!(read().status, ObservationHealthStatus::NoObservations);
        assert!(!path.exists());
        std::fs::write(&path, b"broken ledger").unwrap();
        assert_eq!(read().status, ObservationHealthStatus::Unavailable);
        assert_eq!(std::fs::read(&path).unwrap(), b"broken ledger");
        let result = read_with(
            &Settings::default(),
            || anyhow::bail!("workspace missing"),
            now,
            |_| panic!("workspace不明なら台帳を開かない"),
        );
        assert_eq!(result.status, ObservationHealthStatus::Unavailable);
    }

    #[test]
    fn recent_counts_history_and_workspace_boundaries_stay_separate() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("observations.sqlite3");
        let now = session_ledger::now_ms();
        append(&path, ClientSurface::CodexCli, Some(WORKSPACE_A), now, 20);
        append(&path, ClientSurface::ClaudeCode, Some(WORKSPACE_A), now, 2);
        append(&path, ClientSurface::CodexCli, Some(WORKSPACE_B), now, 1);
        append(&path, ClientSurface::CodexCli, None, now, 3);
        let settings = Settings {
            claude_kb_enabled: false,
            ..Settings::default()
        };
        let result = read_with(
            &settings,
            || Ok(WORKSPACE_A.into()),
            now,
            |query| session_ledger::summary_at(&path, query),
        );
        assert_eq!(result.status, ObservationHealthStatus::Available);
        assert_eq!(result.surfaces.len(), 2);
        let codex = result
            .surfaces
            .iter()
            .find(|row| row.surface == ClientSurface::CodexCli)
            .unwrap();
        assert_eq!(codex.counts.propose_successes, 0);
        assert_eq!(codex.last_propose_days, Some(20));
        assert!(codex.kb_enabled);
        let claude = result
            .surfaces
            .iter()
            .find(|row| row.surface == ClientSurface::ClaudeCode)
            .unwrap();
        assert_eq!(claude.counts.propose_successes, 1);
        assert_eq!(claude.last_propose_days, Some(2));
        assert!(!claude.kb_enabled);
        assert_eq!(result.unassigned.len(), 1);
        assert_eq!(result.unassigned[0].counts.propose_successes, 1);
        assert_eq!(result.unassigned[0].last_propose_days, Some(3));
    }

    /// 2026-09-05: 2回目の読取り失敗や台帳消失を、履歴付きのゼロ件表示へ変換しない。
    #[test]
    fn failed_or_missing_recent_read_never_returns_partial_history_as_zero_counts() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("observations.sqlite3");
        let now = session_ledger::now_ms();
        append(&path, ClientSurface::CodexCli, Some(WORKSPACE_A), now, 20);
        for missing in [false, true] {
            let mut calls = 0;
            let result = read_with(
                &Settings::default(),
                || Ok(WORKSPACE_A.into()),
                now,
                |query| {
                    calls += 1;
                    if calls == 2 {
                        if missing {
                            return session_ledger::summary_at(
                                &temp.path().join("missing.sqlite3"),
                                query,
                            );
                        }
                        anyhow::bail!("read failed")
                    }
                    session_ledger::summary_at(&path, query)
                },
            );
            assert_eq!(result.status, ObservationHealthStatus::Unavailable);
            assert!(result.surfaces.is_empty());
            assert_eq!(calls, 2);
        }
    }

    /// 2026-09-05: 90日と14日の集計間に成功がcommitされても、成功なしや古い経過日へ戻さない。
    #[test]
    fn a_success_between_snapshots_updates_the_last_propose_for_both_groups() {
        let now = session_ledger::now_ms();
        for previous in [None, Some(now - 20 * DAY_MS)] {
            let mut calls = 0;
            let result = read_with(
                &Settings::default(),
                || Ok(WORKSPACE_A.into()),
                now,
                |query| {
                    calls += 1;
                    let last = if calls == 1 { previous } else { Some(now - 1) };
                    let rows = || {
                        last.map(|at| {
                            let mut row = SurfaceSummary::empty(ClientSurface::CodexCli);
                            row.propose_successes = 1;
                            row.last_successful_propose_at_ms = Some(at);
                            row
                        })
                        .into_iter()
                        .collect()
                    };
                    Ok(LedgerSummary {
                        exists: true,
                        since_ms: query.since_ms,
                        until_ms: query.until_ms,
                        retention_days: RETENTION_DAYS,
                        workspaces: vec![session_ledger::WorkspaceSummary {
                            workspace_id: WORKSPACE_A.into(),
                            surfaces: rows(),
                        }],
                        unassigned: rows(),
                    })
                },
            );
            assert_eq!(calls, 2);
            assert_eq!(result.status, ObservationHealthStatus::Available);
            for group in [&result.surfaces, &result.unassigned] {
                assert_eq!(group.len(), 1);
                assert_eq!(group[0].counts.propose_successes, 1);
                assert_eq!(group[0].last_propose_days, Some(0));
            }
        }
    }
}
