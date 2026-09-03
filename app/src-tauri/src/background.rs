//! 常駐と復帰 — trayアイコン・閉じるでの待機・ログイン時の自動起動(ADR-0017)。
//!
//! kb-appは会話の最中に「今の状態を見る」「設定を切り替える」ために開かれるので、
//! そのたびに起動を待つ形にしない。窓を閉じてもプロセスは残り、trayアイコンから
//! 復帰する。**終了はtrayメニューだけ**が入口で、閉じるボタンでは終わらせない。

use tauri::menu::{Menu, MenuEvent, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Runtime, WindowEvent};

use crate::error::{AppError, AppResult};

const MAIN_WINDOW: &str = "main";
const TRAY_ID: &str = "kb-app";
const SHOW_ITEM: &str = "show";
const QUIT_ITEM: &str = "quit";

/// 画面の言語が決まる前(ログイン直後の無表示起動)に出るtrayメニューの文言。
/// webviewが読み込まれた時点で `tray_set_labels` が利用者の言語へ差し替える。
const DEFAULT_SHOW_LABEL: &str = "kb-app を開く";
const DEFAULT_QUIT_LABEL: &str = "終了";

/// ログイン項目から起動されたか。窓を出さずtrayだけで待つ。
pub fn starts_hidden() -> bool {
    hidden_flag_present(std::env::args())
}

fn hidden_flag_present(mut args: impl Iterator<Item = String>) -> bool {
    args.any(|arg| arg == kb_core::autostart::HIDDEN_FLAG)
}

/// trayアイコンと閉じる操作の待機を用意する。
///
/// trayを作れなかったときは窓の閉じる操作へ触れずに返す。開く手段が無いのに
/// 窓を隠すと、アプリを取り戻せなくなるため。
pub fn install<R: Runtime>(app: &AppHandle<R>) -> AppResult<()> {
    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| AppError::unexpected("アプリのアイコンが埋め込まれていない"))?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip("kb-app")
        .menu(&menu(app, DEFAULT_SHOW_LABEL, DEFAULT_QUIT_LABEL)?)
        // 左クリックは画面を開く。メニューは右クリックへ寄せる — 依頼が
        // 「アイコンクリックでUI表示」で、Windowsのtrayもこの作法のため。
        // (Linuxのindicatorはクリック事象を配らないので、メニューが唯一の入口)
        .show_menu_on_left_click(false)
        .on_menu_event(on_menu_event)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show(tray.app_handle());
            }
        })
        .build(app)
        .map_err(AppError::unexpected)?;

    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let handle = app.clone();
        window.on_window_event(move |event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // 破棄すると次に開くとき起動待ちが戻る。窓は隠すだけにする。
                api.prevent_close();
                hide(&handle);
            }
        });
    }
    Ok(())
}

/// 窓を出して前面へ出す。tray・Dock・ログイン後の復帰で共通の入口。
pub fn show<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
        return;
    };
    // 失敗しても常駐は続けたいので、ここでの取りこぼしはアプリを止めない。
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
}

fn hide<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.hide();
    }
}

fn menu<R: Runtime>(app: &AppHandle<R>, show: &str, quit: &str) -> AppResult<Menu<R>> {
    let show = MenuItem::with_id(app, SHOW_ITEM, show, true, None::<&str>)
        .map_err(AppError::unexpected)?;
    let quit = MenuItem::with_id(app, QUIT_ITEM, quit, true, None::<&str>)
        .map_err(AppError::unexpected)?;
    Menu::with_items(app, &[&show, &quit]).map_err(AppError::unexpected)
}

fn on_menu_event<R: Runtime>(app: &AppHandle<R>, event: MenuEvent) {
    match event.id().as_ref() {
        SHOW_ITEM => show(app),
        QUIT_ITEM => app.exit(0),
        _ => {}
    }
}

/// trayメニューを画面と同じ言語にする。webview側の言語設定が正本なので、
/// 起動時と言語切り替えのたびにフロントから渡し直す。
pub fn apply_labels<R: Runtime>(app: &AppHandle<R>, show: &str, quit: &str) -> AppResult<()> {
    let tray = app
        .tray_by_id(TRAY_ID)
        .ok_or_else(|| AppError::unexpected("trayアイコンが見つからない"))?;
    tray.set_menu(Some(menu(app, show, quit)?))
        .map_err(AppError::unexpected)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ログイン項目が渡す引数と、ここで見る引数が一致すること。ずれると
    /// 「無表示で起動したはずが窓が出る」が静かに生まれ、ログイン時にしか気付けない。
    #[test]
    fn the_hidden_flag_matches_what_the_login_item_passes() {
        fn args(values: &[&str]) -> std::vec::IntoIter<String> {
            values
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>()
                .into_iter()
        }

        assert!(hidden_flag_present(args(&[
            "kb-app",
            kb_core::autostart::HIDDEN_FLAG
        ])));
        assert!(!hidden_flag_present(args(&["kb-app"])));
        // 前方一致で拾うと `--hidden-something` まで無表示になる。
        assert!(!hidden_flag_present(args(&["kb-app", "--hidden-window"])));
    }
}
