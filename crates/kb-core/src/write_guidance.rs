//! 保存の成否から独立した、起票・更新後の判断材料。追加の書込や蒸留は実行しない。

use std::collections::HashSet;

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;

use crate::{distillation::DistillationPlanEntry, frontmatter::Note, vault::Vault};

// 長文ノートでも検索式と応答を抑える。候補は全文確認前の手掛かりに限る。
const QUERY_CHARS: usize = 512;
const NEIGHBOR_LIMIT: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub(crate) enum UpdateWarning {
    BodyReduced {
        before_chars: usize,
        after_chars: usize,
    },
    RelationsReduced {
        before_count: usize,
        after_count: usize,
    },
}

pub(crate) fn update_warnings(before: &Note, after: &Note) -> Vec<UpdateWarning> {
    let mut warnings = Vec::new();
    let body_chars = |body: &str| body.trim_start_matches('\n').trim_end().chars().count();
    let before_chars = body_chars(&before.body);
    let after_chars = body_chars(&after.body);
    // Unicode scalar数で50%以上の縮小を判定する。byte数やfrontmatterは含めない。
    // Note::to_file_stringと同じ正規化で、保存時に追加される末尾改行の差を除く。
    if before_chars > 0 && after_chars <= before_chars / 2 {
        warnings.push(UpdateWarning::BodyReduced {
            before_chars,
            after_chars,
        });
    }
    let before_count = before.front.relations.len();
    let after_count = after.front.relations.len();
    if after_count < before_count {
        warnings.push(UpdateWarning::RelationsReduced {
            before_count,
            after_count,
        });
    }
    warnings
}

#[derive(Debug, Serialize)]
pub(crate) struct GuidanceIssue {
    pub code: &'static str,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct WriteGuidance {
    pub schema: &'static str,
    pub advisory: bool,
    pub planner: Option<DistillationPlanEntry>,
    pub distillation: Option<crate::distillation_jobs::NoteJobStatus>,
    pub cadence: Option<crate::distillation_cadence::DistillationCadenceStatus>,
    pub neighbors: Vec<crate::search::Hit>,
    pub query_truncated: bool,
    pub scope_matches: Vec<DistillationPlanEntry>,
    pub scope_matches_truncated: bool,
    pub warnings: Vec<UpdateWarning>,
    pub degraded: Vec<GuidanceIssue>,
}

impl WriteGuidance {
    fn issue(&mut self, code: &'static str, error: impl ToString) {
        self.degraded.push(GuidanceIssue {
            code,
            detail: error.to_string(),
        });
    }

    pub(crate) fn text(&self) -> String {
        let mut lines = vec!["判断支援（保存済み・この案内は変更を実行しない）:".to_owned()];
        if let Some(job) = &self.distillation {
            lines.push(format!(
                "- 蒸留の処理状態: {}（版{}）。自動実行は設定画面のAI・モデル設定に従う",
                job.state, job.generation
            ));
        }
        if let Some(planner) = &self.planner {
            lines.push(format!(
                "- planner: {} — {}",
                planner.operation.as_str(),
                planner.reason
            ));
        }
        if let Some(cadence) = &self.cadence {
            let lanes = cadence
                .due_lanes()
                .iter()
                .map(|lane| lane.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "- メンテナンス期限: {}",
                if lanes.is_empty() {
                    "期限到来なし"
                } else {
                    &lanes
                }
            ));
            if cadence.last_failure.is_some() {
                lines.push("- 前回のメンテナンス受入失敗あり（cadence.last_failure参照）".into());
            }
        }
        if self.query_truncated {
            lines.push("- 関連検索は起票・更新内容の先頭512文字のみ".into());
        }
        for candidate in &self.neighbors {
            lines.push(format!(
                "- 関連候補（getで全文確認）: {} [{}]",
                candidate.id,
                candidate.title.as_deref().unwrap_or("無題")
            ));
        }
        for candidate in &self.scope_matches {
            lines.push(format!(
                "- 同じnamespace/scope: {} [{}]",
                candidate.note,
                candidate.title.as_deref().unwrap_or("無題")
            ));
        }
        if self.scope_matches_truncated {
            lines.push("- 同じnamespace/scopeの候補は先頭5件のみ".into());
        }
        for warning in &self.warnings {
            lines.push(match warning {
                UpdateWarning::BodyReduced {
                    before_chars,
                    after_chars,
                } => format!(
                    "- 警告 [body_reduced]: 本文が50%以上縮小（{before_chars}→{after_chars}文字）"
                ),
                UpdateWarning::RelationsReduced {
                    before_count,
                    after_count,
                } => format!(
                    "- 警告 [relations_reduced]: 関連数が減少（{before_count}→{after_count}件）"
                ),
            });
        }
        for issue in &self.degraded {
            lines.push(format!(
                "- 判断支援の取得失敗 [{}]: {}（保存成功は維持）",
                issue.code, issue.detail
            ));
        }
        lines.join("\n")
    }
}

