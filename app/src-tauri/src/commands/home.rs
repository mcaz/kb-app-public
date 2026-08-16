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
    degraded: Vec<kb_core::degradation::Degradation>,
}

#[tauri::command]
#[specta::specta]
pub fn home_state(state: State<'_, AppState>) -> AppResult<HomeState> {
    // 画面更新の際に他デバイスの変化も取り込む(コア側で60秒スロットリング・fail-open)
    let pull_degraded = state.with_vault(|vault| Ok(kb_core::connect::pull_if_stale(vault)))?;

    // 一覧の鮮度が要る入口なので索引は必ず更新する
    state.with_index(Sync::Force, |vault, conn, mut degraded| {
        degraded.extend(pull_degraded.clone());
        home_state_from(vault, conn, degraded)
    })
}

fn home_state_from(
    vault: &kb_core::vault::Vault,
    conn: &kb_core::rusqlite::Connection,
    mut degraded: Vec<kb_core::degradation::Degradation>,
) -> AppResult<HomeState> {
    let notes = recent(conn, 500).map_err(AppError::index)?;
    // お手入れは補助情報なので本体を止めないが、失敗を空一覧と偽らない。
    if let Err(error) = kb_core::care::detect(conn, vault) {
        degraded.push(kb_core::degradation::Degradation::CareDetection {
            detail: error.to_string(),
        });
    }
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
#[tauri::command]
#[specta::specta]
pub fn tag_overview(state: State<'_, AppState>) -> AppResult<TagOverview> {
    state.with_index(Sync::Throttled, |_, conn, degraded| {
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

    /// careの表だけが壊れても最近のノートを返し、検知・一覧の失敗を別codeで残す。
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

        let home = home_state_from(&vault, &conn, Vec::new()).unwrap();
        assert_eq!(home.notes.len(), 1);
        assert!(home.care.is_empty());
        assert!(home.degraded.iter().any(|item| matches!(
            item,
            kb_core::degradation::Degradation::CareDetection { .. }
        )));
        assert!(
            home.degraded
                .iter()
                .any(|item| matches!(item, kb_core::degradation::Degradation::CareList { .. }))
        );
    }
}

#[tauri::command]
#[specta::specta]
pub fn care_dismiss(state: State<'_, AppState>, key: String) -> AppResult<()> {
    state.with_index(Sync::Throttled, |_, conn, _| {
        kb_core::care::dismiss(conn, &key).map_err(AppError::index)
    })
}
