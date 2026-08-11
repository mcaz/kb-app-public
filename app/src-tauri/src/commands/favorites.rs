//! お気に入り(一覧の絞り込み状態の保存)。vault ごとに持つ。

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[tauri::command]
#[specta::specta]
pub fn favorites_list(state: State<'_, AppState>) -> AppResult<Vec<kb_core::favorites::Favorite>> {
    Ok(kb_core::favorites::list(&state.vault_name()?))
}

#[tauri::command]
#[specta::specta]
pub fn favorite_add(
    state: State<'_, AppState>,
    fav: kb_core::favorites::Favorite,
) -> AppResult<()> {
    kb_core::favorites::add(&state.vault_name()?, fav).map_err(AppError::from)
}

#[tauri::command]
#[specta::specta]
pub fn favorite_remove(state: State<'_, AppState>, name: String) -> AppResult<()> {
    kb_core::favorites::remove(&state.vault_name()?, &name).map_err(AppError::from)
}
