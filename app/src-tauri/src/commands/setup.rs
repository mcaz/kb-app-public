//! 起動時の状態とオンボーディング(FR-A1)。

use kb_core::registry::Registry;
use kb_core::vault::Vault;
use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_specta::Event;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Serialize, specta::Type)]
pub struct SetupState {
    needs_onboarding: bool,
    vault_name: Option<String>,
    vault_path: Option<String>,
}

/// 既存Vaultの検査・clone・Full Artifact復元の進捗。
#[derive(Clone, Serialize, specta::Type, Event)]
pub struct VaultRestoreProgress {
    phase: kb_core::connect::RestorePhase,
    completed: usize,
    total: usize,
    fetched: usize,
    reused: usize,
}

impl From<kb_core::connect::RestoreProgress> for VaultRestoreProgress {
    fn from(progress: kb_core::connect::RestoreProgress) -> Self {
        Self {
            phase: progress.phase,
            completed: progress.completed,
            total: progress.total,
            fetched: progress.fetched,
            reused: progress.reused,
        }
    }
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

/// 2台目以降: private GitHub repository にある既存 Vault を検査・復元して登録する。
/// 復元先は未使用 path を選び、既存フォルダへ overlay しない。
#[tauri::command]
#[specta::specta]
pub fn onboard_existing(
    app: AppHandle,
    state: State<'_, AppState>,
    url: String,
) -> AppResult<SetupState> {
    let repository = kb_core::github::parse_repository_url(&url).map_err(AppError::backup)?;
    let root = dirs::home_dir()
        .ok_or_else(|| AppError::VaultUnavailable {
            message: "home が特定できない".into(),
        })?
        .join("kb");
    let mut registry = Registry::load().map_err(AppError::from)?;
    let (name, path) = (1usize..)
        .map(|number| {
            let name = if number == 1 {
                repository.repo.clone()
            } else {
                format!("{}-{number}", repository.repo)
            };
            let path = root.join(&name);
            (name, path)
        })
        .find(|(name, path)| {
            !path.exists() && !registry.vaults.iter().any(|entry| entry.name == *name)
        })
        .expect("無限の連番から未使用名が必ず見つかる");

    kb_core::connect::clone_existing_vault_with_progress(&url, &path, |progress| {
        // 進捗通知が閉じた画面へ届かなくても、復元そのものは続ける。
        let _ = VaultRestoreProgress::from(progress).emit(&app);
    })
    .map_err(AppError::backup)?;
    registry.add(&name, path).map_err(AppError::from)?;
    registry.save().map_err(AppError::from)?;
    state.reset();
    setup_state()
}
