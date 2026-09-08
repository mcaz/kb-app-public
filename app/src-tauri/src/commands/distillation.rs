//! 自動蒸留の端末設定と進行状況。AI実行とノートの変更はバックグラウンド処理へ委ねる。

use kb_core::distillation_ai::{
    self, AiRunError, DistillationAiProvider, DistillationAiProviderStatus, DistillationAiSettings,
    DistillationModelCatalog,
};
use kb_core::distillation_jobs::{
    self, ImmediateDistillationResult, ImmediateDistillationScope, JobIssue, JobStatus,
};
use kb_core::distillation_metrics::{self, MetricsView};
use serde::Serialize;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Serialize, specta::Type)]
pub struct DistillationQueueView {
    pub paused: bool,
    pub jobs: Option<JobStatus>,
    pub issues: Vec<DistillationIssueView>,
    pub metrics: Option<MetricsView>,
}

#[derive(Debug, Serialize, specta::Type)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum DistillationIssueError {
    Ai { kind: AiRunError },
    ContextLimit,
    ContextSizeLimit,
    ReviewRoundLimit,
    ReviewNoProgress,
    NotSupported,
    ReviewFailed,
    NeedsReview { reason: String },
}

#[derive(Serialize, specta::Type)]
pub struct DistillationIssueView {
    pub note: String,
    pub title: Option<String>,
    pub state: String,
    pub error: DistillationIssueError,
    pub available_at: i64,
    pub attempt: u32,
}

#[tauri::command]
#[specta::specta]
pub fn distillation_settings_get() -> AppResult<DistillationAiSettings> {
    distillation_ai::load().map_err(Into::into)
}

#[tauri::command(async)]
#[specta::specta]
pub fn distillation_settings_set(
    settings: DistillationAiSettings,
) -> AppResult<DistillationAiSettings> {
    distillation_ai::save(settings).map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub fn distillation_providers() -> Vec<DistillationAiProviderStatus> {
    distillation_ai::providers()
}

#[tauri::command(async)]
#[specta::specta]
pub fn distillation_models(provider: DistillationAiProvider) -> DistillationModelCatalog {
    distillation_ai::models(provider)
}

#[tauri::command(async)]
#[specta::specta]
pub fn distillation_queue_status(state: State<'_, AppState>) -> AppResult<DistillationQueueView> {
    with_enabled_queue(|| state.with_db(|vault, conn, _| queue_contents(vault, conn)))
}

#[tauri::command(async)]
#[specta::specta]
pub fn distillation_retry_failed(state: State<'_, AppState>) -> AppResult<DistillationQueueView> {
    with_enabled_queue(|| {
        state.with_db(|vault, conn, _| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(AppError::unexpected)?
                .as_secs();
            let now = i64::try_from(now).map_err(AppError::unexpected)?;
            distillation_jobs::retry_failed(conn, now).map_err(AppError::storage)?;
            queue_contents(vault, conn)
        })
    })
}

#[tauri::command(async)]
#[specta::specta]
pub fn distillation_request_now(
    state: State<'_, AppState>,
    scope: ImmediateDistillationScope,
) -> AppResult<ImmediateDistillationResult> {
    let config = distillation_ai::load()?;
    let kb = kb_core::settings::load()?;
    with_ready_request(&config, &kb, distillation_ai::providers, || {
        state.with_db(|_, conn, _| {
            // 設定画面とDBの待機中に停止された場合も、実行待ちを新しく作らない。
            if distillation_ai::load()? != config
                || !distillation_ai::kb_enabled(&config, &kb_core::settings::load()?)
            {
                return Err(AppError::configuration(anyhow::anyhow!(
                    "蒸留要求の受付中に設定が変わった"
                )));
            }
            distillation_jobs::request_now(conn, scope, kb_core::auto_distillation::now_seconds())
                .map_err(AppError::storage)
        })
    })
}

