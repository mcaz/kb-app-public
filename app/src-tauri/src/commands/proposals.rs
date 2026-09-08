//! 提案の一覧・全文・本人による判断をコアへ取り次ぐ。

use kb_core::degradation::Degradation;
use kb_core::proposal_workflow::{self, DecisionInput, TicketView};
use serde::Serialize;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Serialize, specta::Type)]
pub struct ProposalListData {
    tickets: Vec<TicketView>,
    degraded: Vec<Degradation>,
}

#[derive(Serialize, specta::Type)]
pub struct ProposalDetailData {
    ticket: TicketView,
    degraded: Vec<Degradation>,
}

#[derive(Serialize, specta::Type)]
pub struct ProposalMutationData {
    ticket: TicketView,
    export_pending: bool,
    degraded: Vec<Degradation>,
}

#[tauri::command(async)]
#[specta::specta]
pub fn proposal_list(state: State<'_, AppState>) -> AppResult<ProposalListData> {
    state.with_db(|_, conn, degraded| {
        Ok(ProposalListData {
            tickets: proposal_workflow::list(conn).map_err(AppError::proposal)?,
            degraded,
        })
    })
}

#[tauri::command(async)]
#[specta::specta]
pub fn proposal_get(state: State<'_, AppState>, note: String) -> AppResult<ProposalDetailData> {
    state.with_db(|_, conn, degraded| {
        Ok(ProposalDetailData {
            ticket: proposal_workflow::get(conn, &note).map_err(AppError::proposal)?,
            degraded,
        })
    })
}

#[tauri::command(async)]
#[specta::specta]
pub fn proposal_decide(
    state: State<'_, AppState>,
    note: String,
    expected_etag: String,
    input: DecisionInput,
) -> AppResult<ProposalMutationData> {
    state.with_db(|vault, conn, degraded| {
        let result = proposal_workflow::decide(vault, conn, &note, &expected_etag, input)
            .map_err(AppError::proposal)?;
        Ok(ProposalMutationData {
            ticket: result.ticket,
            export_pending: result.export_pending,
            degraded,
        })
    })
}
