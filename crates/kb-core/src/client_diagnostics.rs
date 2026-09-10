//! 登録・stdout・受信・実操作を同じ「接続済み」へ畳まないための読み取り専用診断。

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

use crate::client_surface::ClientSurface;
use crate::mcp::ToolSurface;
use crate::rule_identity::{RuleIdentity, WorkspaceRuleIdentity};
use crate::session_ledger::{
    self, SummaryQuery,
    rule_delivery::{self, RuleOutputDiagnostic, RuleOutputState},
};
use crate::vault::Vault;

// 過去の応答件数を示す観測窓。稼働中プロセスの確認有効期限や正式対応の判定ではない。
const WINDOW_DAYS: u32 = 30;

#[derive(Clone, Copy, Debug, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    Unverified,
}

#[derive(Clone, Debug, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct OperationObservations {
    pub propose_successes: u64,
    pub update_successes: u64,
    pub propose_errors: u64,
    pub update_errors: u64,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ClientRuleDiagnostics {
    pub surface: ClientSurface,
    pub rules: RuleIdentity,
    pub hook_output: RuleOutputDiagnostic,
    pub receipt: ReceiptStatus,
    pub operations: OperationObservations,
    pub observations_available: bool,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ClientDiagnosticsReport {
    pub observed_at_ms: i64,
    pub window_days: u32,
    pub os: String,
    pub workspace: WorkspaceRuleIdentity,
    pub clients: Vec<ClientRuleDiagnostics>,
}

pub fn read(vault: &Vault, conn: &Connection) -> Result<ClientDiagnosticsReport> {
    let workspace_id = crate::workspace::stored_workspace_id(vault)?;
    let snapshot = conn.unchecked_transaction()?;
    let workspace = crate::rule_identity::workspace_snapshot(&snapshot, &workspace_id)?;
    snapshot.commit()?;
    let at = session_ledger::now_ms();
    let query = SummaryQuery {
        since_ms: at
            .saturating_sub(i64::from(WINDOW_DAYS) * 86_400_000)
            .max(0),
        until_ms: at.saturating_add(1),
        workspace_id: Some(workspace_id),
    };
    let summary = session_ledger::summary(&query);
    let mut clients = Vec::new();
    for (surface, hint) in [
        (ClientSurface::CodexCli, "codex-cli/gpt"),
        (ClientSurface::ClaudeCode, "claude-code/claude"),
        (ClientSurface::ClaudeDesktop, "claude-desktop/claude"),
    ] {
        let rules = crate::mcp::rule_identity_for(hint, ToolSurface::Read, true);
        let output = rule_delivery::inspect(&query, &rules, &workspace);
        // 観測台帳が壊れても登録修復は可能。読めない件数を「ゼロ件」と確定させない。
        let observations_available = summary.is_ok() && output.is_ok();
        let operations = summary
            .as_ref()
            .ok()
            .and_then(|summary| {
                summary
                    .workspaces
                    .iter()
                    .find(|entry| entry.workspace_id == workspace.workspace_id)
            })
            .and_then(|entry| entry.surfaces.iter().find(|entry| entry.surface == surface))
            .map(|entry| OperationObservations {
                propose_successes: entry.propose_successes,
                update_successes: entry.update_successes,
                propose_errors: entry.propose_errors,
                update_errors: entry.update_errors,
            })
            .unwrap_or_default();
        clients.push(ClientRuleDiagnostics {
            surface,
            rules,
            operations,
            observations_available,
            receipt: ReceiptStatus::Unverified,
            hook_output: output.unwrap_or(RuleOutputDiagnostic {
                state: RuleOutputState::NotObserved,
                last_observed_at_ms: None,
            }),
        });
    }
    Ok(ClientDiagnosticsReport {
        observed_at_ms: at,
        window_days: WINDOW_DAYS,
        os: std::env::consts::OS.into(),
        workspace,
        clients,
    })
}