fn with_ready_request<T>(
    config: &DistillationAiSettings,
    kb: &kb_core::settings::Settings,
    providers: impl FnOnce() -> Vec<DistillationAiProviderStatus>,
    request: impl FnOnce() -> AppResult<T>,
) -> AppResult<T> {
    // 即時実行も保存済みの常駐workerを使う。OFFの状態でVaultを開かない。
    if !config.enabled || !distillation_ai::kb_enabled(config, kb) {
        return Err(AppError::configuration(anyhow::anyhow!(
            "自動蒸留と選択AIのKB利用を有効にして保存する必要がある"
        )));
    }
    config.validate()?;
    if !providers().iter().any(|provider| {
        Some(provider.provider) == config.provider
            && provider.installed
            && provider.unavailable_reason.is_none()
    }) {
        return Err(AppError::configuration(anyhow::anyhow!(
            "保存済みの蒸留AIをこの端末で利用できない"
        )));
    }
    request()
}

fn with_enabled_queue(
    read: impl FnOnce() -> AppResult<DistillationQueueView>,
) -> AppResult<DistillationQueueView> {
    let settings = distillation_ai::load()?;
    let kb = kb_core::settings::load()?;
    // OFFの判定をAppStateへのアクセスより先に行い、Vaultの遅延初期化も起動しない。
    queue_view(distillation_ai::kb_enabled(&settings, &kb), read)
}

fn queue_view(
    enabled: bool,
    read: impl FnOnce() -> AppResult<DistillationQueueView>,
) -> AppResult<DistillationQueueView> {
    if !enabled {
        return Ok(DistillationQueueView {
            paused: true,
            jobs: None,
            issues: Vec::new(),
            metrics: None,
        });
    }
    read()
}

fn queue_contents(
    vault: &kb_core::vault::Vault,
    conn: &kb_core::rusqlite::Connection,
) -> AppResult<DistillationQueueView> {
    Ok(DistillationQueueView {
        paused: false,
        jobs: Some(distillation_jobs::status(conn).map_err(AppError::storage)?),
        issues: distillation_jobs::issues(conn, 20)
            .map_err(AppError::storage)?
            .into_iter()
            .map(issue_view)
            .collect(),
        metrics: Some(
            distillation_metrics::read(vault, conn).unwrap_or_else(|_| MetricsView::unavailable()),
        ),
    })
}

fn issue_view(issue: JobIssue) -> DistillationIssueView {
    let error = issue_error(&issue.state, issue.last_error.as_deref());
    DistillationIssueView {
        note: issue.note,
        title: issue.title,
        state: issue.state,
        error,
        available_at: issue.available_at,
        attempt: issue.attempt,
    }
}