pub(crate) fn collect(
    vault: &Vault,
    conn: &Connection,
    id: &str,
    warnings: Vec<UpdateWarning>,
) -> WriteGuidance {
    collect_with_cadence(vault, conn, id, warnings, |plan| {
        crate::distillation_cadence::status_for_plan(vault, plan)
    })
}

fn collect_with_cadence(
    vault: &Vault,
    conn: &Connection,
    id: &str,
    warnings: Vec<UpdateWarning>,
    cadence: impl FnOnce(
        &crate::distillation::DistillationPlan,
    ) -> Result<crate::distillation_cadence::DistillationCadenceStatus>,
) -> WriteGuidance {
    let mut guidance = WriteGuidance {
        schema: "kb-app.write-guidance/v1",
        advisory: true,
        planner: None,
        distillation: None,
        cadence: None,
        neighbors: Vec::new(),
        query_truncated: false,
        scope_matches: Vec::new(),
        scope_matches_truncated: false,
        warnings,
        degraded: Vec::new(),
    };
    // 同時更新でplannerと関連候補の参照状態が混ざらないよう、同じread snapshotを使う。
    let mut prepared_cache = None;
    let result = (|| -> Result<()> {
        let snapshot = conn.unchecked_transaction()?;
        let note = vault.read_note_from_db(&snapshot, id)?;
        match crate::distillation_jobs::note_status(&snapshot, id) {
            Ok(status) => guidance.distillation = status,
            Err(error) => guidance.issue("distillation_status", error),
        }
        let query = [
            note.front.title.as_deref().unwrap_or(""),
            note.front.description.as_deref().unwrap_or(""),
            &note.body,
        ]
        .join("\n");
        guidance.query_truncated = query.chars().count() > QUERY_CHARS;
        let query: String = query.chars().take(QUERY_CHARS).collect();
        let search = crate::search::search_for_note(&snapshot, &query, NEIGHBOR_LIMIT, id);
        guidance.neighbors = search.hits;
        for issue in search.degraded {
            guidance.issue(issue.code(), issue);
        }
        match crate::distillation::plan_in_transaction(&snapshot) {
            Ok(plan) => {
                match crate::cadence_cache::prepare(&snapshot, &plan) {
                    Ok(cache) => prepared_cache = Some(cache),
                    Err(error) => guidance.issue("cadence_cache", error),
                }
                match normal_reference_ids(&snapshot) {
                    Ok(visible) => {
                        let referable = |entry: &&DistillationPlanEntry| {
                            visible.contains(&entry.note)
                                && entry.depends_on.iter().all(|id| visible.contains(id))
                        };
                        guidance.planner = plan
                            .entries
                            .iter()
                            .filter(referable)
                            .find(|entry| entry.note == id)
                            .cloned();
                        let matches: Vec<_> = plan
                            .entries
                            .iter()
                            .filter(referable)
                            .filter(|entry| {
                                entry.note != id
                                    && entry
                                        .authority
                                        .as_ref()
                                        .zip(note.front.authority.as_ref())
                                        .is_some_and(|(other, current)| {
                                            other.namespace == current.namespace
                                                && other.scope == current.scope
                                        })
                            })
                            .collect();
                        guidance.scope_matches_truncated = matches.len() > NEIGHBOR_LIMIT;
                        guidance.scope_matches =
                            matches.into_iter().take(NEIGHBOR_LIMIT).cloned().collect();
                    }
                    Err(error) => guidance.issue("write_guidance_visibility", error),
                }
                match cadence(&plan) {
                    Ok(status) => guidance.cadence = Some(status),
                    Err(error) => guidance.issue("write_guidance_cadence", error),
                }
            }
            Err(error) => guidance.issue("write_guidance_planner", error),
        }
        snapshot
            .rollback()
            .context("判断支援のread snapshotを閉じられない")?;
        Ok(())
    })();
    if let Err(error) = result {
        guidance.issue("write_guidance_snapshot", error);
    }
    if let Some(cache) = prepared_cache
        && let Err(error) = crate::cadence_cache::publish(conn, &cache)
    {
        guidance.issue("cadence_cache", error);
    }
    guidance
}

