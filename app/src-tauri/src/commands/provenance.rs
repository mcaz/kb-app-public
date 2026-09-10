//! ノートの来歴(ADR-0023)を読む面。
//!
//! 統治のロジック(台帳の集計・フィルタ)は kb-core 側(`kb_core::provenance`)に
//! 置いてあるので、ここは GUI 向けの view 型へ詰め替えるだけの薄い口(coding-guidelines §4)。
//! `kb_core` の型をそのまま specta で公開すると `bindings.ts` の差分が大きくなるため、
//! `Operation` / `RevisionKind` / `ModelBasis` はここで文字列へ写す。

use kb_core::provenance::{self, ModelBasis, WriteActor};
use serde::Serialize;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// `note_history` / `activity_feed` の表示件数の上限。
/// MCP の `history` ツール(`crates/kb-core/src/mcp.rs`)と同じ値に揃える。
const MAX_LIMIT: u32 = 200;

fn model_basis_key(basis: ModelBasis) -> &'static str {
    match basis {
        ModelBasis::Handshake => "handshake",
        ModelBasis::AppApi => "app_api",
        ModelBasis::SelfReported => "self_reported",
        ModelBasis::Config => "config",
        ModelBasis::Unknown => "unknown",
    }
}

#[derive(Serialize, specta::Type)]
pub struct SectionAuthorView {
    heading: String,
    actor_label: String,
    at: String,
}

#[derive(Serialize, specta::Type)]
pub struct ProvenanceView {
    line: String,
    created_by: Option<String>,
    created_at: Option<String>,
    last_by: Option<String>,
    last_at: Option<String>,
    event_count: u32,
    distinct_actors: u32,
    section_authors: Vec<SectionAuthorView>,
}

fn provenance_view(summary: provenance::ProvenanceSummary) -> ProvenanceView {
    ProvenanceView {
        line: provenance::provenance_line(&summary),
        created_by: summary.created_by.as_ref().map(WriteActor::label),
        created_at: summary.created_at,
        last_by: summary.last_by.as_ref().map(WriteActor::label),
        last_at: summary.last_at,
        event_count: summary.event_count as u32,
        distinct_actors: summary.distinct_actors as u32,
        section_authors: summary
            .section_authors
            .into_iter()
            .map(|author| SectionAuthorView {
                heading: author.heading,
                actor_label: author.actor.label(),
                at: author.at,
            })
            .collect(),
    }
}

/// ノート1件の来歴要約(1行サマリ・見出しごとの最終書き手)。
#[tauri::command(async)]
#[specta::specta]
pub fn note_provenance(state: State<'_, AppState>, id: String) -> AppResult<ProvenanceView> {
    state.with_db(|vault, conn, _| {
        // note_getと同じ形にする — 不正なIDは「見つからない」ではなく「入力が不正」と
        // 分類したいので、Vault::note_provenance内部でのparseに任せず先に検査する。
        let note_id = kb_core::note_id::NoteId::parse(&id).map_err(AppError::invalid_input)?;
        let summary = vault
            .note_provenance(conn, note_id.as_str())
            .map_err(AppError::index)?;
        Ok(provenance_view(summary))
    })
}

#[derive(Serialize, specta::Type)]
pub struct FieldChangeView {
    field: String,
    from: String,
    to: String,
}

#[derive(Serialize, specta::Type)]
pub struct NoteEventView {
    event_id: String,
    at: String,
    operation: String,
    kind: String,
    actor_label: String,
    model_basis: String,
    summary: Option<String>,
    reason: Option<String>,
    origin_claim: Option<String>,
    sections: Vec<String>,
    changes: Vec<FieldChangeView>,
    /// `with_diff` を渡したときだけ入る(契約20: 台帳は本文を複製しないので、
    /// 一覧表示では毎回運ばない)。
    body_diff: Option<String>,
    diff_truncated: bool,
}

fn note_event_view(event: provenance::NoteEvent) -> NoteEventView {
    NoteEventView {
        event_id: event.event_id,
        at: event.at,
        operation: event.operation.as_str().to_string(),
        kind: event.kind.as_str().to_string(),
        model_basis: model_basis_key(event.actor.model_basis).to_string(),
        actor_label: event.actor.label(),
        summary: event.summary,
        reason: event.reason,
        origin_claim: event.origin_claim,
        sections: event.sections,
        changes: event
            .changes
            .into_iter()
            .map(|(field, change)| FieldChangeView {
                field,
                from: change.from.to_string(),
                to: change.to.to_string(),
            })
            .collect(),
        body_diff: event.body_diff,
        diff_truncated: event.diff_truncated,
    }
}

/// ノート1件のイベント履歴(新しい順)。既定では本文diffを運ばず、
/// `with_diff` を明示したときだけ含める(会話・画面の初期表示を軽くする)。
#[tauri::command(async)]
#[specta::specta]
pub fn note_history(
    state: State<'_, AppState>,
    id: String,
    limit: u32,
    with_diff: bool,
) -> AppResult<Vec<NoteEventView>> {
    let limit = limit.clamp(1, MAX_LIMIT) as usize;
    state.with_db(|vault, conn, _| {
        let note_id = kb_core::note_id::NoteId::parse(&id).map_err(AppError::invalid_input)?;
        let mut events = vault
            .note_history(conn, note_id.as_str(), limit)
            .map_err(AppError::index)?;
        if !with_diff {
            for event in &mut events {
                event.body_diff = None;
            }
        }
        Ok(events.into_iter().map(note_event_view).collect())
    })
}

