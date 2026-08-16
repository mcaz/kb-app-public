//! 「繋ぐ」画面(AI アプリ・かしこい検索・バックアップ)と、AI の起動。

use kb_core::registry::Registry;
use kb_core::search::stats;
use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_specta::Event;

use crate::error::{AppError, AppResult};
use crate::state::{AppState, Sync};

/// 生成される TS では "not_installed" | "downloading" | "enabled" の union になる
/// (JSON 表現は従来の文字列のまま)。
#[derive(Serialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum SmartSearchPhase {
    NotInstalled,
    Downloading,
    Enabled,
}

#[derive(Serialize, specta::Type)]
pub struct SmartSearchState {
    state: SmartSearchPhase,
    embedded: usize,
    total: usize,
}

#[derive(Serialize, specta::Type)]
pub struct ConnectState {
    desktop: kb_core::connect::DesktopStatus,
    backup: kb_core::connect::BackupStatus,
    sync_error: Option<String>,
    sync_error_kind: Option<kb_core::backup::BackupFailureKind>,
    smart_search: SmartSearchState,
}

/// かしこい検索の進み具合。数分かかるので画面へ流す。
#[derive(Clone, Serialize, specta::Type, Event)]
pub struct EmbedProgress {
    pub embedded: usize,
    pub total: usize,
}

#[tauri::command]
#[specta::specta]
pub fn connect_state(state: State<'_, AppState>) -> AppResult<ConnectState> {
    let desktop = kb_core::connect::claude_desktop_config_path()
        .map(|p| kb_core::connect::desktop_status_at(&p))
        .unwrap_or(kb_core::connect::DesktopStatus::NotFound);

    state.with_index(Sync::Throttled, |vault, conn, _| {
        let s = stats(conn).map_err(AppError::from)?;
        let sync = kb_core::connect::sync_state(vault);
        Ok(ConnectState {
            desktop,
            backup: kb_core::connect::backup_status(vault).map_err(AppError::backup)?,
            sync_error: sync.last_error,
            sync_error_kind: sync.last_error_kind,
            smart_search: SmartSearchState {
                state: if s.embed_enabled {
                    SmartSearchPhase::Enabled
                } else if kb_core::embed::downloading() {
                    SmartSearchPhase::Downloading
                } else {
                    SmartSearchPhase::NotInstalled
                },
                embedded: s.embedded,
                total: s.total,
            },
        })
    })
}

/// かしこい検索をオンにする(モデル導入+全ノート埋め込み)。数分かかる。
///
/// **同期コマンドにしてある**。Tauri は同期コマンドを別スレッドで動かすが、
/// async コマンドは async ランタイム上で動くため、ここのようにブロッキングで
/// 回す処理を async にするとランタイムを止めてしまう。
#[tauri::command]
#[specta::specta]
pub fn embed_enable(app: AppHandle, state: State<'_, AppState>) -> AppResult<()> {
    kb_core::embed::install_model().map_err(|e| AppError::EmbedFailed {
        message: e.to_string(),
    })?;

    loop {
        // 1回あたり10本ずつ。ロックを握りっぱなしにせず、他のコマンドを通す
        let (processed, progress) = state.with_index(Sync::Throttled, |_, conn, _| {
            let processed =
                kb_core::embed::embed_pending(conn, 10).map_err(|e| AppError::EmbedFailed {
                    message: e.to_string(),
                })?;
            let s = stats(conn).map_err(AppError::from)?;
            Ok((
                processed,
                EmbedProgress {
                    embedded: s.embedded,
                    total: s.total,
                },
            ))
        })?;

        // 進捗の通知に失敗しても本処理は続ける
        let _ = progress.emit(&app);
        if processed == 0 {
            break;
        }
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn backup_set_remote(state: State<'_, AppState>, url: String) -> AppResult<()> {
    state.with_vault(|vault| {
        kb_core::connect::set_backup_remote(vault, &url).map_err(AppError::backup)
    })
}

#[tauri::command]
#[specta::specta]
pub fn backup_create_repository(state: State<'_, AppState>, name: String) -> AppResult<()> {
    let repository = kb_core::github::create_private_repository(&name).map_err(AppError::backup)?;
    state.with_vault(|vault| {
        kb_core::connect::set_backup_remote(vault, &repository.clone_url).map_err(AppError::backup)
    })
}

#[tauri::command]
#[specta::specta]
pub fn backup_now(state: State<'_, AppState>) -> AppResult<String> {
    state.with_vault(|vault| kb_core::connect::backup_push(vault).map_err(AppError::backup))
}

/// Claude Desktop の設定にこの実行ファイルを MCP サーバーとして登録する。
#[tauri::command]
#[specta::specta]
pub fn connect_desktop(state: State<'_, AppState>) -> AppResult<()> {
    let name = state.vault_name()?;
    let cfg =
        kb_core::connect::claude_desktop_config_path().ok_or(AppError::ClaudeDesktopNotFound)?;
    let exe = std::env::current_exe()?;
    kb_core::connect::connect_desktop_at(&cfg, &exe, &name).map_err(AppError::from)?;
    // 使っていないが、レジストリの整合を明示するために読み出しておく
    debug_assert!(Registry::load().is_ok());
    Ok(())
}

/// 現在ノートを記録して Claude Desktop を前面に(FR-A5 最小)。
#[tauri::command]
#[specta::specta]
pub fn launch_ai(state: State<'_, AppState>, note: Option<String>) -> AppResult<()> {
    if let Some(id) = note {
        state.with_vault(|vault| {
            kb_core::connect::set_current_note(vault, &id).map_err(AppError::from)
        })?;
    }
    launch_claude_desktop()
}

/// Claude Desktop を前面に出す(OS ごとの起動方法)。
// cfg で分岐しているため各ブロック末尾の return が必要(どれか1つだけが残る)
#[allow(clippy::needless_return)]
fn launch_claude_desktop() -> AppResult<()> {
    #[cfg(target_os = "macos")]
    {
        let ok = std::process::Command::new("open")
            .args(["-a", "Claude"])
            .status()?
            .success();
        return if ok {
            Ok(())
        } else {
            Err(AppError::ClaudeDesktopLaunchFailed)
        };
    }
    #[cfg(target_os = "windows")]
    {
        // 既定のインストール先 → だめならプロトコルハンドラ経由
        if let Some(local) = dirs::data_local_dir() {
            let exe = local.join("AnthropicClaude").join("claude.exe");
            if exe.exists() && std::process::Command::new(&exe).spawn().is_ok() {
                return Ok(());
            }
        }
        let ok = std::process::Command::new("cmd")
            .args(["/C", "start", "", "claude://"])
            .status()?
            .success();
        return if ok {
            Ok(())
        } else {
            Err(AppError::ClaudeDesktopLaunchFailed)
        };
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let ok = std::process::Command::new("xdg-open")
            .arg("claude://")
            .status()?
            .success();
        return if ok {
            Ok(())
        } else {
            Err(AppError::ClaudeDesktopLaunchFailed)
        };
    }
}