fn issue_error(state: &str, message: Option<&str>) -> DistillationIssueError {
    let Some(message) = message else {
        return DistillationIssueError::ReviewFailed;
    };
    // AIが明示した保留理由だけを本文データとして渡す。コアやCLIの診断は翻訳用codeに閉じる。
    if state == "blocked"
        && let Some(reason) = message.strip_prefix("review_blocked:")
    {
        let reason = reason.trim();
        if !reason.is_empty() && !reason.contains(['\n', '\r']) {
            return DistillationIssueError::NeedsReview {
                reason: reason.chars().take(500).collect(),
            };
        }
    }
    match message {
        "context_limit" => DistillationIssueError::ContextLimit,
        "context_size_limit" => DistillationIssueError::ContextSizeLimit,
        "review_round_limit" => DistillationIssueError::ReviewRoundLimit,
        "review_no_progress" => DistillationIssueError::ReviewNoProgress,
        "review_not_supported" => DistillationIssueError::NotSupported,
        _ => serde_json::from_value::<AiRunError>(serde_json::Value::String(message.into()))
            .map(|kind| DistillationIssueError::Ai { kind })
            .unwrap_or(DistillationIssueError::ReviewFailed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-09-06: 設定画面の定期取得や再試行でも、KB OFF時にVaultを開かない。
    #[test]
    fn disabled_queue_does_not_open_or_read_the_vault() {
        let view = queue_view(false, || panic!("KB OFF時にはVaultへ触れない")).unwrap();
        assert!(view.paused);
        assert!(view.jobs.is_none());
        assert!(view.issues.is_empty());
        assert!(view.metrics.is_none());
    }

    /// 2026-09-07: 計測だけが読めない場合も、待機件数と失敗一覧を使い続けられる。
    #[test]
    fn metrics_failure_does_not_hide_the_job_queue() {
        let dir = tempfile::tempdir().unwrap();
        let vault = kb_core::vault::Vault {
            root: dir.path().to_path_buf(),
        };
        let conn = kb_core::rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE distillation_jobs(
                note TEXT, state TEXT, queued_at INTEGER, available_at INTEGER,
                attempt INTEGER, last_error TEXT
             );
             CREATE TABLE notes(
                id TEXT, title TEXT, normal_reference_allowed INTEGER,
                distillation_allowed INTEGER, document TEXT
             );
             INSERT INTO distillation_jobs VALUES('notes/test', 'pending', 1, 0, 0, NULL);",
        )
        .unwrap();
        let view = queue_contents(&vault, &conn).unwrap();
        assert!(!view.paused);
        assert_eq!(view.jobs.unwrap().pending, 1);
        assert!(view.issues.is_empty());
        assert!(!view.metrics.unwrap().available);
        // 欠けた保管庫IDを自動修復してから計測データを読みに行かない。
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    /// 2026-09-07: 即時ボタンでも停止中・未設定・保護不可ならVaultアクセス前に拒否する。
    #[test]
    fn immediate_request_checks_saved_settings_before_opening_the_vault() {
        let config = DistillationAiSettings {
            enabled: true,
            provider: Some(DistillationAiProvider::Codex),
            ..Default::default()
        };
        let kb = kb_core::settings::Settings::default();
        let provider = DistillationAiProviderStatus {
            provider: DistillationAiProvider::Codex,
            installed: true,
            unavailable_reason: None,
        };
        for (config, kb) in [
            (DistillationAiSettings::default(), kb),
            (
                DistillationAiSettings {
                    provider: None,
                    ..config.clone()
                },
                kb,
            ),
            (
                config.clone(),
                kb_core::settings::Settings {
                    ai_kb_enabled: false,
                    ..kb
                },
            ),
            (
                config.clone(),
                kb_core::settings::Settings {
                    gpt_kb_enabled: false,
                    ..kb
                },
            ),
        ] {
            let result: AppResult<()> = with_ready_request(
                &config,
                &kb,
                || panic!("OFF時は実行環境も調べない"),
                || panic!("OFF時はVaultを開かない"),
            );
            assert!(result.is_err());
        }
        for provider in [
            DistillationAiProviderStatus {
                installed: false,
                ..provider.clone()
            },
            DistillationAiProviderStatus {
                unavailable_reason: Some(AiRunError::GuardUnavailable),
                ..provider.clone()
            },
        ] {
            let result: AppResult<()> = with_ready_request(
                &config,
                &kb,
                || vec![provider],
                || panic!("利用不可ならVaultを開かない"),
            );
            assert!(result.is_err());
        }
        assert_eq!(
            with_ready_request(&config, &kb, || vec![provider], || Ok("accepted")).unwrap(),
            "accepted",
        );
    }

    /// 2026-09-06: 失敗一覧にノート本文を含み得る生診断を混ぜない。
    #[test]
    fn issue_errors_only_expose_explicit_review_reasons_and_stable_codes() {
        let unexpected = issue_error("retry_wait", Some("private diagnostic /some/path"));
        assert_eq!(
            serde_json::to_string(&unexpected).unwrap(),
            r#"{"code":"review_failed"}"#,
        );
        assert!(matches!(
            issue_error("retry_wait", Some("timed_out")),
            DistillationIssueError::Ai {
                kind: AiRunError::TimedOut
            }
        ));
        assert!(matches!(
            issue_error("blocked", Some("context_limit")),
            DistillationIssueError::ContextLimit
        ));
        assert!(matches!(
            issue_error("blocked", Some("review_blocked: 出典の確認が必要")),
            DistillationIssueError::NeedsReview { reason } if reason == "出典の確認が必要"
        ));
        for state in ["retry_wait", "running"] {
            assert!(matches!(
                issue_error(state, Some("review_blocked: private diagnostic")),
                DistillationIssueError::ReviewFailed
            ));
        }
        assert!(matches!(
            issue_error("blocked", Some("review_blocked: first\nsecond")),
            DistillationIssueError::ReviewFailed
        ));
    }

    /// 2026-09-07: 短いレシピの探索停止を本文容量の超過と誤表示しない。
    #[test]
    fn review_limits_keep_size_round_and_progress_causes_separate() {
        for code in [
            "context_limit",
            "context_size_limit",
            "review_round_limit",
            "review_no_progress",
        ] {
            let error = issue_error("blocked", Some(code));
            assert_eq!(serde_json::to_value(error).unwrap()["code"], code);
        }
    }
}
