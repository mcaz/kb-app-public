//! ノートの表示・検索・グラフ。

use kb_core::search::{
    NoteCategory, NoteListPage, SearchOutcome, note_categories as categories, notes_in_category,
    related_of, search,
};
use serde::Serialize;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::state::{AppState, Sync};

#[derive(Serialize, specta::Type)]
pub struct NoteView {
    id: String,
    title: String,
    description: Option<String>,
    body: String,
    status: String,
    origin: Option<String>,
    tags: Vec<String>,
    created_at: Option<String>,
    generated_at: Option<String>,
    related: Vec<(String, Option<String>)>,
    similar: Vec<(String, Option<String>, f32)>,
    vault_root: String,
}

#[tauri::command]
#[specta::specta]
pub fn note_get(state: State<'_, AppState>, id: String) -> AppResult<NoteView> {
    state.with_index(Sync::Throttled, |vault, conn, _| {
        let note = vault
            .read_note(&id)
            .map_err(|_| AppError::NoteNotFound { id: id.clone() })?;
        // 「このノート」文脈(FR-A5): 開いたノートを現在ノートとして記録
        // (MCP の get 引数なしがこれを返す)。失敗しても表示は続ける
        let _ = kb_core::connect::set_current_note(vault, &id);
        Ok(NoteView {
            title: note.front.title.clone().unwrap_or_else(|| id.clone()),
            description: note.front.description.clone(),
            body: note.body.clone(),
            status: note.front.effective_status().to_string(),
            origin: note.front.origin.clone(),
            tags: note.front.tags.clone(),
            created_at: note.front.created_at(),
            generated_at: note.front.updated_at(),
            related: related_of(conn, Some(&id)).unwrap_or_default(),
            similar: kb_core::search::similar_notes(conn, &id, 6).unwrap_or_default(),
            vault_root: vault.root.display().to_string(),
            id: id.clone(),
        })
    })
}

#[tauri::command]
#[specta::specta]
pub fn note_search(state: State<'_, AppState>, query: String) -> AppResult<SearchOutcome> {
    // 検索は鮮度が要る(書いた直後に引けること — 増分 sync の前提)
    state.with_index(Sync::Force, |_, conn, degraded| {
        let mut out = search(conn, &query, 30);
        out.degraded.extend(degraded);
        Ok(out)
    })
}

/// サイドバー用のディレクトリと子孫ノート件数。ノート本文は返さない。
#[tauri::command]
#[specta::specta]
pub fn note_categories(state: State<'_, AppState>) -> AppResult<Vec<NoteCategory>> {
    state.with_index(Sync::Throttled, |_, conn, _| {
        categories(conn).map_err(AppError::from)
    })
}

/// 選択ディレクトリ配下のノートをcursor pageで返す。
#[tauri::command]
#[specta::specta]
pub fn note_list(
    state: State<'_, AppState>,
    category: String,
    after: Option<String>,
    limit: usize,
) -> AppResult<NoteListPage> {
    state.with_index(Sync::Throttled, |_, conn, _| {
        notes_in_category(conn, &category, after.as_deref(), limit).map_err(AppError::from)
    })
}

#[derive(Serialize, specta::Type)]
pub struct GraphNode {
    id: String,
    title: String,
    origin: Option<String>,
    status: String,
    degree: usize,
}

#[derive(Serialize, specta::Type)]
pub struct GraphData {
    nodes: Vec<GraphNode>,
    edges: Vec<(String, String)>,
}

/// グラフビュー(FR-A7)用のノード・エッジ。退役ノートと未執筆リンク先は除く。
#[tauri::command]
#[specta::specta]
pub fn graph_data(state: State<'_, AppState>) -> AppResult<GraphData> {
    state.with_index(Sync::Throttled, |_, conn, _| {
        let mut nodes: Vec<GraphNode> = {
            let mut stmt = conn
                .prepare(
                    "SELECT id, coalesce(title, id), origin, status \
                     FROM notes WHERE status != 'deprecated'",
                )
                .map_err(AppError::unexpected)?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(GraphNode {
                        id: r.get(0)?,
                        title: r.get(1)?,
                        origin: r.get(2)?,
                        status: r.get(3)?,
                        degree: 0,
                    })
                })
                .map_err(AppError::unexpected)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(AppError::unexpected)?
        };

        let ids: std::collections::HashSet<String> = nodes.iter().map(|n| n.id.clone()).collect();
        let edges: Vec<(String, String)> = {
            let mut stmt = conn
                .prepare("SELECT src, dst FROM links")
                .map_err(AppError::unexpected)?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(AppError::unexpected)?;
            rows.filter_map(|r| r.ok())
                .filter(|(s, d)| ids.contains(s) && ids.contains(d))
                .collect()
        };

        for n in &mut nodes {
            n.degree = edges
                .iter()
                .filter(|(s, d)| *s == n.id || *d == n.id)
                .count();
        }
        Ok(GraphData { nodes, edges })
    })
}
