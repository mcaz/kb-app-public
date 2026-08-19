//! ホーム画面(健全性の要約・タグ・お手入れ提案)。

use kb_core::search::{Hit, Stats, recent, stats};
use serde::Serialize;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Serialize, specta::Type)]
pub struct HomeState {
    stats: Stats,
    notes: Vec<Hit>,
    care: Vec<kb_core::care::CareProposal>,
    tags: Vec<(String, usize)>,
    degraded: Vec<kb_core::degradation::Degradation>,
}

#[tauri::command(async)]
#[specta::specta]
pub fn home_state(state: State<'_, AppState>) -> AppResult<HomeState> {
    state.with_db(|_, conn, degraded| home_state_from(conn, degraded))
}

fn home_state_from(
    conn: &kb_core::rusqlite::Connection,
    mut degraded: Vec<kb_core::degradation::Degradation>,
) -> AppResult<HomeState> {
    let notes = recent(conn, 500).map_err(AppError::index)?;
    let care = partial_or_default(kb_core::care::list_open(conn), &mut degraded, |error| {
        kb_core::degradation::Degradation::CareList {
            detail: error.to_string(),
        }
    });
    let tags = partial_or_default(
        kb_core::search::tag_counts(conn, 30),
        &mut degraded,
        |error| kb_core::degradation::Degradation::TagCounts {
            detail: error.to_string(),
        },
    );
    Ok(HomeState {
        stats: stats(conn).map_err(AppError::index)?,
        notes,
        care,
        tags,
        degraded,
    })
}

/// ネットワーク・Markdown export・埋め込み・お手入れ検知をUIの前景から分離する。
#[derive(Serialize, specta::Type)]
pub struct MaintenanceReport {
    degraded: Vec<kb_core::degradation::Degradation>,
    elapsed_ms: usize,
}

#[tauri::command(async)]
#[specta::specta]
pub fn maintenance_refresh(state: State<'_, AppState>) -> AppResult<MaintenanceReport> {
    let started = std::time::Instant::now();
    let root = state.vault_root()?;
    let vault = kb_core::vault::Vault::open(&root).map_err(AppError::vault)?;

    // pullは内部で別DB接続へimportする。共有AppStateのロックは一切握らない。
    let mut degraded = kb_core::connect::pull_if_stale(&vault)
        .into_iter()
        .collect();
    let conn = kb_core::index::open_db(&vault).map_err(AppError::index)?;
    run_local_maintenance(&vault, &conn, &mut degraded);
    state.set_degraded_for(&root, degraded.clone())?;

    Ok(MaintenanceReport {
        degraded,
        elapsed_ms: started.elapsed().as_millis() as usize,
    })
}

fn run_local_maintenance(
    vault: &kb_core::vault::Vault,
    conn: &kb_core::rusqlite::Connection,
    degraded: &mut Vec<kb_core::degradation::Degradation>,
) {
    match kb_core::index::sync_with_degradations(vault, conn) {
        Ok(mut report) => degraded.append(&mut report.degraded),
        Err(error) => degraded.push(kb_core::degradation::Degradation::IndexSync {
            detail: error.to_string(),
        }),
    }
    degraded.extend(kb_core::index::embed_step(conn));
    if let Err(error) = kb_core::care::detect(conn, vault) {
        degraded.push(kb_core::degradation::Degradation::CareDetection {
            detail: error.to_string(),
        });
    }
}

fn partial_or_default<T: Default>(
    result: anyhow::Result<T>,
    degraded: &mut Vec<kb_core::degradation::Degradation>,
    classify: impl FnOnce(anyhow::Error) -> kb_core::degradation::Degradation,
) -> T {
    match result {
        Ok(data) => data,
        Err(error) => {
            degraded.push(classify(error));
            T::default()
        }
    }
}

#[derive(Serialize, specta::Type)]
pub struct TagOverview {
    tags: Vec<kb_core::search::TagInfo>,
    glossary_note: Option<String>,
    degraded: Vec<kb_core::degradation::Degradation>,
}

