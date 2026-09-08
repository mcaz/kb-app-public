//! 復旧だけを行う別の管理画面。通常のDB接続・常駐・同期を起動しない。

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Mutex;

use kb_core::runtime_recovery::{
    RuntimeRecoveryPlan, RuntimeRecoveryReceipt, RuntimeRecoveryRequest,
};
use kb_core::{registry::Registry, settings, vault::Vault, workspace};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tauri_specta::collect_commands;

use crate::error::{AppError, AppResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum AppBootMode {
    Normal,
    StorageRecovery,
}

/// 起動引数から選んだnativeの固定値。URLやWebViewの保存値でモードを変えない。
#[tauri::command]
#[specta::specta]
pub fn app_boot_mode(mode: State<'_, AppBootMode>) -> AppBootMode {
    *mode
}

#[derive(Default)]
pub(crate) struct RecoverySession {
    target: Option<RecoveryTarget>,
    plan_digest: Option<String>,
    attempted: bool,
    closing: bool,
}

struct RecoveryTarget {
    root: PathBuf,
    workspace_id: String,
}

impl RecoveryTarget {
    fn selected() -> AppResult<Self> {
        let registry = Registry::load().map_err(AppError::configuration)?;
        let root = registry.resolve(None).map_err(AppError::vault)?;
        let root = root.canonicalize().map_err(AppError::vault)?;
        let vault = Vault::open(&root).map_err(AppError::vault)?;
        let workspace_id = workspace::stored_workspace_id(&vault).map_err(AppError::vault)?;
        Ok(Self { root, workspace_id })
    }

    fn verify(&self) -> AppResult<Vault> {
        let root = self.root.canonicalize().map_err(AppError::vault)?;
        let vault = Vault::open(&root).map_err(AppError::vault)?;
        let id = workspace::stored_workspace_id(&vault).map_err(AppError::vault)?;
        if root != self.root || id != self.workspace_id {
            return Err(AppError::vault(anyhow::anyhow!(
                "復旧対象の同一性が変わった"
            )));
        }
        Ok(vault)
    }
}

type RecoveryState = Mutex<RecoverySession>;

impl RecoverySession {
    fn require_open(&self) -> AppResult<()> {
        if self.closing {
            return Err(AppError::configuration(anyhow::anyhow!("復旧画面は終了中")));
        }
        Ok(())
    }
}

// main threadでworkerのMutexを待たない。終了が先なら後着の復旧を拒否し、
// 復旧が先ならtransactionと退避検証の終了まで通常のquitを受け付けない。
fn begin_exit(state: &RecoveryState) -> bool {
    match state.try_lock() {
        Ok(mut session) => {
            session.closing = true;
            true
        }
        Err(std::sync::TryLockError::WouldBlock) => false,
        Err(std::sync::TryLockError::Poisoned(_)) => true,
    }
}

fn require_paused() -> AppResult<()> {
    if settings::load()?.ai_kb_enabled {
        return Err(AppError::configuration(anyhow::anyhow!(
            "復旧中のAI利用停止が解除された"
        )));
    }
    Ok(())
}

#[tauri::command(async)]
#[specta::specta]
pub(crate) fn recovery_plan(state: State<'_, RecoveryState>) -> AppResult<RuntimeRecoveryPlan> {
    let mut session = state
        .lock()
        .map_err(|_| AppError::configuration(anyhow::anyhow!("復旧セッションを取得できない")))?;
    session.require_open()?;
    session.plan_digest = None;
    require_paused()?;
    if session.target.is_none() {
        session.target = Some(RecoveryTarget::selected()?);
    }
    let vault = session
        .target
        .as_ref()
        .ok_or(AppError::VaultUnavailable)?
        .verify()?;
    let report = kb_core::runtime_recovery::plan(&vault).map_err(AppError::index)?;
    session.plan_digest = report.plan_digest.clone();
    session.attempted = false;
    Ok(report)
}

#[tauri::command(async)]
#[specta::specta]
pub(crate) fn recovery_apply(
    state: State<'_, RecoveryState>,
    input: RuntimeRecoveryRequest,
) -> AppResult<RuntimeRecoveryReceipt> {
    let mut session = state
        .lock()
        .map_err(|_| AppError::configuration(anyhow::anyhow!("復旧セッションを取得できない")))?;
    session.require_open()?;
    if session.attempted || session.plan_digest.as_deref() != Some(&input.expected_plan_digest) {
        return Err(AppError::configuration(anyhow::anyhow!(
            "復旧にはこの画面での再照合が必要"
        )));
    }
    let vault = session
        .target
        .as_ref()
        .ok_or(AppError::VaultUnavailable)?
        .verify()?;
    // 結果が応答途中で失われても同じ確認を再利用しない。再照合で現状を確かめる。
    session.attempted = true;
    require_paused()?;
    kb_core::runtime_recovery::apply(&vault, &input).map_err(AppError::recovery)
}

#[tauri::command]
#[specta::specta]
pub(crate) fn recovery_exit(app: AppHandle, state: State<'_, RecoveryState>) {
    // master OFFを保持し、通常GUI・同期・自動蒸留は再開しない。
    if begin_exit(&state) {
        app.exit(0);
    }
}

/// 専用画面には復旧以外のcommandを登録しない。
pub(crate) fn builder() -> tauri_specta::Builder<tauri::Wry> {
    tauri_specta::Builder::<tauri::Wry>::new().commands(collect_commands![
        app_boot_mode,
        recovery_plan,
        recovery_apply,
        recovery_exit,
    ])
}

/// 型生成には全commandを集めるが、通常画面から復旧のセッションを起動させない。
pub(crate) fn normal_command_allowed(command: &str) -> bool {
    !matches!(
        command,
        "recovery_plan" | "recovery_apply" | "recovery_exit"
    )
}

pub fn run_if_requested() -> bool {
    match requested(std::env::args_os().skip(1).collect()) {
        Ok(false) => false,
        Ok(true) => {
            run();
            true
        }
        Err(()) => {
            eprintln!("kb-app: --storage-recoveryは他の起動引数と併用できません");
            std::process::exit(2);
        }
    }
}

fn requested(args: Vec<OsString>) -> Result<bool, ()> {
    if args.len() == 1 && args[0] == "--storage-recovery" {
        return Ok(true);
    }
    if args.iter().any(|arg| {
        arg == "--storage-recovery" || arg.to_string_lossy().starts_with("--storage-recovery=")
    }) {
        return Err(());
    }
    Ok(false)
}

fn run() {
    let commands = builder();
    tauri::Builder::default()
        .manage(AppBootMode::StorageRecovery)
        .manage(RecoveryState::default())
        .invoke_handler(commands.invoke_handler())
        .setup(|app| {
            // 停止済みMCPが再接続されても、新規KB要求を開始しない状態を保つ。
            // 受付済み旧処理の停止は起動手順で別途行い、この設定だけで代用しない。
            settings::set_ai_kb_enabled(false).map_err(AppError::from)?;
            if let Some(window) = app.get_webview_window("main") {
                window.show()?;
                window.set_focus()?;
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if begin_exit(&window.state::<RecoveryState>()) {
                    window.app_handle().exit(0);
                }
            }
        })
        .build(crate::app_context())
        .expect("recovery window initialization failed")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event
                && !begin_exit(&app.state::<RecoveryState>())
            {
                api.prevent_exit();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-09-07: 復旧反映の途中で通常のquitがtransactionを打ち切らない。
    #[test]
    fn exit_does_not_wait_for_active_recovery_and_prevents_late_commands() {
        let state = RecoveryState::default();
        let session = state.lock().unwrap();
        assert!(!begin_exit(&state));
        assert!(session.require_open().is_ok());
        drop(session);
        assert!(begin_exit(&state));
        assert!(state.lock().unwrap().require_open().is_err());
    }

    #[test]
    fn normal_mode_rejects_every_recovery_session_command() {
        for command in ["recovery_plan", "recovery_apply", "recovery_exit"] {
            assert!(!normal_command_allowed(command));
        }
        assert!(normal_command_allowed("app_boot_mode"));
        assert!(normal_command_allowed("setup_state"));
    }

    #[test]
    fn recovery_argument_cannot_mix_with_mcp_hooks_or_a_selected_path() {
        assert_eq!(requested(vec!["--storage-recovery".into()]), Ok(true));
        for args in [
            vec!["--mcp", "--storage-recovery"],
            vec!["--storage-recovery", "--vault", "other"],
            vec!["--storage-recovery", "--hidden"],
            vec!["--hook-event", "start", "--storage-recovery"],
            vec!["--storage-recovery=true"],
        ] {
            assert_eq!(
                requested(args.into_iter().map(OsString::from).collect()),
                Err(())
            );
        }
        assert_eq!(requested(Vec::new()), Ok(false));
        assert_eq!(requested(vec!["--mcp".into()]), Ok(false));
    }

    #[test]
    fn recovery_target_is_pinned_even_when_an_equivalent_vault_replaces_it() {
        let dir = tempfile::tempdir().unwrap();
        let first = Vault::create(dir.path().join("first")).unwrap();
        let target = RecoveryTarget {
            root: first.root.canonicalize().unwrap(),
            workspace_id: workspace::stored_workspace_id(&first).unwrap(),
        };
        assert!(target.verify().is_ok());
        std::fs::write(
            first.root.join(workspace::ID_FILE),
            "01ARZ3NDEKTSV4RRFFQ69G5FAV\n",
        )
        .unwrap();
        assert!(target.verify().is_err());
    }
}