fn normal_reference_ids(conn: &Connection) -> Result<HashSet<String>> {
    // メンテナンス用の全planは保ち、未採用票とその票に依存する助言を通常応答へ出さない。
    let mut statement = conn.prepare_cached(
        "SELECT id FROM notes WHERE status != 'deprecated' AND normal_reference_allowed = 1",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        authority::{Authority, AuthorityRole, AuthorityStatus, NoteNamespace},
        vault::NoteProposal,
    };

    fn setup() -> (tempfile::TempDir, Vault, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        (dir, vault, conn)
    }

    fn propose(
        vault: &Vault,
        conn: &Connection,
        title: &str,
        role: AuthorityRole,
        scope: &str,
        description: Option<&str>,
    ) -> String {
        vault
            .propose(
                conn,
                NoteProposal {
                    judgment: None,
                    title,
                    body: "検索判断材料 テスト本文",
                    description,
                    tags: &["test".into()],
                    authority: Authority {
                        namespace: if role == AuthorityRole::Record {
                            NoteNamespace::Records
                        } else {
                            NoteNamespace::Knowledge
                        },
                        role,
                        status: AuthorityStatus::Active,
                        scope: scope.into(),
                    },
                    relations: Vec::new(),
                    allow_new_tags: false,
                    client: "test/client",
                    actor: None,
                    revision: None,
                },
            )
            .unwrap()
    }

    #[test]
    fn planner_and_cadence_reuse_existing_rules_without_writing() {
        let (_dir, vault, conn) = setup();
        for (role, description, expected) in [
            (AuthorityRole::Canonical, Some("要約"), "keep"),
            (AuthorityRole::Canonical, None, "normalize"),
            (AuthorityRole::Record, Some("要約"), "extract"),
            (AuthorityRole::Proposal, Some("要約"), "unresolved"),
        ] {
            let id = propose(&vault, &conn, expected, role, expected, description);
            let before = crate::distillation::plan(&conn).unwrap();
            let status_before = crate::distillation_cadence::status(&vault, &conn).unwrap();
            let guidance = collect(&vault, &conn, &id, Vec::new());
            assert_eq!(
                guidance.planner.as_ref().unwrap().operation.as_str(),
                expected
            );
            assert_eq!(
                guidance.planner.as_ref().unwrap(),
                before
                    .entries
                    .iter()
                    .find(|entry| entry.note == id)
                    .unwrap()
            );
            let status = guidance.cadence.unwrap();
            assert_eq!(
                status.current_checkpoint_id,
                status_before.current_checkpoint_id
            );
            assert_eq!(status.lanes, status_before.lanes);
            assert!(!status.state_exists);
            assert_eq!(status.due_lanes().len(), 4);
            assert!(guidance.degraded.is_empty(), "{:?}", guidance.degraded);
            assert_eq!(crate::distillation::plan(&conn).unwrap(), before);
            assert_eq!(crate::note_store::pending_count(&conn).unwrap(), 0);
        }
    }

    #[test]
    fn search_finds_existing_duplicates_without_source_embeddings() {
        let (_dir, vault, conn) = setup();
        let existing = propose(
            &vault,
            &conn,
            "検索判断材料",
            AuthorityRole::Record,
            "duplicate",
            Some("要約"),
        );
        let created = propose(
            &vault,
            &conn,
            "検索判断材料",
            AuthorityRole::Record,
            "duplicate",
            Some("要約"),
        );
        let guidance = collect(&vault, &conn, &created, Vec::new());
        assert!(guidance.neighbors.iter().any(|hit| hit.id == existing));
        assert!(!guidance.neighbors.iter().any(|hit| hit.id == created));
        assert!(
            guidance
                .scope_matches
                .iter()
                .any(|entry| entry.note == existing)
        );
        assert!(guidance.neighbors.iter().all(|hit| hit.distance.is_none()));
    }

    fn decision_note(
        vault: &Vault,
        conn: &Connection,
        title: &str,
        role: AuthorityRole,
        relations: Vec<crate::authority::NoteRelation>,
    ) -> String {
        vault
            .propose(
                conn,
                NoteProposal {
                    judgment: None,
                    title,
                    body: "検索判断材料 テスト本文",
                    description: Some("判断支援の参照境界"),
                    tags: &["test".into()],
                    authority: Authority {
                        namespace: NoteNamespace::Decisions,
                        role,
                        status: AuthorityStatus::Active,
                        scope: "test/proposal-visibility".into(),
                    },
                    relations,
                    allow_new_tags: false,
                    client: "test/client",
                    actor: None,
                    revision: None,
                },
            )
            .unwrap()
    }

    fn hidden_ticket(
        vault: &Vault,
        conn: &Connection,
        title: &str,
    ) -> crate::proposal_workflow::TicketView {
        crate::proposal_workflow::create(
            vault,
            conn,
            crate::proposal_workflow::ProposalInput {
                title: title.into(),
                problem: "検索判断材料に未採用票が混ざる".into(),
                proposal: "通常参照から分離する".into(),
                impact: "レビュー経路で扱う".into(),
                acceptance: "未採用票を候補に出さない".into(),
                tags: vec!["test".into()],
                scope: "test/proposal-visibility".into(),
            },
            "codex",
        )
        .unwrap()
        .ticket
    }

    /// 2026-09-06: 通常検索を閉じても、全planのscope候補から未採用票が漏れていた。
    #[test]
    fn hidden_proposals_are_filtered_before_scope_limits_without_changing_maintenance() {
        let (_dir, vault, conn) = setup();
        let source = decision_note(
            &vault,
            &conn,
            "保存対象",
            AuthorityRole::Proposal,
            Vec::new(),
        );
        let visible = decision_note(
            &vault,
            &conn,
            "同scopeの通常記録",
            AuthorityRole::Proposal,
            Vec::new(),
        );
        let hidden: Vec<_> = (0..6)
            .map(|index| hidden_ticket(&vault, &conn, &format!("private-only-proposal-{index}")))
            .collect();
        let before = crate::distillation::plan(&conn).unwrap();
        let cadence_before = crate::distillation_cadence::status(&vault, &conn).unwrap();
        let guidance = collect(&vault, &conn, &source, Vec::new());
        assert!(guidance.degraded.is_empty(), "{:?}", guidance.degraded);
        assert_eq!(guidance.scope_matches.len(), 1);
        assert_eq!(guidance.scope_matches[0].note, visible);
        assert!(!guidance.scope_matches_truncated);
        let output = serde_json::to_string(&guidance).unwrap();
        let text = guidance.text();
        for ticket in hidden {
            for value in [&ticket.note_id, &ticket.note_uid, &ticket.title] {
                assert!(!output.contains(value));
                assert!(!text.contains(value));
            }
        }
        assert_eq!(crate::distillation::plan(&conn).unwrap(), before);
        assert_eq!(
            guidance.cadence.as_ref().unwrap().current_checkpoint_id,
            cadence_before.current_checkpoint_id
        );
        assert_eq!(
            guidance.cadence.as_ref().unwrap().lanes,
            cadence_before.lanes
        );
    }

    /// 2026-09-06: 非表示depends_onのIDだけを削ると、隠れた案に基づく改訂助言が残る。
    #[test]
    fn guidance_omits_entries_depending_on_hidden_proposals_until_they_are_approved() {
        use crate::authority::{NoteRelation, RelationKind};
        use crate::proposal_workflow::{
            self, DecisionInput, DecisionOutcome, ReviewInput, ReviewRecommendation,
        };
        let (_dir, vault, conn) = setup();
        let ticket = hidden_ticket(&vault, &conn, "private-only-dependency");
        let canonical = decision_note(
            &vault,
            &conn,
            "既存の決定",
            AuthorityRole::Canonical,
            vec![NoteRelation {
                kind: RelationKind::Contradicts,
                target: ticket.note_uid.parse().unwrap(),
            }],
        );
        let source = decision_note(
            &vault,
            &conn,
            "新たな記録",
            AuthorityRole::Proposal,
            Vec::new(),
        );
        let before = crate::distillation::plan(&conn).unwrap();
        assert_eq!(
            before
                .entries
                .iter()
                .find(|entry| entry.note == canonical)
                .unwrap()
                .depends_on
                .as_slice(),
            std::slice::from_ref(&ticket.note_id)
        );
        let canonical_guidance = collect(&vault, &conn, &canonical, Vec::new());
        assert!(canonical_guidance.degraded.is_empty());
        assert!(canonical_guidance.planner.is_none());
        let source_guidance = collect(&vault, &conn, &source, Vec::new());
        assert!(source_guidance.scope_matches.is_empty());
        assert!(!source_guidance.scope_matches_truncated);
        for guidance in [&canonical_guidance, &source_guidance] {
            let output = serde_json::to_string(guidance).unwrap();
            for value in [&ticket.note_id, &ticket.note_uid, &ticket.title] {
                assert!(!output.contains(value));
                assert!(!guidance.text().contains(value));
            }
        }
        assert_eq!(crate::distillation::plan(&conn).unwrap(), before);

        let reviewed = proposal_workflow::review(
            &vault,
            &conn,
            &ticket.note_id,
            &ticket.etag,
            ReviewInput {
                summary: "参照境界を確認した".into(),
                benefits: "根拠を分離できる".into(),
                risks: "既に渡した文脈は消えない".into(),
                alternatives: "指示で区別する".into(),
                recommendation: ReviewRecommendation::Approve,
            },
            "claude",
        )
        .unwrap()
        .ticket;
        proposal_workflow::decide(
            &vault,
            &conn,
            &ticket.note_id,
            &reviewed.etag,
            DecisionInput {
                outcome: DecisionOutcome::Approve,
                reason: String::new(),
                next_action: String::new(),
            },
        )
        .unwrap();
        let after = collect(&vault, &conn, &canonical, Vec::new());
        assert_eq!(after.planner.unwrap().depends_on, [ticket.note_id]);
    }

    #[test]
    fn cadence_failure_keeps_planner_neighbors_and_saved_note() {
        let (_dir, vault, conn) = setup();
        propose(
            &vault,
            &conn,
            "検索判断材料",
            AuthorityRole::Proposal,
            "old",
            Some("要約"),
        );
        let id = propose(
            &vault,
            &conn,
            "検索判断材料 新規",
            AuthorityRole::Proposal,
            "new",
            Some("要約"),
        );
        let before = crate::distillation::plan(&conn).unwrap();
        let guidance = collect_with_cadence(&vault, &conn, &id, Vec::new(), |_| {
            anyhow::bail!("壊れた期限状態")
        });
        assert!(guidance.planner.is_some());
        assert!(!guidance.neighbors.is_empty());
        assert!(guidance.cadence.is_none());
        assert_eq!(guidance.degraded[0].code, "write_guidance_cadence");
        assert!(guidance.text().contains("保存成功は維持"));
        assert_eq!(crate::distillation::plan(&conn).unwrap(), before);
    }

    #[test]
    fn snapshot_failure_is_explicit_and_keeps_update_warnings() {
        let (_dir, vault, conn) = setup();
        let warnings = vec![UpdateWarning::BodyReduced {
            before_chars: 10,
            after_chars: 5,
        }];
        let guidance = collect(&vault, &conn, "notes/missing", warnings.clone());
        assert!(guidance.planner.is_none());
        assert_eq!(guidance.warnings, warnings);
        assert_eq!(guidance.degraded[0].code, "write_guidance_snapshot");
    }

    #[test]
    fn planner_failure_does_not_hide_search_results_or_claim_cadence_is_current() {
        let (_dir, vault, conn) = setup();
        let damaged = propose(
            &vault,
            &conn,
            "検索判断材料",
            AuthorityRole::Record,
            "old",
            Some("要約"),
        );
        let id = propose(
            &vault,
            &conn,
            "検索判断材料 新規",
            AuthorityRole::Record,
            "new",
            Some("要約"),
        );
        // 別ノートのDB documentだけが壊れ、検索索引はまだ使える状態を再現する。
        conn.execute(
            "UPDATE notes SET document = 'broken' WHERE id = ?1",
            [&damaged],
        )
        .unwrap();
        let guidance = collect(&vault, &conn, &id, Vec::new());
        assert!(guidance.planner.is_none());
        assert!(guidance.cadence.is_none());
        assert!(guidance.neighbors.iter().any(|hit| hit.id == damaged));
        assert!(
            guidance
                .degraded
                .iter()
                .any(|issue| issue.code == "write_guidance_planner")
        );
        assert!(vault.read_note_from_db(&conn, &id).is_ok());
    }

    #[test]
    fn query_and_scope_candidates_are_bounded_and_namespaces_remain_distinct() {
        let (_dir, vault, conn) = setup();
        for index in 0..7 {
            propose(
                &vault,
                &conn,
                &format!("検索判断材料 {index}"),
                AuthorityRole::Record,
                "shared",
                Some("要約"),
            );
        }
        let other_namespace = propose(
            &vault,
            &conn,
            "別namespace",
            AuthorityRole::Canonical,
            "shared",
            Some("要約"),
        );
        let id = propose(
            &vault,
            &conn,
            "長文",
            AuthorityRole::Record,
            "shared",
            Some("要約"),
        );
        let mut note = vault.read_note_from_db(&conn, &id).unwrap();
        note.body = "検索判断材料".repeat(200);
        crate::note_store::put(
            &vault,
            &conn,
            &id,
            &note,
            crate::note_store::WriteAttribution::new(
                "fixture",
                "fixture",
                &crate::provenance::test_context(),
            ),
        )
        .unwrap();
        let guidance = collect(&vault, &conn, &id, Vec::new());
        assert!(guidance.query_truncated);
        assert!(guidance.neighbors.len() <= 5);
        assert_eq!(guidance.scope_matches.len(), 5);
        assert!(guidance.scope_matches_truncated);
        assert!(
            !guidance
                .scope_matches
                .iter()
                .any(|entry| entry.note == other_namespace)
        );
    }

    #[test]
    fn reduction_uses_unicode_characters_and_includes_exact_half() {
        let mut before = Note {
            front: crate::frontmatter::Frontmatter::new_note("元"),
            body: "あいうえお😀".into(),
        };
        let mut after = before.clone();
        after.body = "abc".into();
        assert_eq!(
            update_warnings(&before, &after),
            vec![UpdateWarning::BodyReduced {
                before_chars: 6,
                after_chars: 3
            }]
        );
        after.body = "abcd".into();
        assert!(update_warnings(&before, &after).is_empty());
        before.body.push('\n');
        after.body = "\nabc \n".into();
        assert_eq!(
            update_warnings(&before, &after),
            vec![UpdateWarning::BodyReduced {
                before_chars: 6,
                after_chars: 3
            }]
        );
        before.body.clear();
        after.body.clear();
        assert!(update_warnings(&before, &after).is_empty());
    }
}