/// タグ一覧(説明は KB の「タグ運用」ノート由来 — アプリは意味づけを持たない)。
#[tauri::command(async)]
#[specta::specta]
pub fn tag_overview(state: State<'_, AppState>) -> AppResult<TagOverview> {
    state.with_db(|_, conn, degraded| {
        let (tags, glossary_note) = kb_core::search::tag_overview(conn).map_err(AppError::index)?;
        Ok(TagOverview {
            tags,
            glossary_note,
            degraded,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kb_core::index::{open_db, sync};
    use kb_core::vault::{NoteProposal, Vault};

    /// 2026-08-16までは補助一覧の失敗を空配列へ変換し、0件と区別できなかった。
    #[test]
    fn partial_default_always_records_a_typed_degradation() {
        let mut degraded = Vec::new();
        let data: Vec<String> =
            partial_or_default(Err(anyhow::anyhow!("db locked")), &mut degraded, |error| {
                kb_core::degradation::Degradation::CareList {
                    detail: error.to_string(),
                }
            });
        assert!(data.is_empty());
        assert!(matches!(
            degraded.as_slice(),
            [kb_core::degradation::Degradation::CareList { detail }] if detail == "db locked"
        ));
    }

    /// careの表だけが壊れても、前景処理は最近のノートを返して一覧失敗だけを残す。
    #[test]
    fn broken_care_store_does_not_look_like_an_empty_inbox() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        vault
            .propose(
                &conn,
                NoteProposal {
                    title: "残るノート",
                    body: "本文",
                    description: None,
                    tags: &["test".into()],
                    authority: kb_core::authority::Authority {
                        namespace: kb_core::authority::NoteNamespace::Knowledge,
                        role: kb_core::authority::AuthorityRole::Canonical,
                        status: kb_core::authority::AuthorityStatus::Active,
                        scope: "test/home-state".into(),
                    },
                    relations: Vec::new(),
                    allow_new_tags: true,
                    client: "test/client",
                },
            )
            .unwrap();
        sync(&vault, &conn).unwrap();
        conn.execute_batch(
            "DROP TABLE IF EXISTS care_proposals;
             CREATE TABLE care_proposals(key TEXT PRIMARY KEY);",
        )
        .unwrap();

        let home = home_state_from(&conn, Vec::new()).unwrap();
        assert_eq!(home.notes.len(), 1);
        assert!(home.care.is_empty());
        assert!(!home.degraded.iter().any(|item| matches!(
            item,
            kb_core::degradation::Degradation::CareDetection { .. }
        )));
        assert!(
            home.degraded
                .iter()
                .any(|item| matches!(item, kb_core::degradation::Degradation::CareList { .. }))
        );
    }

    #[test]
    fn foreground_home_reads_db_before_background_care_detection() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        conn.execute(
            "INSERT INTO notes(id,title,status,body,tags) \
             VALUES ('notes/a','A','stable','','')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO links(src,dst) VALUES ('notes/a','notes/missing')",
            [],
        )
        .unwrap();

        let before = home_state_from(&conn, Vec::new()).unwrap();
        assert!(
            before.care.is_empty(),
            "前景DB readが検知まで始めてはいけない"
        );

        let mut degraded = Vec::new();
        run_local_maintenance(&vault, &conn, &mut degraded);
        assert!(degraded.is_empty(), "{degraded:?}");
        let after = home_state_from(&conn, Vec::new()).unwrap();
        assert!(
            after.care.iter().any(|proposal| proposal.kind == "broken"),
            "バックグラウンド保守後は検知結果をDBから読める"
        );
    }
}

#[tauri::command(async)]
#[specta::specta]
pub fn care_dismiss(state: State<'_, AppState>, key: String) -> AppResult<()> {
    state.with_db(|_, conn, _| kb_core::care::dismiss(conn, &key).map_err(AppError::index))
}
