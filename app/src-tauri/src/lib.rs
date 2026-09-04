//! 管理アプリの Tauri 層 — コア(kb-core)の薄い GUI。
//!
//! 構成(ADR-0002):
//!   commands/  … `invoke` で呼ばれる関数。機能ごとに分割
//!   background.rs… trayアイコン・閉じるでの待機・ログイン自動起動(ADR-0017)
//!   state.rs   … vault と索引接続の共有(コマンドごとに開き直さない)
//!   error.rs   … 画面へ返すエラー。種類を型にして訳し分けられるようにする
//!   mcp_mode.rs… 同じ実行ファイルを MCP サーバーとして動かす経路
//!
//! このファイルは Builder の組み立てだけを持つ。

pub mod background;
pub mod commands;
pub mod error;
pub mod hook_mode;
pub mod mcp_mode;
pub mod state;

use tauri::Manager;
use tauri_specta::{collect_commands, collect_events};

use commands::{
    background as background_commands, connect, favorites, files, home, notes, settings, setup,
};

/// GUI が呼べるコマンドとイベントの全集合。ここが `app/src/lib/bindings.ts` の正本。
fn specta_builder() -> tauri_specta::Builder<tauri::Wry> {
    tauri_specta::Builder::<tauri::Wry>::new()
        .commands(collect_commands![
            background_commands::autostart_status,
            background_commands::autostart_set,
            background_commands::tray_set_labels,
            setup::setup_state,
            setup::onboard,
            setup::onboard_existing,
            settings::settings_get,
            settings::settings_set_ai_kb_enabled,
            settings::settings_set_claude_kb_enabled,
            settings::settings_set_gpt_kb_enabled,
            settings::settings_ai_guard_status,
            settings::settings_install_ai_guard,
            settings::settings_enable_ai_guard_development_mode,
            home::home_state,
            home::maintenance_refresh,
            home::tag_overview,
            home::care_dismiss,
            notes::note_get,
            notes::note_search,
            notes::note_categories,
            notes::note_list,
            notes::graph_data,
            favorites::favorites_list,
            favorites::favorite_add,
            favorites::favorite_remove,
            files::note_files,
            files::files_list,
            files::file_add,
            files::file_add_from_clipboard,
            files::file_detach,
            files::file_fetch,
            files::file_open,
            files::file_download,
            files::file_preview,
            files::legacy_open,
            connect::connect_state,
            connect::github_auth_state,
            connect::github_sign_in,
            connect::github_sign_out,
            connect::github_open_device_page,
            connect::connect_desktop,
            connect::backup_now,
            connect::backup_create_repository,
            connect::backup_set_remote,
            connect::embed_enable,
            connect::launch_ai,
        ])
        .events(collect_events![
            connect::EmbedProgress,
            connect::GitHubDeviceAuthorization,
            setup::VaultRestoreProgress
        ])
}

/// 型と invoke ラッパの書き出し。`cargo test` からも呼び、CI では生成物に差分が
/// 出ないことを検査する(手書きの型合わせを廃止した — ADR-0002)。
fn export_bindings() -> Result<(), Box<dyn std::error::Error>> {
    specta_builder()
        // usize / u64 が通るのは「件数」と「添付のバイト数(上限 50MB)」だけで、
        // JSON 上も元から number。2^53 を超える値はこの境界に存在しない
        .dangerously_cast_bigints_to_number()
        .export(
            specta_typescript::Typescript::default().header(
                "// このファイルは tauri-specta の生成物です。手で編集しないこと(ADR-0002)。",
            ),
            "../src/lib/bindings.ts",
        )?;
    Ok(())
}

pub fn run() {
    let builder = specta_builder();

    #[cfg(debug_assertions)]
    export_bindings().expect("bindings.ts の生成に失敗");

    tauri::Builder::default()
        // ファイルを選ぶ経路。取り込みに渡すのはパスだけなので、
        // 中身を JS 側へ載せない選択肢がこれしかない(ADR-0003 決定8)
        .plugin(tauri_plugin_dialog::init())
        // 既定のアプリでファイルを開く。**JS 側の権限は与えない** —
        // 開く経路を files::file_open だけにして、必ず resolver を通す
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(builder.invoke_handler())
        .setup(move |app| {
            // イベントの購読口を張る(進捗通知など)
            builder.mount_events(app);
            // trayと「閉じても常駐」。窓は既定で非表示なので、ここで出す側に回る。
            // trayを作れない環境(indicatorの無いLinux等)では常駐せず、閉じるボタンは
            // 従来どおり終了になる。開く手段が無い状態で隠したままにはしない。
            let resident = match background::install(app.handle()) {
                Ok(()) => true,
                Err(error) => {
                    eprintln!("kb-app: trayを用意できなかったので常駐しない: {error}");
                    false
                }
            };
            if !resident || !background::starts_hidden() {
                background::show(app.handle());
            }
            // 開発ビルドの実行ファイルをログイン項目へ登録しない。`#[cfg]` で
            // 切ると release でしか型検査されないので、実行時の判定にしてある。
            if !cfg!(debug_assertions)
                && let Ok(exe) = std::env::current_exe()
                && let Err(error) = kb_core::autostart::initialize(&exe)
            {
                // 常駐そのものは続ける。失敗は設定画面のswitchがOFFとして映す。
                eprintln!("kb-app: 自動起動を登録できなかった: {error}");
            }
            // vault と索引接続はここに集約する(生成は遅延 — 未オンボーディングでも起動できる)
            app.manage(state::AppState::default());
            // asset protocolの静的scopeは空。登録済みの現在Vaultだけを動的に許可する。
            if let Ok(registry) = kb_core::registry::Registry::load()
                && let Ok(root) = registry.resolve(None)
            {
                state::allow_vault_assets(app.handle(), &root)?;
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("tauri build")
        // Dockアイコンからの復帰。窓を隠して常駐している間、macOSはここしか通らない
        // (窓が破棄されていないので、Launchpad / Spotlight もこの経路になる)。
        .run(|_app, _event| {
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = _event {
                background::show(_app);
            }
        });
}

#[cfg(test)]
mod tests {
    /// TS 側の型を生成し直す。CI はこの後 `git diff --exit-code` で
    /// 「コアを変えたのに bindings.ts を更新し忘れた」を落とす。
    #[test]
    fn bindings_are_up_to_date() {
        super::export_bindings().expect("bindings.ts の生成に失敗");
    }

    /// 窓を `visible: false` で作るのは、ログイン起動でちらつかせないため(ADR-0017)。
    /// 表示は setup が担うので、この既定を戻すと `--hidden` の無表示起動が壊れる。
    #[test]
    fn the_main_window_starts_hidden_and_is_shown_from_setup() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(
            config["app"]["windows"][0]["visible"],
            serde_json::json!(false)
        );
    }

    #[test]
    fn webview_security_boundary_is_not_wide_open() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let security = &config["app"]["security"];
        let csp = security["csp"].as_str().expect("CSPはnullにしない");
        assert!(csp.contains("default-src 'self'"));
        assert!(csp.contains("connect-src ipc: http://ipc.localhost"));
        assert!(csp.contains("object-src 'none'"));
        assert!(csp.contains("frame-src asset: http://asset.localhost"));
        assert!(!csp.contains("https:"), "外部通信を既定許可しない");
        assert_eq!(security["assetProtocol"]["scope"], serde_json::json!([]));
    }
}
