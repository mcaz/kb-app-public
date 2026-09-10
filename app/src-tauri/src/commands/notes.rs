//! ノートの表示・検索・グラフ。

use kb_core::search::{
    NoteCategory, NoteListPage, SearchOutcome, note_categories as categories, notes_in_category,
    related_of, search,
};
use serde::Serialize;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Serialize, specta::Type)]
pub struct NoteView {
    id: String,
    title: String,
    description: Option<String>,
    body: String,
    status: String,
    origin: Option<String>,
    tags: Vec<String>,
    note_uid: Option<kb_core::authority::NoteUid>,
    authority: Option<kb_core::authority::Authority>,
    relations: Vec<kb_core::authority::NoteRelation>,
    created_at: Option<String>,
    generated_at: Option<String>,
    related: Vec<(String, Option<String>)>,
    similar: Vec<(String, Option<String>, f32)>,
    degraded: Vec<kb_core::degradation::Degradation>,
    vault_root: String,
    /// 来歴(契約20)の1行要約。イベントが1件も無い(移行前かつbackfill前の)
    /// ノートでは `None`(空文字で誤魔化さない)。
    provenance_line: Option<String>,
}

#[tauri::command(async)]
#[specta::specta]
pub fn note_get(state: State<'_, AppState>, id: String) -> AppResult<NoteView> {
    state.with_db(|vault, conn, degraded| note_view_from(vault, conn, &id, degraded))
}

/// 自動再取得や検索プレビューに文脈を奪われないよう、主ノートの選択だけを記録する。
#[tauri::command(async)]
#[specta::specta]
pub fn note_set_current(
    state: State<'_, AppState>,
    id: String,
) -> AppResult<Vec<kb_core::degradation::Degradation>> {
    state.with_db(|vault, conn, _| set_current_note_from(vault, conn, &id))
}

fn set_current_note_from(
    vault: &kb_core::vault::Vault,
    conn: &kb_core::rusqlite::Connection,
    id: &str,
) -> AppResult<Vec<kb_core::degradation::Degradation>> {
    let id = kb_core::note_id::NoteId::parse(id).map_err(AppError::invalid_input)?;
    let id = id.as_str();
    if !kb_core::note_store::contains(conn, id).map_err(AppError::index)? {
        return Err(AppError::note_not_found(id));
    }
    // 文脈の記録に失敗しても、既に取得した本文の表示は止めない。
    Ok(kb_core::connect::set_current_note(vault, id)
        .err()
        .map(
            |error| kb_core::degradation::Degradation::CurrentNoteContext {
                detail: error.to_string(),
            },
        )
        .into_iter()
        .collect())
}

fn note_view_from(
    vault: &kb_core::vault::Vault,
    conn: &kb_core::rusqlite::Connection,
    id: &str,
    mut degraded: Vec<kb_core::degradation::Degradation>,
) -> AppResult<NoteView> {
    let id = kb_core::note_id::NoteId::parse(id).map_err(AppError::invalid_input)?;
    let id = id.as_str();
    let note = vault
        .read_note_from_db(conn, id)
        .map_err(|_| AppError::note_not_found(id))?;
    // 本文は表示できるので、派生索引の失敗だけを型付きで添える。
    let related = match related_of(conn, Some(id)) {
        Ok(related) => related,
        Err(error) => {
            degraded.push(kb_core::degradation::Degradation::RelatedNotes {
                detail: error.to_string(),
            });
            Vec::new()
        }
    };
    let similar = match kb_core::search::similar_notes(conn, id, 6) {
        Ok(similar) => similar,
        Err(error) => {
            degraded.push(kb_core::degradation::Degradation::SimilarNotes {
                detail: error.to_string(),
            });
            Vec::new()
        }
    };
    // mcp.rsの`get`ツール(Phase 2)と同じ扱い — 台帳は本文と同じ`with_db`接続で読むので、
    // ここが失敗する状況は本文自体も信用できない状況に近い。fail-openの対象にしない。
    let provenance = vault.note_provenance(conn, id).map_err(AppError::index)?;
    let provenance_line =
        (provenance.event_count > 0).then(|| kb_core::provenance::provenance_line(&provenance));
    Ok(NoteView {
        title: note.front.title.clone().unwrap_or_else(|| id.to_string()),
        description: note.front.description.clone(),
        body: note.body.clone(),
        status: note.front.effective_status().to_string(),
        origin: note.front.origin.clone(),
        tags: note.front.tags.clone(),
        note_uid: note.front.note_uid.clone(),
        authority: note.front.authority.clone(),
        relations: note.front.relations.clone(),
        created_at: note.front.created_at(),
        generated_at: note.front.updated_at(),
        related,
        similar,
        degraded,
        vault_root: vault.root.display().to_string(),
        id: id.to_string(),
        provenance_line,
    })
}

