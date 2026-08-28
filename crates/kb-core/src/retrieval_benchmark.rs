//! Google型retrieval改善の前後を比べる合成benchmark fixture。
//!
//! 実KBや実際の発話を入力にせず、毎回隔離した一時Vaultへ同じノート集合を作る。
//! `controls`は現行品質の回帰gate、`challenges`は未実装の改善余地を測る診断集合。
//! 1.1.0 suiteは配信profile × rerankの比較軸を持ち、1.0.0 suiteは従来どおり
//! `top3` / `linked_v1`だけで評価する(profile / rerank指定は無視する)。

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::authority::{Authority, NoteRelation, RelationKind};
use crate::retrieval_eval::{
    EvaluationPlan, EvaluationReport, EvaluationStrategy, GoldenSuite,
    LEGACY_EVALUATION_SCHEMA_VERSION,
};
use crate::retrieval_profile::{RerankMode, RetrievalProfile};
use crate::vault::{NoteProposal, NoteUpdate, Vault};

pub const RETRIEVAL_BENCHMARK_SCHEMA_VERSION: &str = "1.1.0";
/// profile / rerank軸を持たない従来形式。google / holdoutの2 suiteはこの形のまま凍結する。
pub const LEGACY_RETRIEVAL_BENCHMARK_SCHEMA_VERSION: &str = "1.0.0";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalBenchmarkSuite {
    pub schema_version: String,
    pub notes: Vec<RetrievalFixtureNote>,
    pub controls: GoldenSuite,
    pub challenges: GoldenSuite,
    /// 1.1.0: 省略時は`["session_auto"]`。CLIの`--profiles`が優先する
    #[serde(default)]
    pub profiles: Option<Vec<RetrievalProfile>>,
    /// 1.1.0: 省略時は`off`。CLIの`--rerank`が優先する
    #[serde(default)]
    pub rerank: Option<RerankMode>,
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

/// 1回の計測の指定。suite側の既定をCLI側で上書きする。
#[derive(Clone, Debug, Default)]
pub struct BenchmarkRunOptions {
    pub profiles: Option<Vec<RetrievalProfile>>,
    pub rerank: Option<RerankMode>,
    /// suite fileのSHA-256。比較表でfixtureの同一性を示すためにreportへ写す
    pub fixture_digest: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RetrievalBenchmarkReport {
    pub schema_version: &'static str,
    pub suite_schema_version: String,
    pub core_version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixture_digest: Option<String>,
    pub fixture_note_count: usize,
    /// 実際に評価したprofile。1.0.0 suiteでは空
    pub profiles: Vec<RetrievalProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerank: Option<RerankMode>,
    pub controls: EvaluationReport,
    pub challenges: EvaluationReport,
}

/// 合成suiteを隔離した一時Vaultでmaterializeし、controlとchallengeを同じ実装で測る。
pub fn evaluate(suite: &RetrievalBenchmarkSuite) -> Result<RetrievalBenchmarkReport> {
    evaluate_with(suite, &BenchmarkRunOptions::default())
}

pub fn evaluate_with(
    suite: &RetrievalBenchmarkSuite,
    run: &BenchmarkRunOptions,
) -> Result<RetrievalBenchmarkReport> {
    validate_suite(suite)?;
    let (profiles, rerank) = resolve_matrix(suite, run);
    let plan = match rerank {
        Some(rerank) => EvaluationPlan::with_profiles(&profiles, rerank),
        None => EvaluationPlan::classic(),
    };
    // fixture vaultを作る前に落とす。coreのrerank軸はoff固定(R4 I-5)。
    plan.ensure_rerank_off_for_core()?;
    let directory = tempfile::tempdir().context("retrieval benchmark用一時directoryを作れない")?;
    let vault = create_fixture(suite, &directory.path().join("vault"))?;
    let conn = crate::index::open_db(&vault)?;
    let controls = crate::retrieval_eval::evaluate_with(&conn, &suite.controls, &plan)?;
    let challenges = crate::retrieval_eval::evaluate_with(&conn, &suite.challenges, &plan)?;
    Ok(RetrievalBenchmarkReport {
        schema_version: RETRIEVAL_BENCHMARK_SCHEMA_VERSION,
        suite_schema_version: suite.schema_version.clone(),
        core_version: crate::CORE_VERSION,
        fixture_digest: run.fixture_digest.clone(),
        fixture_note_count: suite.notes.len(),
        profiles,
        rerank,
        controls,
        challenges,
    })
}

/// 1.0.0 suiteは指定を無視して従来出力、1.1.0 suiteはCLI > suite > 既定の順で決める。
fn resolve_matrix(
    suite: &RetrievalBenchmarkSuite,
    run: &BenchmarkRunOptions,
) -> (Vec<RetrievalProfile>, Option<RerankMode>) {
    if is_legacy(suite) {
        return (Vec::new(), None);
    }
    let mut profiles = Vec::new();
    for profile in run
        .profiles
        .clone()
        .or_else(|| suite.profiles.clone())
        .unwrap_or_else(|| vec![RetrievalProfile::SessionAuto])
    {
        if !profiles.contains(&profile) {
            profiles.push(profile);
        }
    }
    let rerank = run.rerank.or(suite.rerank).unwrap_or(RerankMode::Off);
    (profiles, Some(rerank))
}

pub fn is_legacy(suite: &RetrievalBenchmarkSuite) -> bool {
    suite.schema_version == LEGACY_RETRIEVAL_BENCHMARK_SCHEMA_VERSION
}

/// suite fileそのもののSHA-256(hex)。比較表でfixtureの同一性を示す軸にする。
pub fn fixture_digest(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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
    let profiles = if report.profiles.is_empty() {
        "-".to_string()
    } else {
        report
            .profiles
            .iter()
            .map(|profile| profile.label())
            .collect::<Vec<_>>()
            .join(", ")
    };
    output.push_str(&format!(
        "- schema: `{}` (suite `{}`)\n- core: `{}`\n- fixture digest: `{}`\n- synthetic notes: {}\n- profiles: {}\n- rerank: {}\n- control gate: **{}**\n- challenge gate: **{}**\n\n",
        report.schema_version,
        report.suite_schema_version,
        report.core_version,
        report.fixture_digest.as_deref().unwrap_or("-"),
        report.fixture_note_count,
        profiles,
        report.rerank.map(RerankMode::label).unwrap_or("-"),
        pass_fail(report.controls.gate.passed),
        pass_fail(report.challenges.gate.passed),
    ));
    output.push_str("| suite | strategy | gate | selected recall | precision | excluded | avg docs | avg tokens | tokens/required | required rank p50 | spill | budget exhausted | failed surfaces |\n");
    output.push_str(
        "| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n",
    );
    for (label, evaluation) in [
        ("controls", &report.controls),
        ("challenges", &report.challenges),
    ] {
        for summary in evaluation
            .summaries
            .iter()
            .filter(|summary| summary.strategy != EvaluationStrategy::Top3)
        {
            output.push_str(&format!(
                "| {label} | {} | {} | {:.1}% | {:.1}% | {} | {:.2} | {:.0} | {} | {} | {} | {} | {} |\n",
                summary.strategy.label(),
                pass_fail(summary.gate_passed),
                summary.macro_selected_recall * 100.0,
                summary.macro_selected_precision * 100.0,
                summary.excluded_violations,
                summary.average_selected_documents,
                summary.average_estimated_tokens,
                summary
                    .average_tokens_per_required
                    .map(|value| format!("{value:.0}"))
                    .unwrap_or_else(|| "-".into()),
                summary
                    .median_required_rank
                    .map(|rank| rank.to_string())
                    .unwrap_or_else(|| "-".into()),
                summary.spill_cases,
                summary.budget_exhausted_cases,
                summary.gate_failed_cases.len(),
            ));
        }
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
    let legacy = is_legacy(suite);
    if !legacy && suite.schema_version != RETRIEVAL_BENCHMARK_SCHEMA_VERSION {
        bail!(
            "retrieval benchmark schema_versionは{}または{}である必要がある: {}",
            LEGACY_RETRIEVAL_BENCHMARK_SCHEMA_VERSION,
            RETRIEVAL_BENCHMARK_SCHEMA_VERSION,
            suite.schema_version
        );
    }
    if legacy {
        if suite.profiles.is_some() || suite.rerank.is_some() {
            bail!(
                "profiles / rerankはschema_version {}でだけ使える",
                RETRIEVAL_BENCHMARK_SCHEMA_VERSION
            );
        }
        for (label, golden) in [
            ("controls", &suite.controls),
            ("challenges", &suite.challenges),
        ] {
            if golden.schema_version != LEGACY_EVALUATION_SCHEMA_VERSION {
                bail!(
                    "1.0.0 suiteの{label}はGolden Query schema_version {}に固定する: {}",
                    LEGACY_EVALUATION_SCHEMA_VERSION,
                    golden.schema_version
                );
            }
        }
    }
    if suite
        .profiles
        .as_ref()
        .is_some_and(|profiles| profiles.is_empty())
    {
        bail!("profilesを指定するなら1件以上にする");
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
    use crate::retrieval_eval::{BodyRequirementReport, CaseReport, CaseStrategyReport};

    const GOOGLE: &str =
        include_str!("../../../schemas/examples/retrieval-google-benchmark.example.json");
    const HOLDOUT: &str =
        include_str!("../../../schemas/examples/retrieval-realistic-holdout.example.json");
    const PROFILE_CONTEXT: &str =
        include_str!("../../../schemas/examples/retrieval-profile-context.example.json");

    fn suite(json: &str) -> RetrievalBenchmarkSuite {
        serde_json::from_str(json).unwrap()
    }

    fn linked(evaluation: &EvaluationReport) -> &crate::retrieval_eval::StrategySummary {
        evaluation
            .summaries
            .iter()
            .find(|summary| summary.strategy == EvaluationStrategy::LinkedV1)
            .expect("retrieval reportにはlinked_v1 summaryがある")
    }

    #[test]
    fn official_synthetic_benchmark_keeps_controls_and_records_known_gaps() {
        let report = evaluate(&suite(GOOGLE)).unwrap();

        assert!(report.controls.gate.passed);
        assert_eq!(report.controls.case_count, 15);
        assert_eq!(report.challenges.case_count, 18);
        assert!(report.challenges.gate.passed);
        assert!(report.challenges.gate.failed_cases.is_empty());
        let linked = linked(&report.challenges);
        assert_eq!(linked.spill_cases, 0);
        assert_eq!(linked.budget_exhausted_cases, 0);
        assert!(report.profiles.is_empty());
        assert_eq!(report.rerank, None);
        assert_eq!(report.suite_schema_version, "1.0.0");
        assert_eq!(report.controls.strategy_configurations.len(), 2);
        assert!(render_markdown(&report).contains("challenge gate: **PASS**"));
    }

    #[test]
    fn realistic_holdout_keeps_distinct_templates_and_all_challenges() {
        let report = evaluate(&suite(HOLDOUT)).unwrap();

        assert_eq!(report.fixture_note_count, 55);
        assert_eq!(report.controls.case_count, 15);
        assert_eq!(report.challenges.case_count, 21);
        assert!(report.controls.gate.passed);
        assert!(report.challenges.gate.passed);
        assert!(report.controls.gate.failed_cases.is_empty());
        assert!(report.challenges.gate.failed_cases.is_empty());
    }

    #[test]
    fn fixture_rejects_unknown_relation_targets_before_writing() {
        let mut suite = suite(GOOGLE);
        suite.notes[0].relations.push(RetrievalFixtureRelation {
            kind: RelationKind::Mentions,
            target_id: "notes/missing".into(),
        });
        assert!(validate_suite(&suite).is_err());
    }

    /// 1.0.0 fixtureはprofile / rerankの指定を受け付けず、CLI flagも無視して従来出力を返す。
    /// google / holdoutを実験のcontrolとして凍結するための固定(2026-08-28 実験契約 §8.1)。
    #[test]
    fn legacy_fixture_ignores_profile_flags_and_rejects_profile_fields() {
        let mut suite = suite(GOOGLE);
        let report = evaluate_with(
            &suite,
            &BenchmarkRunOptions {
                profiles: Some(vec![RetrievalProfile::SessionExplicit]),
                rerank: Some(RerankMode::On),
                fixture_digest: Some("abc".into()),
            },
        )
        .unwrap();
        assert!(report.profiles.is_empty());
        assert_eq!(report.rerank, None);
        assert_eq!(report.fixture_digest.as_deref(), Some("abc"));
        assert_eq!(report.controls.strategy_configurations.len(), 2);

        suite.profiles = Some(vec![RetrievalProfile::SessionAuto]);
        assert!(validate_suite(&suite).is_err());
        suite.profiles = None;
        suite.rerank = Some(RerankMode::Off);
        assert!(validate_suite(&suite).is_err());
        suite.rerank = None;
        suite.controls.schema_version = crate::retrieval_eval::EVALUATION_SCHEMA_VERSION.into();
        assert!(validate_suite(&suite).is_err());
    }

    /// R4 I-5: 統合coreのrerank軸は`off`のみ受理する。CLI flag(`--rerank on`)経由でも
    /// suite宣言経由でも明確なエラーで拒否し、ContextCard rerankを評価用ブランチへ隔離する。
    /// legacy 1.0.0 suiteが`on`指定を無視して従来出力を返す挙動は
    /// `legacy_fixture_ignores_profile_flags_and_rejects_profile_fields`で別途固定済み。
    #[test]
    fn core_rejects_rerank_on_with_a_clear_error() {
        let mut upgraded = suite(GOOGLE);
        upgraded.schema_version = RETRIEVAL_BENCHMARK_SCHEMA_VERSION.into();

        let error = evaluate_with(
            &upgraded,
            &BenchmarkRunOptions {
                profiles: Some(vec![RetrievalProfile::SessionAuto]),
                rerank: Some(RerankMode::On),
                fixture_digest: None,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("rerank=on"), "{error}");
        assert!(error.to_string().contains("off"), "{error}");

        upgraded.rerank = Some(RerankMode::On);
        let error = evaluate_with(&upgraded, &BenchmarkRunOptions::default()).unwrap_err();
        assert!(error.to_string().contains("rerank=on"), "{error}");
    }

    fn structural_view(
        result: &CaseStrategyReport,
    ) -> (Vec<String>, Vec<String>, bool, Vec<BodyRequirementReport>) {
        (
            result.candidate_ids.clone(),
            result
                .selected
                .iter()
                .map(|document| document.id.clone())
                .collect(),
            result.gate_passed,
            result.body_requirements.clone(),
        )
    }

    fn assert_same_structure(left: &CaseReport, right: &CaseReport) {
        assert_eq!(left.id, right.id);
        assert_eq!(left.surface, right.surface);
        assert_eq!(left.search_degraded, right.search_degraded);
        assert_eq!(left.stable, right.stable);
        let linked = left
            .strategies
            .iter()
            .find(|result| result.strategy == EvaluationStrategy::LinkedV1)
            .unwrap();
        let session_auto = right
            .strategies
            .iter()
            .find(|result| {
                result.strategy
                    == EvaluationStrategy::Profile {
                        profile: RetrievalProfile::SessionAuto,
                        rerank: RerankMode::Off,
                    }
            })
            .unwrap();
        assert_eq!(structural_view(linked), structural_view(session_auto));
        assert_eq!(linked.required_rank, session_auto.required_rank);
    }

    /// 実験契約 §8.1: `--profiles session_auto --rerank off`の結果は91b9bf2の`linked_v1`と
    /// 候補列・選択・gate・本文要件・劣化が一致していなければならない。google / holdoutを
    /// 1.1.0として走らせ、同じreport内の`linked_v1`と突き合わせる。
    #[test]
    fn session_auto_profile_matches_linked_v1_structure_on_control_suites() {
        for json in [GOOGLE, HOLDOUT] {
            let classic = evaluate(&suite(json)).unwrap();
            let mut upgraded = suite(json);
            upgraded.schema_version = RETRIEVAL_BENCHMARK_SCHEMA_VERSION.into();
            let profiled = evaluate_with(
                &upgraded,
                &BenchmarkRunOptions {
                    profiles: Some(vec![RetrievalProfile::SessionAuto]),
                    rerank: Some(RerankMode::Off),
                    fixture_digest: None,
                },
            )
            .unwrap();
            assert_eq!(profiled.profiles, [RetrievalProfile::SessionAuto]);
            assert_eq!(profiled.rerank, Some(RerankMode::Off));
            for (classic, profiled) in [
                (&classic.controls, &profiled.controls),
                (&classic.challenges, &profiled.challenges),
            ] {
                assert_eq!(classic.case_count, profiled.case_count);
                assert_eq!(profiled.strategy_configurations.len(), 3);
                for (left, right) in classic.cases.iter().zip(&profiled.cases) {
                    assert_same_structure(left, right);
                    let classic_linked = &left.strategies[1];
                    let profiled_linked = &right.strategies[1];
                    assert_eq!(
                        structural_view(classic_linked),
                        structural_view(profiled_linked)
                    );
                }
            }
        }
    }

    #[test]
    fn frozen_baseline_reports_match_the_current_control_suites() {
        for (json, baseline) in [
            (
                GOOGLE,
                include_str!("../../../docs/retrieval-experiment-base/google-benchmark.json"),
            ),
            (
                HOLDOUT,
                include_str!("../../../docs/retrieval-experiment-base/realistic-holdout.json"),
            ),
        ] {
            let report = evaluate(&suite(json)).unwrap();
            let current: serde_json::Value = serde_json::to_value(&report).unwrap();
            let frozen: serde_json::Value = serde_json::from_str(baseline).unwrap();
            for key in ["controls", "challenges"] {
                let current_cases = current[key]["cases"].as_array().unwrap();
                let frozen_cases = frozen[key]["cases"].as_array().unwrap();
                assert_eq!(current_cases.len(), frozen_cases.len(), "{key}");
                for (left, right) in current_cases.iter().zip(frozen_cases) {
                    assert_eq!(left["id"], right["id"]);
                    assert_eq!(left["surface"], right["surface"]);
                    assert_eq!(left["search_degraded"], right["search_degraded"]);
                    assert_eq!(left["stable"], right["stable"]);
                    let left_strategies = left["strategies"].as_array().unwrap();
                    let right_strategies = right["strategies"].as_array().unwrap();
                    assert_eq!(left_strategies.len(), right_strategies.len());
                    for (left, right) in left_strategies.iter().zip(right_strategies) {
                        for field in [
                            "strategy",
                            "candidate_ids",
                            "required_in_selected",
                            "excluded_in_selected",
                            "body_requirements",
                            "gate_passed",
                            "required_rank",
                        ] {
                            assert_eq!(
                                left[field], right[field],
                                "{key}/{}/{field}",
                                left["strategy"]
                            );
                        }
                        let ids = |value: &serde_json::Value| {
                            value["selected"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|document| document["id"].clone())
                                .collect::<Vec<_>>()
                        };
                        assert_eq!(ids(left), ids(right));
                    }
                }
            }
        }
    }

    /// 実験契約 §8.6: 新fixtureのfamilyごとの既知結果をbaselineとして固定する。
    /// PASSしているfamilyは各experiment branchで「維持」、FAILは「反転」の対象になる。
    /// requiredなどの期待値はbase凍結後に変えない(§4-D)。
    #[test]
    fn profile_context_fixture_records_known_family_results() {
        let suite = suite(PROFILE_CONTEXT);
        assert_eq!(suite.schema_version, RETRIEVAL_BENCHMARK_SCHEMA_VERSION);
        let report = evaluate(&suite).unwrap();

        // fixtureが宣言する3 profile matrixをそのまま走らせる(§8.10の計測commandと同じ)。
        assert_eq!(
            report.profiles,
            [
                RetrievalProfile::SessionAuto,
                RetrievalProfile::SessionExplicit,
                RetrievalProfile::RoutineAuto,
            ]
        );
        assert_eq!(report.rerank, Some(RerankMode::Off));
        assert_eq!(report.fixture_note_count, 97);
        assert_eq!(report.controls.case_count, 9);
        assert_eq!(report.challenges.case_count, 60);
        assert_eq!(report.controls.strategy_configurations.len(), 5);
        assert!(
            report.controls.gate.passed,
            "{:?}",
            report.controls.gate.failed_cases
        );
        assert!(!report.challenges.gate.passed);

        let expected = [
            ("isolated-fallback", true),
            ("interference", true),
            ("routine-template", true),
            ("explicit-precision", true),
            ("deep-signal", false),
            ("inbound-alias", false),
            ("long-heading", false),
            ("current-vs-history", false),
            ("rationale-bundle", true),
            ("multi-scope-compare", true),
        ];
        let families = report
            .controls
            .families
            .iter()
            .chain(&report.challenges.families)
            .collect::<Vec<_>>();
        assert_eq!(families.len(), expected.len());
        for (family, passed) in expected {
            let summary = families
                .iter()
                .find(|summary| summary.family == family)
                .unwrap_or_else(|| panic!("family {family} がreportにない"));
            let linked = summary
                .summaries
                .iter()
                .find(|summary| summary.strategy == EvaluationStrategy::LinkedV1)
                .unwrap();
            assert_eq!(
                linked.gate_passed, passed,
                "{family}: {:?}",
                linked.gate_failed_cases
            );
            let session_auto = summary
                .summaries
                .iter()
                .find(|summary| {
                    summary.strategy
                        == EvaluationStrategy::Profile {
                            profile: RetrievalProfile::SessionAuto,
                            rerank: RerankMode::Off,
                        }
                })
                .unwrap();
            assert_eq!(session_auto.gate_failed_cases, linked.gate_failed_cases);
        }
        for case in report.controls.cases.iter().chain(&report.challenges.cases) {
            assert!(case.stable, "{}@{:?}", case.id, case.surface);
            assert!(case.search_degraded.is_empty());
        }
        assert!(render_markdown(&report).contains("## Families"));
    }
}
