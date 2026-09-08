//! ウィンドウ内のタブ操作メニュー。利用者の言語と画面の準備状態を受け取る。

use tauri::AppHandle;

use crate::error::AppResult;

#[tauri::command]
#[specta::specta]
pub fn workspace_tab_shortcuts_configure(
    app: AppHandle,
    enabled: bool,
    file: String,
    new_tab: String,
    close_tab: String,
) -> AppResult<()> {
    crate::workspace_tabs::configure(&app, enabled, &file, &new_tab, &close_tab)
}
