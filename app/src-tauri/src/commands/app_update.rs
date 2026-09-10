//! archiveや互換性の判断をUIから指定できない、更新セッションへの薄い入口。

use tauri::State;

use crate::app_update_runtime::{UpdateState, UpdateStatus};
use crate::error::AppResult;

#[tauri::command(async)]
#[specta::specta]
pub fn app_update_status(state: State<'_, UpdateState>) -> AppResult<UpdateStatus> {
    state.status()
}

#[tauri::command]
#[specta::specta]
pub async fn app_update_check(
    app: tauri::AppHandle,
    state: State<'_, UpdateState>,
) -> AppResult<UpdateStatus> {
    state.check(app).await
}

#[tauri::command]
#[specta::specta]
pub async fn app_update_download(state: State<'_, UpdateState>) -> AppResult<UpdateStatus> {
    state.download().await
}

#[tauri::command]
#[specta::specta]
pub async fn app_update_install(
    app: tauri::AppHandle,
    state: State<'_, UpdateState>,
) -> AppResult<UpdateStatus> {
    state.install(app).await
}

#[tauri::command(async)]
#[specta::specta]
pub fn app_update_boot_ready() -> AppResult<()> {
    crate::app_update_supervisor::boot_ready()
}
