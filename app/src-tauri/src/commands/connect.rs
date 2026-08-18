//! 「繋ぐ」画面(AI アプリ・かしこい検索・バックアップ)と、AI の起動。

use kb_core::registry::Registry;
use kb_core::search::stats;
use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_specta::Event;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

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

/// GitHub device flow で画面へ出してよい情報。device code / token は含めない。
#[derive(Clone, Serialize, specta::Type, Event)]
pub struct GitHubDeviceAuthorization {
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
}

impl From<kb_core::github_auth::DeviceAuthorization> for GitHubDeviceAuthorization {
    fn from(authorization: kb_core::github_auth::DeviceAuthorization) -> Self {
        Self {
            user_code: authorization.user_code,
            verification_uri: authorization.verification_uri,
            expires_in: authorization.expires_in,
        }
    }
}

#[tauri::command(async)]
#[specta::specta]
pub fn connect_state(state: State<'_, AppState>) -> AppResult<ConnectState> {
    let desktop = kb_core::connect::claude_desktop_config_path()
        .map(|p| kb_core::connect::desktop_status_at(&p))
        .unwrap_or(kb_core::connect::DesktopStatus::NotFound);

    state.with_db(|vault, conn, _| {
        let s = stats(conn).map_err(AppError::index)?;
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

/// Vault の有無に依存しないため、初回の「既存 Vault を復元」画面からも呼べる。
#[tauri::command]
#[specta::specta]
pub fn github_auth_state() -> AppResult<kb_core::github_auth::GitHubAuthState> {
    kb_core::github_auth::auth_state().map_err(AppError::backup)
}

/// OAuth device flow は polling を含む blocking 処理なので同期 command にする。
#[tauri::command]
#[specta::specta]
pub fn github_sign_in(app: AppHandle) -> AppResult<kb_core::github_auth::GitHubAuthState> {
    kb_core::github_auth::sign_in(|authorization| {
        // 画面が閉じていても認証処理は続ける。token はイベントへ載せない。
        let _ = GitHubDeviceAuthorization::from(authorization).emit(&app);
    })
    .map_err(AppError::backup)
}

#[tauri::command]
#[specta::specta]
pub fn github_sign_out() -> AppResult<()> {
    kb_core::github_auth::sign_out().map_err(AppError::backup)
}

/// Webview に任意 URL を開く権限を渡さず、GitHub の固定ページだけを OS へ渡す。
#[tauri::command]
#[specta::specta]
pub fn github_open_device_page(app: AppHandle) -> AppResult<()> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url("https://github.com/login/device", None::<&str>)
        .map_err(AppError::unexpected)
}

/// かしこい検索をオンにする(モデル導入+全ノート埋め込み)。数分かかる。
///
/// ブロッキング処理なので、同期関数のままTauriの非同期実行枠へ送る。
#[tauri::command(async)]
#[specta::specta]
pub fn embed_enable(app: AppHandle, state: State<'_, AppState>) -> AppResult<()> {
    kb_core::embed::install_model().map_err(AppError::embed)?;

    loop {
        // 1回あたり10本ずつ。ロックを握りっぱなしにせず、他のコマンドを通す
        let (processed, progress) = state.with_db(|_, conn, _| {
            let processed = kb_core::embed::embed_pending(conn, 10).map_err(AppError::embed)?;
            let s = stats(conn).map_err(AppError::index)?;
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
    kb_core::connect::connect_desktop_at(&cfg, &exe, &name).map_err(AppError::configuration)?;
    // 使っていないが、レジストリの整合を明示するために読み出しておく
    debug_assert!(Registry::load().is_ok());
    Ok(())
}

/// 現在ノートを記録して Claude Desktop を前面に(FR-A5 最小)。
#[tauri::command]
#[specta::specta]
pub fn launch_ai(state: State<'_, AppState>, note: Option<String>) -> AppResult<()> {
    if let Some(id) = note {
        let id = kb_core::note_id::NoteId::parse(&id)
            .map_err(AppError::invalid_input)?
            .to_string();
        state.with_vault(|vault| {
            kb_core::connect::set_current_note(vault, &id).map_err(AppError::storage)
        })?;
    }
    launch_claude_desktop()
}

/// Claude Desktop を前面に出す(OS ごとの起動方法)。
#[allow(
    clippy::needless_return,
    reason = "cfg で残る OS 別ブロック末尾には明示 return が必要"
)]
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
