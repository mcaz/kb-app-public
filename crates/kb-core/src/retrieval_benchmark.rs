//! Google型retrieval改善の前後を比べる合成benchmark fixture。
//!
//! 実KBや実際の発話を入力にせず、毎回隔離した一時Vaultへ同じノート集合を作る。
//! `controls`は現行品質の回帰gate、`challenges`は未実装の改善余地を測る診断集合。

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::authority::{Authority, NoteRelation, RelationKind};
use crate::retrieval_eval::{EvaluationReport, GoldenSuite};
use crate::vault::{NoteProposal, NoteUpdate, Vault};

pub const RETRIEVAL_BENCHMARK_SCHEMA_VERSION: &str = "1.0.0";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalBenchmarkSuite {
    pub schema_version: String,
    pub notes: Vec<RetrievalFixtureNote>,
    pub controls: GoldenSuite,
    pub challenges: GoldenSuite,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalFixtureNote {
    pub expected_id: String,
    pub title: String,
    pub description: Option<String>,
    pub body: String,
    #[serde(default = "default_body_repetitions")]
    pub body_repetitions: usize,
    #[serde(default)]
    pub tags: Vec<String>,
    pub authority: Authority,
    #[serde(default)]
    pub relations: Vec<RetrievalFixtureRelation>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalFixtureRelation {
    #[serde(rename = "type")]
    pub kind: RelationKind,
    pub target_id: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RetrievalBenchmarkReport {
    pub schema_version: &'static str,
    pub core_version: &'static str,
    pub fixture_note_count: usize,
    pub controls: EvaluationReport,
    pub challenges: EvaluationReport,
}

/// 合成suiteを隔離した一時Vaultでmaterializeし、controlとchallengeを同じ実装で測る。
pub fn evaluate(suite: &RetrievalBenchmarkSuite) -> Result<RetrievalBenchmarkReport> {
    validate_suite(suite)?;
    let directory = tempfile::tempdir().context("retrieval benchmark用一時directoryを作れない")?;
    let vault = create_fixture(suite, &directory.path().join("vault"))?;
    let conn = crate::index::open_db(&vault)?;
    let controls = crate::retrieval_eval::evaluate(&conn, &suite.controls)?;
    let challenges = crate::retrieval_eval::evaluate(&conn, &suite.challenges)?;
    Ok(RetrievalBenchmarkReport {
        schema_version: RETRIEVAL_BENCHMARK_SCHEMA_VERSION,
        core_version: crate::CORE_VERSION,
        fixture_note_count: suite.notes.len(),
        controls,
        challenges,
    })
}

/// suiteのノートだけを持つVaultを作る。既存directoryへ混ぜない。
pub fn create_fixture(suite: &RetrievalBenchmarkSuite, path: &Path) -> Result<Vault> {
    validate_suite(suite)?;
    if path.exists() && std::fs::read_dir(path)?.next().is_some() {
        bail!(
            "retrieval benchmark fixtureの出力先が空でない: {}",
            path.display()
        );
    }
    let vault = Vault::create(path)?;
    let conn = crate::index::open_db(&vault)?;

    // relationのtarget UIDは全ノート作成後に確定するため、最初はrelationなしで置く。
    for note in &suite.notes {
        let body = note.body.repeat(note.body_repetitions);
        let id = vault.propose(
            &conn,
            NoteProposal {
                title: &note.title,
                body: &body,
                description: note.description.as_deref(),
                tags: &note.tags,
                authority: note.authority.clone(),
                relations: Vec::new(),
                allow_new_tags: true,
                client: "retrieval-benchmark/fixture",
            },
        )?;
        if id != note.expected_id {
            bail!(
                "retrieval fixture note IDが不一致: expected={} actual={}",
                note.expected_id,
                id
            );
        }
    }

    for note in suite.notes.iter().filter(|note| !note.relations.is_empty()) {
        let relations = note
            .relations
            .iter()
            .map(|relation| {
                let target = vault
                    .read_note_from_db(&conn, &relation.target_id)
                    .with_context(|| {
                        format!(
                            "{}のrelation targetを読めない: {}",
                            note.expected_id, relation.target_id
                        )
                    })?;
                let target = target
                    .front
                    .note_uid
                    .with_context(|| format!("note_uidがない: {}", relation.target_id))?;
                Ok(NoteRelation {
                    kind: relation.kind,
                    target,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        vault.agent_update_note(
            &conn,
            NoteUpdate {
                id: &note.expected_id,
                title: None,
                body: None,
                description: None,
                tags: None,
                authority: None,
                relations: Some(relations),
                allow_new_tags: false,
                client: "retrieval-benchmark/fixture",
            },
        )?;
    }
    crate::index::sync(&vault, &conn)?;
    Ok(vault)
}

pub fn render_markdown(report: &RetrievalBenchmarkReport) -> String {
    let mut output = String::from("# Google-style retrieval benchmark\n\n");
    output.push_str(&format!(
        "- schema: `{}`\n- core: `{}`\n- synthetic notes: {}\n- control gate: **{}**\n- challenge gate: **{}** (診断値。現行baselineではFAILを許容)\n\n",
        report.schema_version,
        report.core_version,
        report.fixture_note_count,
        pass_fail(report.controls.gate.passed),
        pass_fail(report.challenges.gate.passed),
    ));
    output.push_str("| suite | linked selected recall | linked precision | excluded | avg tokens | spill | budget exhausted | failed surfaces |\n");
    output.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for (label, evaluation) in [
        ("controls", &report.controls),
        ("challenges", &report.challenges),
    ] {
        let linked = evaluation
            .summaries
            .iter()
            .find(|summary| summary.strategy == crate::retrieval_eval::EvaluationStrategy::LinkedV1)
            .expect("retrieval reportにはlinked_v1 summaryがある");
        output.push_str(&format!(
            "| {label} | {:.1}% | {:.1}% | {} | {:.0} | {} | {} | {} |\n",
            linked.macro_selected_recall * 100.0,
            linked.macro_selected_precision * 100.0,
            linked.excluded_violations,
            linked.average_estimated_tokens,
            linked.spill_cases,
            linked.budget_exhausted_cases,
            evaluation.gate.failed_cases.len(),
        ));
    }
    output.push_str("\n## Control details\n\n");
    output.push_str(
        crate::retrieval_eval::render_markdown(&report.controls)
            .trim_start_matches("# Retrieval evaluation\n\n"),
    );
    output.push_str("\n\n## Challenge details\n\n");
    output.push_str(
        crate::retrieval_eval::render_markdown(&report.challenges)
            .trim_start_matches("# Retrieval evaluation\n\n"),
    );
    output
}

fn validate_suite(suite: &RetrievalBenchmarkSuite) -> Result<()> {
    if suite.schema_version != RETRIEVAL_BENCHMARK_SCHEMA_VERSION {
        bail!(
            "retrieval benchmark schema_versionは{}である必要がある: {}",
            RETRIEVAL_BENCHMARK_SCHEMA_VERSION,
            suite.schema_version
        );
    }
    if suite.notes.is_empty() {
        bail!("retrieval benchmark notesは1件以上必要");
    }
    let mut ids = BTreeSet::new();
    for note in &suite.notes {
        if !ids.insert(note.expected_id.as_str()) {
            bail!(
                "retrieval fixture note IDが重複している: {}",
                note.expected_id
            );
        }
        if note.title.trim().is_empty() || note.title != note.title.trim() {
            bail!(
                "{}のtitleは前後空白なしの非空文字列にする",
                note.expected_id
            );
        }
        if !(1..=5_000).contains(&note.body_repetitions) {
            bail!(
                "{}のbody_repetitionsは1以上5000以下にする",
                note.expected_id
            );
        }
        note.authority.validate()?;
    }
    for note in &suite.notes {
        for relation in &note.relations {
            if !ids.contains(relation.target_id.as_str()) {
                bail!(
                    "{}が未知のrelation targetを参照している: {}",
                    note.expected_id,
                    relation.target_id
                );
            }
        }
    }
    let mut case_ids = BTreeSet::new();
    for case in suite.controls.cases.iter().chain(&suite.challenges.cases) {
        if !case_ids.insert(case.id.as_str()) {
            bail!("control/challenge間でcase IDが重複している: {}", case.id);
        }
    }
    Ok(())
}

const fn default_body_repetitions() -> usize {
    1
}

const fn pass_fail(passed: bool) -> &'static str {
    if passed { "PASS" } else { "FAIL" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_synthetic_benchmark_keeps_controls_and_records_known_gaps() {
        let suite: RetrievalBenchmarkSuite = serde_json::from_str(include_str!(
            "../../../schemas/examples/retrieval-google-benchmark.example.json"
        ))
        .unwrap();
        let report = evaluate(&suite).unwrap();

        assert!(report.controls.gate.passed);
        assert_eq!(report.controls.case_count, 15);
        assert_eq!(report.challenges.case_count, 18);
        assert_eq!(
            report.challenges.gate.failed_cases,
            [
                "query-intent-historical@codex",
                "query-intent-historical@claude_code",
                "query-intent-historical@chatgpt",
                "anchor-text-alias@codex",
                "anchor-text-alias@claude_code",
                "anchor-text-alias@chatgpt",
                "typed-relation-ranking@codex",
                "typed-relation-ranking@claude_code",
                "typed-relation-ranking@chatgpt",
                "dedup-diversification@codex",
                "dedup-diversification@claude_code",
                "dedup-diversification@chatgpt",
            ]
        );
        let linked = report
            .challenges
            .summaries
            .iter()
            .find(|summary| summary.strategy == crate::retrieval_eval::EvaluationStrategy::LinkedV1)
            .unwrap();
        assert_eq!(linked.spill_cases, 3);
        assert_eq!(linked.budget_exhausted_cases, 3);
        assert!(render_markdown(&report).contains("現行baselineではFAILを許容"));
    }

    #[test]
    fn fixture_rejects_unknown_relation_targets_before_writing() {
        let mut suite: RetrievalBenchmarkSuite = serde_json::from_str(include_str!(
            "../../../schemas/examples/retrieval-google-benchmark.example.json"
        ))
        .unwrap();
        suite.notes[0].relations.push(RetrievalFixtureRelation {
            kind: RelationKind::Mentions,
            target_id: "notes/missing".into(),
        });
        assert!(validate_suite(&suite).is_err());
    }
}
