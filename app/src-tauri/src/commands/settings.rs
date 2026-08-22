//! 端末設定。意味と永続化はkb-coreに置き、Tauriは薄い呼び出し口だけを持つ。

use crate::error::{AppError, AppResult};

#[tauri::command]
#[specta::specta]
pub fn settings_get() -> AppResult<kb_core::settings::Settings> {
    kb_core::settings::load().map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub fn settings_set_ai_kb_enabled(enabled: bool) -> AppResult<kb_core::settings::Settings> {
    kb_core::settings::set_ai_kb_enabled(enabled).map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub fn settings_set_claude_kb_enabled(enabled: bool) -> AppResult<kb_core::settings::Settings> {
    kb_core::settings::set_claude_kb_enabled(enabled).map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub fn settings_set_gpt_kb_enabled(enabled: bool) -> AppResult<kb_core::settings::Settings> {
    kb_core::settings::set_gpt_kb_enabled(enabled).map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub fn settings_ai_guard_status() -> AppResult<kb_core::ai_guard::AiGuardStatus> {
    kb_core::ai_guard::status().map_err(Into::into)
}

/// macOS の管理者領域へ、Codex と Claude Code が上書きできないポリシーを置く。
/// 既存の Codex requirements は管理者の正本なので、kb-app 所有でなければ止める。
#[tauri::command]
#[specta::specta]
pub fn settings_install_ai_guard() -> AppResult<kb_core::ai_guard::AiGuardStatus> {
    kb_core::ai_guard::install().map_err(|error| match error {
        kb_core::ai_guard::AiGuardInstallError::Conflict => AppError::AiGuardPolicyConflict,
        kb_core::ai_guard::AiGuardInstallError::StrictModeRequired => {
            AppError::AiGuardStrictModeRequired
        }
        kb_core::ai_guard::AiGuardInstallError::Unsupported => AppError::AiGuardUnsupported,
        kb_core::ai_guard::AiGuardInstallError::Failed(_) => AppError::AiGuardInstallFailed,
    })
}

/// Codex だけを Full Access に切り替える。Codex の KB 仲介は同時に fail-closed になる。
#[tauri::command]
#[specta::specta]
pub fn settings_enable_ai_guard_development_mode() -> AppResult<kb_core::ai_guard::AiGuardStatus> {
    kb_core::ai_guard::enable_development_mode().map_err(|error| match error {
        kb_core::ai_guard::AiGuardInstallError::Conflict => AppError::AiGuardPolicyConflict,
        kb_core::ai_guard::AiGuardInstallError::StrictModeRequired => {
            AppError::AiGuardStrictModeRequired
        }
        kb_core::ai_guard::AiGuardInstallError::Unsupported => AppError::AiGuardUnsupported,
        kb_core::ai_guard::AiGuardInstallError::Failed(_) => AppError::AiGuardInstallFailed,
    })
}
