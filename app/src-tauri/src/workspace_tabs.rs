//! macOSのタブ操作。Cmd+Wを標準CloseWindowとDOMの両方へ流さない。

use serde::Serialize;
use tauri::{AppHandle, Runtime};
use tauri_specta::Event;

use crate::error::AppResult;

#[derive(Clone, Copy, Serialize, specta::Type, Event)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(
    not(target_os = "macos"),
    expect(
        dead_code,
        reason = "macOSのネイティブメニューだけが構築し、他OSでも型付きイベントの生成に使う"
    )
)]
pub enum WorkspaceTabShortcut {
    New,
    Close,
}

pub fn install<R: Runtime>(app: &AppHandle<R>) -> AppResult<()> {
    #[cfg(target_os = "macos")]
    native::install(app)?;
    #[cfg(not(target_os = "macos"))]
    let _ = app;
    Ok(())
}

pub fn configure<R: Runtime>(
    app: &AppHandle<R>,
    enabled: bool,
    file: &str,
    new_tab: &str,
    close_tab: &str,
) -> AppResult<()> {
    #[cfg(target_os = "macos")]
    native::configure(app, enabled, file, new_tab, close_tab)?;
    #[cfg(not(target_os = "macos"))]
    let _ = (app, enabled, file, new_tab, close_tab);
    Ok(())
}

#[cfg(target_os = "macos")]
mod native {
    use tauri::Manager;
    use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu, WINDOW_SUBMENU_ID};

    use super::*;
    use crate::background;
    use crate::error::AppError;

    const FILE_MENU: &str = "workspace-tabs-file";
    const NEW_TAB: &str = "workspace-tabs-new";
    const CLOSE_TAB: &str = "workspace-tabs-close";

    pub(super) fn install<R: Runtime>(app: &AppHandle<R>) -> AppResult<()> {
        let menu = Menu::default(app).map_err(AppError::unexpected)?;
        // 既定のApp/Edit/View/Helpをそのまま残す。CloseWindowはFileとWindowの
        // 両方にあり、片方でも残すとCmd+Wがタブ操作を通らず窓を隠してしまう。
        let mut replaced_file = false;
        let mut replaced_window = false;
        for (index, item) in menu
            .items()
            .map_err(AppError::unexpected)?
            .iter()
            .enumerate()
        {
            let Some(submenu) = item.as_submenu() else {
                continue;
            };
            if submenu.id() == WINDOW_SUBMENU_ID {
                let window = Submenu::with_id_and_items(
                    app,
                    WINDOW_SUBMENU_ID,
                    submenu.text().map_err(AppError::unexpected)?,
                    true,
                    &[
                        &PredefinedMenuItem::minimize(app, None).map_err(AppError::unexpected)?,
                        &PredefinedMenuItem::maximize(app, None).map_err(AppError::unexpected)?,
                    ],
                )
                .map_err(AppError::unexpected)?;
                menu.remove(submenu).map_err(AppError::unexpected)?;
                menu.insert(&window, index).map_err(AppError::unexpected)?;
                replaced_window = true;
            } else if submenu.text().map_err(AppError::unexpected)? == "File" {
                // Menu::defaultの固定名で識別する。利用者向けの翻訳はconfigureが担う。
                let file = Submenu::with_id_and_items(
                    app,
                    FILE_MENU,
                    "ファイル",
                    true,
                    &[
                        &MenuItem::with_id(app, NEW_TAB, "新しいタブ", false, Some("Cmd+N"))
                            .map_err(AppError::unexpected)?,
                        &MenuItem::with_id(
                            app,
                            CLOSE_TAB,
                            "現在のタブを閉じる",
                            false,
                            Some("Cmd+W"),
                        )
                        .map_err(AppError::unexpected)?,
                    ],
                )
                .map_err(AppError::unexpected)?;
                menu.remove(submenu).map_err(AppError::unexpected)?;
                menu.insert(&file, index).map_err(AppError::unexpected)?;
                replaced_file = true;
            }
        }
        if !replaced_file || !replaced_window {
            return Err(AppError::unexpected(
                "標準メニューの構成が変わり、タブ操作用に置き換えられない",
            ));
        }
        app.set_menu(menu).map_err(AppError::unexpected)?;
        app.on_menu_event(on_menu_event);
        Ok(())
    }

    fn on_menu_event<R: Runtime>(app: &AppHandle<R>, event: MenuEvent) {
        let shortcut = match event.id().as_ref() {
            NEW_TAB => WorkspaceTabShortcut::New,
            CLOSE_TAB => WorkspaceTabShortcut::Close,
            _ => return,
        };
        let Some(window) = app.get_webview_window("main") else {
            return;
        };
        if matches!(shortcut, WorkspaceTabShortcut::New) {
            background::show(app);
        } else if !window.is_visible().unwrap_or(false) {
            return;
        }
        if let Err(error) = shortcut.emit_to(app, "main") {
            eprintln!("kb-app: タブ操作を画面へ通知できなかった: {error}");
        }
    }

    pub(super) fn configure<R: Runtime>(
        app: &AppHandle<R>,
        enabled: bool,
        file: &str,
        new_tab: &str,
        close_tab: &str,
    ) -> AppResult<()> {
        let menu = app
            .menu()
            .ok_or_else(|| AppError::unexpected("アプリメニューが見つからない"))?;
        let file_item = menu
            .get(FILE_MENU)
            .ok_or_else(|| AppError::unexpected("タブ操作メニューが見つからない"))?;
        let submenu = file_item
            .as_submenu()
            .ok_or_else(|| AppError::unexpected("タブ操作メニューの構成が一致しない"))?;
        submenu.set_text(file).map_err(AppError::unexpected)?;
        for (id, text) in [(NEW_TAB, new_tab), (CLOSE_TAB, close_tab)] {
            let item = submenu
                .get(id)
                .ok_or_else(|| AppError::unexpected("タブ操作項目が見つからない"))?;
            let item = item
                .as_menuitem()
                .ok_or_else(|| AppError::unexpected("タブ操作項目の構成が一致しない"))?;
            item.set_text(text).map_err(AppError::unexpected)?;
            item.set_enabled(enabled).map_err(AppError::unexpected)?;
        }
        Ok(())
    }
}