#[tauri::command(async)]
#[specta::specta]
pub fn note_search(state: State<'_, AppState>, query: String) -> AppResult<SearchOutcome> {
    state.with_db(|_, conn, degraded| {
        let mut out = search(conn, &query, 30);
        out.degraded.extend(degraded);
        Ok(out)
    })
}

#[tauri::command(async)]
#[specta::specta]
pub fn note_browse(
    state: State<'_, AppState>,
    tags: Vec<String>,
    period: kb_core::search::NoteBrowsePeriod,
    sort: kb_core::search::NoteBrowseSort,
    after: Option<String>,
    limit: usize,
) -> AppResult<kb_core::search::NoteBrowsePage> {
    state.with_db(|_, conn, degraded| {
        let mut page =
            kb_core::search::browse_notes(conn, &tags, period, sort, after.as_deref(), limit)?;
        page.degraded.extend(degraded);
        Ok(page)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kb_core::index::{open_db, sync};
    use kb_core::vault::{NoteProposal, Vault};

    fn fixture_note(vault: &Vault, conn: &kb_core::rusqlite::Connection, title: &str) -> String {
        vault
            .propose(
                conn,
                NoteProposal {
                    judgment: None,
                    title,
                    body: "合成ノートの本文",
                    description: None,
                    tags: &["test".into()],
                    authority: kb_core::authority::Authority {
                        namespace: kb_core::authority::NoteNamespace::Knowledge,
                        role: kb_core::authority::AuthorityRole::Canonical,
                        status: kb_core::authority::AuthorityStatus::Active,
                        scope: format!("test/{title}"),
                    },
                    relations: Vec::new(),
                    allow_new_tags: true,
                    client: "test/client",
                    actor: None,
                    revision: None,
                },
            )
            .unwrap()
    }

    /// 2026-09-06: 自動更新・検索プレビューの取得が、明示選択したMCP文脈を上書きしない。
    #[test]
    fn note_reads_do_not_change_the_explicitly_selected_context() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        let first = fixture_note(&vault, &conn, "first");
        let preview = fixture_note(&vault, &conn, "preview");

        note_view_from(&vault, &conn, &preview, Vec::new()).unwrap();
        assert!(kb_core::connect::current_note(&vault).is_none());
        assert!(
            set_current_note_from(&vault, &conn, &first)
                .unwrap()
                .is_empty()
        );
        for id in [&first, &preview, &first, &preview] {
            let view = note_view_from(&vault, &conn, id, Vec::new()).unwrap();
            assert_eq!(view.body, "合成ノートの本文\n");
            assert_eq!(
                kb_core::connect::current_note(&vault).as_deref(),
                Some(first.as_str())
            );
        }
        assert!(
            set_current_note_from(&vault, &conn, &preview)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            kb_core::connect::current_note(&vault).as_deref(),
            Some(preview.as_str())
        );
    }

    #[test]
    fn selecting_an_invalid_or_missing_note_preserves_the_current_context() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        let first = fixture_note(&vault, &conn, "first");
        set_current_note_from(&vault, &conn, &first).unwrap();

        assert!(matches!(
            set_current_note_from(&vault, &conn, "../outside"),
            Err(AppError::CoreFailed {
                kind: kb_core::error::CoreErrorKind::InvalidInput
            })
        ));
        assert!(matches!(
            set_current_note_from(&vault, &conn, "notes/missing"),
            Err(AppError::NoteNotFound { .. })
        ));
        assert_eq!(
            kb_core::connect::current_note(&vault).as_deref(),
            Some(first.as_str())
        );
    }

    #[test]
    fn context_write_failure_is_a_degradation_and_does_not_block_note_reads() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        let first = fixture_note(&vault, &conn, "first");
        std::fs::create_dir(vault.root.join(".kb/current-note")).unwrap();

        let degraded = set_current_note_from(&vault, &conn, &first).unwrap();
        assert!(matches!(
            degraded.as_slice(),
            [kb_core::degradation::Degradation::CurrentNoteContext { .. }]
        ));
        let view = note_view_from(&vault, &conn, &first, Vec::new()).unwrap();
        assert_eq!(view.body, "合成ノートの本文\n");
        assert!(!view.degraded.iter().any(|item| matches!(
            item,
            kb_core::degradation::Degradation::CurrentNoteContext { .. }
        )));
    }

    /// 2026-08-16までは関連・近いノートのDB失敗が空配列になり、0件と見分けられなかった。
    #[test]
    fn note_body_survives_partial_failures_with_typed_degradations() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        let id = vault
            .propose(
                &conn,
                NoteProposal {
                    judgment: None,
                    title: "本文は読める",
                    body: "本文",
                    description: None,
                    tags: &["test".into()],
                    authority: kb_core::authority::Authority {
                        namespace: kb_core::authority::NoteNamespace::Knowledge,
                        role: kb_core::authority::AuthorityRole::Canonical,
                        status: kb_core::authority::AuthorityStatus::Active,
                        scope: "test/note-body".into(),
                    },
                    relations: Vec::new(),
                    allow_new_tags: true,
                    client: "test/client",
                    actor: None,
                    revision: None,
                },
            )
            .unwrap();
        sync(&vault, &conn).unwrap();
        conn.execute_batch("DROP TABLE links; DROP TABLE note_vecs;")
            .unwrap();

        let view = note_view_from(&vault, &conn, &id, Vec::new()).unwrap();
        assert_eq!(view.body, "本文\n");
        assert!(view.related.is_empty());
        assert!(view.similar.is_empty());
        assert!(
            view.degraded
                .iter()
                .any(|item| matches!(item, kb_core::degradation::Degradation::RelatedNotes { .. }))
        );
        assert!(
            view.degraded
                .iter()
                .any(|item| matches!(item, kb_core::degradation::Degradation::SimilarNotes { .. }))
        );
    }

    #[test]
    fn invalid_note_id_is_typed_as_invalid_input() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();

        let error = match note_view_from(&vault, &conn, "../outside", Vec::new()) {
            Ok(_) => panic!("Vault外のIDが受理された"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            AppError::CoreFailed {
                kind: kb_core::error::CoreErrorKind::InvalidInput
            }
        ));
    }

    #[test]
    fn listing_surfaces_keep_index_degradations() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        std::fs::write(vault.root.join("notes/broken.md"), "frontmatterではない").unwrap();
        let report = kb_core::index::import_markdown_snapshot(&vault, &conn).unwrap();
        assert!(report.degraded.iter().any(|item| matches!(
            item,
            kb_core::degradation::Degradation::IndexParse { note, .. }
                if note == "notes/broken"
        )));

        let categories = note_categories_from(&conn, report.degraded.clone()).unwrap();
        assert!(categories.categories.is_empty());
        assert_eq!(categories.degraded, report.degraded);

        let page = note_list_from(&conn, "", None, 100, categories.degraded.clone()).unwrap();
        assert!(page.notes.is_empty());
        assert_eq!(page.degraded, categories.degraded);
    }

    #[test]
    fn graph_row_failures_keep_valid_data_and_add_typed_degradations() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        conn.execute(
            "INSERT INTO notes(id,title,status,body,tags) VALUES ('notes/a','A','stable','','')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO notes(id,title,status,body,tags) VALUES (?1,'broken','stable','','')",
            [vec![0xff]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO links(src,dst) VALUES ('notes/a','notes/a')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO links(src,dst) VALUES (?1,'notes/a')",
            [vec![0xff]],
        )
        .unwrap();

        let graph = graph_data_from(&conn, Vec::new()).unwrap();
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.edges, vec![("notes/a".into(), "notes/a".into())]);
        assert!(
            graph
                .degraded
                .iter()
                .any(|item| matches!(item, kb_core::degradation::Degradation::GraphNodes { .. }))
        );
        assert!(
            graph
                .degraded
                .iter()
                .any(|item| matches!(item, kb_core::degradation::Degradation::GraphEdges { .. }))
        );
    }
}