#[derive(Serialize, specta::Type)]
pub struct ActivityRowView {
    event_id: String,
    note_id: String,
    title: String,
    at: String,
    actor_label: String,
    operation: String,
    kind: String,
    summary: Option<String>,
    section_count: u32,
}

#[derive(Serialize, specta::Type)]
pub struct ActivitySummaryView {
    last_7_days: u32,
    distinct_actors: u32,
    unknown_model_ratio: f32,
}

#[derive(Serialize, specta::Type)]
pub struct ActivityFeedView {
    rows: Vec<ActivityRowView>,
    summary: ActivitySummaryView,
    /// フィルタの選択肢(絞り込み結果ではなく、台帳に実在する値)。
    clients: Vec<String>,
    models: Vec<String>,
}

fn activity_feed_view(feed: provenance::ActivityFeed) -> ActivityFeedView {
    ActivityFeedView {
        rows: feed
            .rows
            .into_iter()
            .map(|row| ActivityRowView {
                event_id: row.event_id,
                note_id: row.note_id,
                title: row.title,
                at: row.at,
                actor_label: row.actor.label(),
                operation: row.operation.as_str().to_string(),
                kind: row.kind.as_str().to_string(),
                summary: row.summary,
                section_count: row.section_count as u32,
            })
            .collect(),
        summary: ActivitySummaryView {
            last_7_days: feed.summary.last_7_days as u32,
            distinct_actors: feed.summary.distinct_actors as u32,
            unknown_model_ratio: feed.summary.unknown_model_ratio,
        },
        clients: feed.clients,
        models: feed.models,
    }
}

/// ホームの活動ビュー。ノートを跨いで新しい順に書込を並べる。
#[tauri::command(async)]
#[specta::specta]
pub fn activity_feed(
    state: State<'_, AppState>,
    limit: u32,
    filter: provenance::ActivityFilter,
) -> AppResult<ActivityFeedView> {
    let limit = limit.clamp(1, MAX_LIMIT) as usize;
    state.with_db(|_, conn, _| {
        let feed = provenance::activity_feed(conn, &filter, limit).map_err(AppError::index)?;
        Ok(activity_feed_view(feed))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kb_core::index::open_db;
    use kb_core::vault::{NoteProposal, Vault};

    /// `TempDir` はguardを保持している間だけ実体がある。呼び出し面で束縛せず
    /// helperの戻り値だけ受け取ると、関数を抜けた時点でディレクトリごと消える
    /// (実際にこの取り違えで「repositoryが見つからない」を踏んだ — 2026-09-10)。
    fn propose(
        vault: &Vault,
        conn: &kb_core::rusqlite::Connection,
        title: &str,
        scope: &str,
        client: &str,
    ) -> String {
        vault
            .propose(
                conn,
                NoteProposal {
                    judgment: None,
                    title,
                    body: "## 背景\n最初の本文。",
                    description: None,
                    tags: &["test".into()],
                    authority: kb_core::authority::Authority {
                        namespace: kb_core::authority::NoteNamespace::Records,
                        role: kb_core::authority::AuthorityRole::Record,
                        status: kb_core::authority::AuthorityStatus::Active,
                        scope: format!("test/provenance-command/{scope}"),
                    },
                    relations: Vec::new(),
                    allow_new_tags: true,
                    client,
                    actor: None,
                    revision: None,
                },
            )
            .unwrap()
    }

    #[test]
    fn note_provenance_and_history_survive_one_propose() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        let id = propose(&vault, &conn, "来歴command", "a", "codex-cli/gpt-5.6-sol");

        let summary = vault.note_provenance(&conn, &id).unwrap();
        let view = provenance_view(summary);
        assert_eq!(view.event_count, 1);
        assert_eq!(view.distinct_actors, 1);
        assert!(view.created_by.as_deref().unwrap().contains("codex-cli"));
        assert_eq!(view.section_authors.len(), 1);

        let events = vault.note_history(&conn, &id, 10).unwrap();
        let views: Vec<NoteEventView> = events.into_iter().map(note_event_view).collect();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].operation, "propose");
        assert_eq!(views[0].kind, "create");
        assert_eq!(views[0].model_basis, "config");
        // 既定ではdiffを持たない書込(作成)なので、withoutでもwithでも差が出ない。
        assert!(views[0].body_diff.is_none());
    }

    #[test]
    fn activity_feed_view_carries_titles_and_filter_options() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        propose(
            &vault,
            &conn,
            "活動commandA",
            "a",
            "claude-code/claude-fable-5-1",
        );
        propose(&vault, &conn, "活動commandB", "b", "codex-cli");

        let feed =
            provenance::activity_feed(&conn, &provenance::ActivityFilter::default(), 30).unwrap();
        let view = activity_feed_view(feed);
        assert_eq!(view.rows.len(), 2);
        assert!(view.rows.iter().any(|row| row.title == "活動commandA"));
        assert_eq!(view.clients, vec!["claude-code", "codex-cli"]);
        assert_eq!(view.summary.distinct_actors, 2);
    }
}
