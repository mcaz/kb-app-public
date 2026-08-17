//! 端末設定。意味と永続化はkb-coreに置き、Tauriは薄い呼び出し口だけを持つ。

use crate::error::AppResult;

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
