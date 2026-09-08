//! ホーム画面(健全性の要約・タグ・お手入れ提案)。

use kb_core::search::{Hit, Stats, recent, stats};
use serde::Serialize;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Serialize, specta::Type)]
pub struct HomeState {
    stats: Stats,
    note_count: usize,
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

/// 別MCPプロセスの保存を、一覧全件の取得や保守処理なしで検知する。
#[tauri::command(async)]
#[specta::specta]
pub fn note_revision(state: State<'_, AppState>) -> AppResult<String> {
    state.with_db(|_, conn, _| kb_core::cadence_cache::note_revision(conn).map_err(AppError::index))
}

/// 台帳障害やロック待ちを、最近のノート一覧の取得失敗へ波及させない。
#[tauri::command(async)]
#[specta::specta]
pub fn home_observation_health(
    state: State<'_, AppState>,
) -> AppResult<kb_core::observation_health::ObservationHealth> {
    let Ok(settings) = kb_core::settings::load() else {
        return Ok(kb_core::observation_health::ObservationHealth::unavailable());
    };
    Ok(kb_core::observation_health::read(&settings, || {
        // read()がOFFを先に判定し、台帳の読取り中は共有DBのロックを握らない。
        state
            .with_db(|vault, _, _| {
                kb_core::workspace::stored_workspace_id(vault).map_err(AppError::storage)
            })
            .map_err(|_| anyhow::anyhow!("観測対象のworkspaceを確認できない"))
    }))
}

#[tauri::command(async)]
#[specta::specta]
pub fn home_observation_trend(
    state: State<'_, AppState>,
    day_boundaries_ms: Vec<i64>,
    filter: kb_core::observation_trend::ObservationTrendFilter,
) -> AppResult<kb_core::observation_trend::ObservationTrend> {
    let Ok(settings) = kb_core::settings::load() else {
        return Ok(kb_core::observation_trend::ObservationTrend::unavailable());
    };
    Ok(kb_core::observation_trend::read(
        &settings,
        &day_boundaries_ms,
        filter,
        || {
            state
                .with_db(|vault, _, _| {
                    kb_core::workspace::stored_workspace_id(vault).map_err(AppError::storage)
                })
                .map_err(|_| anyhow::anyhow!("観測対象のworkspaceを確認できない"))
        },
    ))
}

#[tauri::command(async)]
#[specta::specta]
pub fn home_note_count_trend(
    state: State<'_, AppState>,
    local_today: String,
) -> AppResult<kb_core::note_count_history::NoteCountTrend> {
    let workspace = state.with_db(|vault, _, _| {
        kb_core::workspace::stored_workspace_id(vault).map_err(AppError::storage)
    });
    let Ok(workspace_id) = workspace else {
        return Ok(kb_core::note_count_history::NoteCountTrend::unavailable());
    };
    // 履歴の読取り中は共有ノートDBのロックを保持しない。
    Ok(kb_core::note_count_history::read(
        &workspace_id,
        &local_today,
    ))
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
        note_count: kb_core::search::browsable_note_count(conn).map_err(AppError::index)?,
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
    let mut degraded = kb_core::connect::pull_if_stale(&vault);
    let conn = kb_core::index::open_db(&vault).map_err(AppError::index)?;
    run_local_maintenance(
        &vault,
        &conn,
        &mut degraded,
        kb_core::note_count_history::observe,
    );
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
    observe_counts: impl FnOnce(
        &kb_core::vault::Vault,
        &kb_core::rusqlite::Connection,
    ) -> anyhow::Result<()>,
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
    // 保守後の確定した件数だけを観測する。履歴の失敗で保守結果を取り消さない。
    if let Err(error) = observe_counts(vault, conn) {
        degraded.push(kb_core::degradation::Degradation::NoteCountHistory {
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

    /// 2026-09-08: 保守用Statsでは未採用提案も数えるため、通常参照の導線と母集団が違った。
    #[test]
    fn home_note_count_matches_unfiltered_browse_across_categories() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        for (id, status, allowed) in [
            ("root", "stable", 1),
            ("notes/a", "stable", 1),
            ("team/deep/b", "stable", 1),
            ("team/deprecated", "deprecated", 1),
            ("proposals/undecided", "stable", 0),
        ] {
            conn.execute(
                "INSERT INTO notes(id,title,status,body,tags,normal_reference_allowed)
                 VALUES (?1,?1,?2,'','',?3)",
                kb_core::rusqlite::params![id, status, allowed],
            )
            .unwrap();
        }
        let home = home_state_from(&conn, Vec::new()).unwrap();
        let page = kb_core::search::browse_notes(
            &conn,
            &[],
            kb_core::search::NoteBrowsePeriod::All,
            kb_core::search::NoteBrowseSort::Updated,
            None,
            100,
        )
        .unwrap();
        assert_eq!(home.note_count, 3);
        assert_eq!(home.note_count, page.total);
        assert_eq!(home.stats.total, 5, "他用途のStatsは書き換えない");
    }

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
                    judgment: None,
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
        run_local_maintenance(&vault, &conn, &mut degraded, |_, _| Ok(()));
        assert!(degraded.is_empty(), "{degraded:?}");
        let after = home_state_from(&conn, Vec::new()).unwrap();
        assert!(
            after.care.iter().any(|proposal| proposal.kind == "broken"),
            "バックグラウンド保守後は検知結果をDBから読める"
        );

        run_local_maintenance(&vault, &conn, &mut degraded, |_, _| {
            Err(anyhow::anyhow!("history locked"))
        });
        assert!(matches!(
            degraded.as_slice(),
            [kb_core::degradation::Degradation::NoteCountHistory { .. }]
        ));
        let with_history_failure = home_state_from(&conn, degraded).unwrap();
        assert_eq!(with_history_failure.stats.total, 1);
        assert!(
            with_history_failure
                .care
                .iter()
                .any(|proposal| proposal.kind == "broken")
        );
    }
}

#[tauri::command(async)]
#[specta::specta]
pub fn care_dismiss(state: State<'_, AppState>, key: String) -> AppResult<()> {
    state.with_db(|_, conn, _| kb_core::care::dismiss(conn, &key).map_err(AppError::index))
}
