//! 常駐まわりの呼び出し口 — ログイン自動起動とtrayメニューの文言(ADR-0017)。
//!
//! 登録の意味と OS 別の置き場は `kb_core::autostart` が持つ。ここは実行ファイルの
//! 位置を渡すだけにする。

use tauri::AppHandle;

use crate::background;
use crate::error::{AppError, AppResult};

#[tauri::command]
#[specta::specta]
pub fn autostart_status() -> AppResult<kb_core::autostart::AutostartState> {
    kb_core::autostart::state(&current_exe()?).map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub fn autostart_set(enabled: bool) -> AppResult<kb_core::autostart::AutostartState> {
    kb_core::autostart::set(&current_exe()?, enabled).map_err(Into::into)
}

/// trayメニューを画面と同じ言語にする。言語設定はwebview側にしかないので、
/// 起動時と切り替え時にフロントから渡す(常駐開始直後だけ既定の日本語が出る)。
#[tauri::command]
#[specta::specta]
pub fn tray_set_labels(app: AppHandle, show: String, quit: String) -> AppResult<()> {
    background::apply_labels(&app, &show, &quit)
}

fn current_exe() -> AppResult<std::path::PathBuf> {
    std::env::current_exe().map_err(|error| AppError::configuration(anyhow::Error::new(error)))
}