/// サイドバー用のディレクトリと子孫ノート件数。ノート本文は返さない。
#[derive(Serialize, specta::Type)]
pub struct NoteCategories {
    categories: Vec<NoteCategory>,
    degraded: Vec<kb_core::degradation::Degradation>,
}

#[tauri::command(async)]
#[specta::specta]
pub fn note_categories(state: State<'_, AppState>) -> AppResult<NoteCategories> {
    state.with_db(|_, conn, degraded| note_categories_from(conn, degraded))
}

fn note_categories_from(
    conn: &kb_core::rusqlite::Connection,
    degraded: Vec<kb_core::degradation::Degradation>,
) -> AppResult<NoteCategories> {
    Ok(NoteCategories {
        categories: categories(conn).map_err(AppError::index)?,
        degraded,
    })
}

/// 選択ディレクトリ配下のノートをcursor pageで返す。
#[tauri::command(async)]
#[specta::specta]
pub fn note_list(
    state: State<'_, AppState>,
    category: String,
    after: Option<String>,
    limit: usize,
) -> AppResult<NoteListPage> {
    let mut page = state.with_db(|_, conn, degraded| {
        note_list_from(conn, &category, after.as_deref(), limit, degraded)
    })?;
    state.with_artifacts(|vault, _, ledger, _| {
        let managed_counts = ledger.current_counts_by_note();
        for note in &mut page.notes {
            let managed = managed_counts.get(&note.id).copied().unwrap_or(0);
            let legacy = vault
                .list_attachments(&note.id)
                .map_err(AppError::storage)?
                .len();
            note.file_count = managed + legacy;
        }
        Ok(page)
    })
}

