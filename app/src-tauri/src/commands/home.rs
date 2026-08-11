//! ホーム画面(健全性の要約・タグ・お手入れ提案)。

use kb_core::search::{Hit, Stats, recent, stats};
use serde::Serialize;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::state::{AppState, Sync};

#[derive(Serialize, specta::Type)]
pub struct HomeState {
    stats: Stats,
    notes: Vec<Hit>,
    care: Vec<kb_core::care::CareProposal>,
    tags: Vec<(String, usize)>,
    degraded: Vec<String>,
}

#[tauri::command]
#[specta::specta]
pub fn home_state(state: State<'_, AppState>) -> AppResult<HomeState> {
    // 画面更新の際に他デバイスの変化も取り込む(コア側で60秒スロットリング・fail-open)
    let pull_degraded = state.with_vault(|vault| Ok(kb_core::connect::pull_if_stale(vault)))?;

    // 一覧の鮮度が要る入口なので索引は必ず更新する
    state.with_index(Sync::Force, |vault, conn, degraded| {
        let notes = recent(conn, 500).map_err(AppError::from)?;
        // お手入れの検知(FR-C7 最小形)。失敗しても画面は出す(fail-open)
        let _ = kb_core::care::detect(conn, vault);
        Ok(HomeState {
            stats: stats(conn).map_err(AppError::from)?,
            notes,
            care: kb_core::care::list_open(conn).unwrap_or_default(),
            tags: kb_core::search::tag_counts(conn, 30).unwrap_or_default(),
            degraded: degraded.into_iter().chain(pull_degraded.clone()).collect(),
        })
    })
}

#[derive(Serialize, specta::Type)]
pub struct TagOverview {
    tags: Vec<kb_core::search::TagInfo>,
    glossary_note: Option<String>,
}

/// タグ一覧(説明は KB の「タグ運用」ノート由来 — アプリは意味づけを持たない)。
#[tauri::command]
#[specta::specta]
pub fn tag_overview(state: State<'_, AppState>) -> AppResult<TagOverview> {
    state.with_index(Sync::Throttled, |_, conn, _| {
        let (tags, glossary_note) = kb_core::search::tag_overview(conn).map_err(AppError::from)?;
        Ok(TagOverview {
            tags,
            glossary_note,
        })
    })
}

#[tauri::command]
#[specta::specta]
pub fn care_dismiss(state: State<'_, AppState>, key: String) -> AppResult<()> {
    state.with_index(Sync::Throttled, |_, conn, _| {
        kb_core::care::dismiss(conn, &key).map_err(AppError::from)
    })
}
