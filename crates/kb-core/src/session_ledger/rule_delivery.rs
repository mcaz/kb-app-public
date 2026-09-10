//! 子MCPで確認した版とhook stdoutの観測を結ぶ。hostの受信や規則遵守は推定しない。

use super::*;
use crate::rule_identity::{RuleIdentity, WorkspaceRuleIdentity};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleEvidence {
    pub rules: RuleIdentity,
    pub workspace: WorkspaceRuleIdentity,
}

impl RuleEvidence {
    pub(super) fn validate(&self, event: &LedgerEvent) -> Result<()> {
        self.rules.validate()?;
        self.workspace.validate()?;
        ensure!(
            matches!(
                event.observation,
                Observation::Hook {
                    observation: HookObservation::OutputPrepared { .. }
                }
            ),
            "規則版は検索本文の出力準備にだけ付ける"
        );
        ensure!(
            self.rules.client_surface == event.surface
                && Some(self.workspace.workspace_id.as_str()) == event.workspace_id.as_deref()
                && self.rules.tool_surface == crate::mcp::ToolSurface::Read,
            "規則版のsurfaceまたはworkspaceが観測と不一致"
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum RuleOutputState {
    NotObserved,
    CurrentRules,
    DifferentRules,
    VocabularyUnverified,
    ConcurrentOutputs,
    StdoutFailed,
    PreparedOnly,
    NotApplicable,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RuleOutputDiagnostic {
    pub state: RuleOutputState,
    pub last_observed_at_ms: Option<i64>,
}

pub(crate) fn inspect(
    query: &SummaryQuery,
    rules: &RuleIdentity,
    workspace: &WorkspaceRuleIdentity,
) -> Result<RuleOutputDiagnostic> {
    inspect_at(&runtime_path()?, query, rules, workspace)
}

fn inspect_at(
    path: &Path,
    query: &SummaryQuery,
    rules: &RuleIdentity,
    workspace: &WorkspaceRuleIdentity,
) -> Result<RuleOutputDiagnostic> {
    rules.validate()?;
    workspace.validate()?;
    ensure!(
        query.since_ms >= 0 && query.until_ms > query.since_ms,
        "規則観測の期間が不正"
    );
    ensure!(
        query.workspace_id.as_deref() == Some(workspace.workspace_id.as_str()),
        "規則観測の接続先が不一致"
    );
    let mut result = RuleOutputDiagnostic {
        state: if matches!(
            rules.client_surface,
            ClientSurface::CodexCli | ClientSurface::ClaudeCode
        ) {
            RuleOutputState::NotObserved
        } else {
            RuleOutputState::NotApplicable
        },
        last_observed_at_ms: None,
    };
    scan_validated_events(
        path,
        query,
        Some(&workspace.workspace_id),
        false,
        |event, emission| {
            if event.surface != rules.client_surface
                || !matches!(
                    event.observation,
                    Observation::Hook {
                        observation: HookObservation::OutputPrepared { .. }
                    }
                )
            {
                return Ok(());
            }
            // 古い成功より新しい失敗・版なし観測を優先し、再起動後の成功を捏造しない。
            let next_state = match emission {
                Some("stdout_failed") => RuleOutputState::StdoutFailed,
                Some("prepared") => RuleOutputState::PreparedOnly,
                Some("emitted") => match &event.rule_evidence {
                    Some(evidence)
                        if evidence.rules == *rules && evidence.workspace == *workspace =>
                    {
                        if workspace.vocabulary.source_status
                            == crate::rule_identity::VocabularySourceStatus::Pinned
                        {
                            RuleOutputState::CurrentRules
                        } else {
                            RuleOutputState::VocabularyUnverified
                        }
                    }
                    Some(_) => RuleOutputState::DifferentRules,
                    None => RuleOutputState::NotObserved,
                },
                _ => unreachable!("検証済み出力観測だけが渡る"),
            };
            result.state = if result.last_observed_at_ms == Some(event.observed_at_ms)
                && result.state != next_state
            {
                // ミリ秒が同じ並行出力にhash順の前後関係を付けない。
                RuleOutputState::ConcurrentOutputs
            } else {
                next_state
            };
            result.last_observed_at_ms = Some(event.observed_at_ms);
            Ok(())
        },
    )?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const WORKSPACE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

    fn identity() -> RuleEvidence {
        RuleEvidence {
            rules: crate::mcp::rule_identity_for(
                "codex-cli/gpt",
                crate::mcp::ToolSurface::Read,
                true,
            ),
            workspace: serde_json::from_value(json!({
                "schema": 1, "workspace_id": WORKSPACE,
                "vocabulary": {"source_status":"pinned", "source_note_uid":WORKSPACE,
                    "source_revision":WORKSPACE, "source_document_sha256":"c".repeat(64)}
            }))
            .unwrap(),
        }
    }

    fn prepared(at: i64, id: &str, evidence: Option<RuleEvidence>) -> LedgerEvent {
        let context = EventContext {
            surface: ClientSurface::CodexCli,
            workspace_id: Some(WORKSPACE),
            session_id: Some("rules"),
            prompt_id: Some(id),
            turn_id: None,
            permission_mode: None,
        };
        let delivery = crate::hook_delivery::render_hook_delivery(
            &json!({
                "hits": [], "documents": []
            }),
            context.surface.hook_output_budget(),
        )
        .unwrap();
        let mut event = LedgerEvent::hook(
            context,
            at,
            HookObservation::OutputPrepared {
                stats: delivery.stats,
                timings: HookTimings::default(),
                cap_assumption: CapAssumption::for_surface(context.surface),
            },
        )
        .unwrap();
        if let Some(evidence) = evidence {
            event = event.with_rule_evidence(evidence).unwrap();
        }
        event
    }

    /// 2026-09-08: 設定一致やstdoutの書込成功だけで現行規則の受入済みにしない。
    #[test]
    fn latest_output_distinguishes_missing_revision_and_failed_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite3");
        let at = now_ms();
        let evidence = identity();
        let query = SummaryQuery {
            since_ms: at - 1,
            until_ms: at + 100,
            workspace_id: Some(WORKSPACE.into()),
        };
        let inspect = || {
            inspect_at(&path, &query, &evidence.rules, &evidence.workspace)
                .unwrap()
                .state
        };
        assert_eq!(inspect(), RuleOutputState::NotObserved);
        for (offset, revision, outcome, expected) in [
            (
                0,
                Some(evidence.clone()),
                Some(HookEmissionOutcome::Emitted),
                RuleOutputState::CurrentRules,
            ),
            (
                1,
                None,
                Some(HookEmissionOutcome::Emitted),
                RuleOutputState::NotObserved,
            ),
            (
                2,
                Some(evidence.clone()),
                Some(HookEmissionOutcome::StdoutFailed),
                RuleOutputState::StdoutFailed,
            ),
            (
                3,
                Some(evidence.clone()),
                None,
                RuleOutputState::PreparedOnly,
            ),
        ] {
            let event = prepared(at + offset, &offset.to_string(), revision);
            let receipt = append_at_time(&path, &event, at + offset)
                .unwrap()
                .receipt
                .unwrap();
            if let Some(outcome) = outcome {
                finalize_hook_at(&path, &receipt, outcome).unwrap();
            }
            assert_eq!(inspect(), expected);
        }
    }

    #[test]
    fn rule_or_vocabulary_changes_and_other_workspaces_do_not_match() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite3");
        let at = now_ms();
        let evidence = identity();
        let event = prepared(at, "one", Some(evidence.clone()));
        let receipt = append_at_time(&path, &event, at).unwrap().receipt.unwrap();
        finalize_hook_at(&path, &receipt, HookEmissionOutcome::Emitted).unwrap();
        let query = SummaryQuery {
            since_ms: at - 1,
            until_ms: at + 1,
            workspace_id: Some(WORKSPACE.into()),
        };
        let mut changed = evidence.rules.clone();
        changed.instructions_sha256 = "a".repeat(64);
        assert_eq!(
            inspect_at(&path, &query, &changed, &evidence.workspace)
                .unwrap()
                .state,
            RuleOutputState::DifferentRules
        );
        let mut other = evidence.clone();
        other.workspace.workspace_id = "01ARZ3NDEKTSV4RRFFQ69G5FAW".into();
        assert!(event.clone().with_rule_evidence(other.clone()).is_err());
        let other_query = SummaryQuery {
            workspace_id: Some(other.workspace.workspace_id.clone()),
            ..query
        };
        assert_eq!(
            inspect_at(&path, &other_query, &other.rules, &other.workspace)
                .unwrap()
                .state,
            RuleOutputState::NotObserved
        );
        let mut changed = evidence.workspace.clone();
        changed.vocabulary = serde_json::from_value(
            json!({"source_status":"pinned", "source_note_uid":WORKSPACE,
            "source_revision":WORKSPACE,"source_document_sha256":"b".repeat(64)}),
        )
        .unwrap();
        let query = SummaryQuery {
            since_ms: at - 1,
            until_ms: at + 1,
            workspace_id: Some(WORKSPACE.into()),
        };
        assert_eq!(
            inspect_at(&path, &query, &evidence.rules, &changed)
                .unwrap()
                .state,
            RuleOutputState::DifferentRules
        );
    }

    #[test]
    fn concurrent_success_and_failure_do_not_depend_on_event_hash_order() {
        for reverse in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("ledger.sqlite3");
            let at = now_ms();
            let evidence = identity();
            let mut observations = vec![
                ("one", HookEmissionOutcome::Emitted),
                ("two", HookEmissionOutcome::StdoutFailed),
            ];
            if reverse {
                observations.reverse();
            }
            for (id, outcome) in observations {
                let event = prepared(at, id, Some(evidence.clone()));
                let receipt = append_at_time(&path, &event, at).unwrap().receipt.unwrap();
                finalize_hook_at(&path, &receipt, outcome).unwrap();
            }
            let query = SummaryQuery {
                since_ms: at - 1,
                until_ms: at + 1,
                workspace_id: Some(WORKSPACE.into()),
            };
            assert_eq!(
                inspect_at(&path, &query, &evidence.rules, &evidence.workspace)
                    .unwrap()
                    .state,
                RuleOutputState::ConcurrentOutputs
            );
        }
    }
}