fn note_list_from(
    conn: &kb_core::rusqlite::Connection,
    category: &str,
    after: Option<&str>,
    limit: usize,
    degraded: Vec<kb_core::degradation::Degradation>,
) -> AppResult<NoteListPage> {
    let mut page = notes_in_category(conn, category, after, limit).map_err(AppError::index)?;
    page.degraded.extend(degraded);
    Ok(page)
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
    degraded: Vec<kb_core::degradation::Degradation>,
}

/// グラフビュー(FR-A7)用のノード・エッジ。退役ノートと未執筆リンク先は除く。
#[tauri::command(async)]
#[specta::specta]
pub fn graph_data(state: State<'_, AppState>) -> AppResult<GraphData> {
    state.with_db(|_, conn, degraded| graph_data_from(conn, degraded))
}

fn graph_data_from(
    conn: &kb_core::rusqlite::Connection,
    mut degraded: Vec<kb_core::degradation::Degradation>,
) -> AppResult<GraphData> {
    let mut nodes: Vec<GraphNode> = {
        let mut stmt = conn
            .prepare(
                "SELECT id, coalesce(title, id), origin, status \
                     FROM notes WHERE status != 'deprecated'",
            )
            .map_err(AppError::index)?;
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
            .map_err(AppError::index)?;
        let mut nodes = Vec::new();
        for row in rows {
            match row {
                Ok(node) => nodes.push(node),
                Err(error) => degraded.push(kb_core::degradation::Degradation::GraphNodes {
                    detail: error.to_string(),
                }),
            }
        }
        nodes
    };

    let ids: std::collections::HashSet<String> = nodes.iter().map(|n| n.id.clone()).collect();
    let edges: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare(
                "SELECT src, dst FROM links
                 UNION
                 SELECT source.id, target.id FROM note_relations relation
                 JOIN notes source ON source.note_uid = relation.src_uid
                 JOIN notes target ON target.note_uid = relation.target_uid",
            )
            .map_err(AppError::index)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(AppError::index)?;
        let mut edges = Vec::new();
        for row in rows {
            match row {
                Ok((src, dst)) if ids.contains(&src) && ids.contains(&dst) => {
                    edges.push((src, dst));
                }
                Ok(_) => {}
                Err(error) => degraded.push(kb_core::degradation::Degradation::GraphEdges {
                    detail: error.to_string(),
                }),
            }
        }
        edges
    };

    for n in &mut nodes {
        n.degree = edges
            .iter()
            .filter(|(s, d)| *s == n.id || *d == n.id)
            .count();
    }
    Ok(GraphData {
        nodes,
        edges,
        degraded,
    })
}
