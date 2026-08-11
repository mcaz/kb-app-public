//! 起動時の状態とオンボーディング(FR-A1)。

use kb_core::registry::Registry;
use kb_core::vault::Vault;
use serde::Serialize;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Serialize, specta::Type)]
pub struct SetupState {
    needs_onboarding: bool,
    vault_name: Option<String>,
    vault_path: Option<String>,
}

#[tauri::command]
#[specta::specta]
pub fn setup_state() -> AppResult<SetupState> {
    let reg = Registry::load().map_err(AppError::from)?;
    match reg.resolve(None) {
        Ok(path) => Ok(SetupState {
            needs_onboarding: false,
            vault_name: reg
                .vaults
                .iter()
                .find(|v| v.path == path)
                .map(|v| v.name.clone()),
            vault_path: Some(path.display().to_string()),
        }),
        Err(_) => Ok(SetupState {
            needs_onboarding: true,
            vault_name: None,
            vault_path: None,
        }),
    }
}

/// 最初の vault を自動作成(既定名「わたしのノート」実体 my-notes)。
#[tauri::command]
#[specta::specta]
pub fn onboard(state: State<'_, AppState>) -> AppResult<SetupState> {
    let path = dirs::home_dir()
        .ok_or_else(|| AppError::VaultUnavailable {
            message: "home が特定できない".into(),
        })?
        .join("kb")
        .join("my-notes");
    let vault = Vault::create(&path).map_err(AppError::from)?;
    let mut reg = Registry::load().map_err(AppError::from)?;
    reg.add("my-notes", vault.root.clone())
        .map_err(AppError::from)?;
    reg.save().map_err(AppError::from)?;
    // 作ったばかりの vault を次のコマンドから使えるようにする
    state.reset();
    setup_state()
}
