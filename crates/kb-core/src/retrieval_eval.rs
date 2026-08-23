//! リンク連鎖retrievalの再現可能な比較評価。
//!
//! 本番経路は`linked_v1`のまま維持し、旧`top3`はこの評価器の中だけで再現する。
//! 実利用の発話を自動収集せず、本人が用意したGolden Queryだけを端末内で読む。

use std::collections::{BTreeSet, HashSet};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension};

use crate::degradation::Degradation;
use crate::retrieval::{
    AUTO_SEED_LIMIT, RetrievalBundle, RetrievalOptions, RetrievalSource, context_documents,
    context_documents_for_query,
};

pub const EVALUATION_SCHEMA_VERSION: &str = "2.0.0";
pub const HOOK_SPILL_THRESHOLD_TOKENS: usize = 12_000;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenSuite {
    pub schema_version: String,
    #[serde(default = "default_stability_runs")]
    pub stability_runs: usize,
    pub cases: Vec<GoldenCase>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenCase {
    pub id: String,
    pub queries: SurfaceQueries,
    pub required: Vec<String>,
    #[serde(default)]
    pub relevant: Vec<String>,
    #[serde(default)]
    pub excluded: Vec<String>,
    #[serde(default)]
    pub body_requirements: Vec<BodyRequirement>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceQueries {
    pub codex: String,
    pub claude_code: String,
    pub chatgpt: String,
}

impl SurfaceQueries {
    fn iter(&self) -> [(EvaluationSurface, &str); 3] {
        [
            (EvaluationSurface::Codex, &self.codex),
            (EvaluationSurface::ClaudeCode, &self.claude_code),
            (EvaluationSurface::Chatgpt, &self.chatgpt),
        ]
    }
}

#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationSurface {
    Codex,
    ClaudeCode,
    Chatgpt,
}

impl EvaluationSurface {
    fn label(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude_code",
            Self::Chatgpt => "chatgpt",
        }
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BodyRequirement {
    pub id: String,
    #[serde(default)]
    pub all_terms: Vec<String>,
    #[serde(default)]
    pub any_terms: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationStrategy {
    Top3,
    LinkedV1,
}

impl EvaluationStrategy {
    fn label(self) -> &'static str {
        match self {
            Self::Top3 => "top3",
            Self::LinkedV1 => "linked_v1",
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct EvaluationReport {
    pub schema_version: &'static str,
    pub core_version: &'static str,
    pub case_count: usize,
    pub search_configuration: SearchConfiguration,
    pub strategy_configurations: Vec<StrategyConfiguration>,
    pub summaries: Vec<StrategySummary>,
    pub linked_minus_top3: StrategyDelta,
    pub gate: GateReport,
    pub cases: Vec<CaseReport>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct GateReport {
    pub strategy: EvaluationStrategy,
    pub passed: bool,
    pub failed_cases: Vec<String>,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct SearchConfiguration {
    pub ranked_hit_limit: usize,
    pub any_terms: bool,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct StrategyConfiguration {
    pub strategy: EvaluationStrategy,
    pub seed_limit: usize,
    pub max_depth: u8,
    pub candidate_limit: usize,
    pub document_limit: usize,
    /// `None`は評価専用top3 baselineの本文量予算なしを表す。
    pub estimated_token_budget: Option<usize>,
    pub include_incoming: bool,
}

impl StrategyConfiguration {
    fn retrieval_options(self) -> RetrievalOptions {
        RetrievalOptions {
            seed_limit: self.seed_limit,
            max_depth: self.max_depth,
            candidate_limit: self.candidate_limit,
            document_limit: self.document_limit,
            estimated_token_budget: self.estimated_token_budget.unwrap_or(usize::MAX),
            include_incoming: self.include_incoming,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct StrategySummary {
    pub strategy: EvaluationStrategy,
    pub cases: usize,
    pub macro_candidate_recall: f64,
    pub macro_selected_recall: f64,
    pub macro_selected_precision: f64,
    pub excluded_violations: usize,
    pub average_selected_documents: f64,
    pub average_estimated_tokens: f64,
    pub spill_cases: usize,
    pub candidate_cap_cases: usize,
    pub document_cap_cases: usize,
    pub budget_exhausted_cases: usize,
    pub selected_depth_0: usize,
    pub selected_depth_1: usize,
    pub selected_depth_2: usize,
    pub selected_incoming: usize,
    pub p50_elapsed_us: u64,
    pub p95_elapsed_us: u64,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct StrategyDelta {
    pub macro_candidate_recall: f64,
    pub macro_selected_recall: f64,
    pub macro_selected_precision: f64,
    pub excluded_violations: i64,
    pub average_selected_documents: f64,
    pub average_estimated_tokens: f64,
    pub spill_cases: i64,
    pub p95_elapsed_us: i64,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct CaseReport {
    pub id: String,
    pub surface: EvaluationSurface,
    pub query: String,
    pub required: Vec<String>,
    pub relevant: Vec<String>,
    pub excluded: Vec<String>,
    pub search_degraded: Vec<Degradation>,
    pub stable: bool,
    pub strategies: Vec<CaseStrategyReport>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct CaseStrategyReport {
    pub strategy: EvaluationStrategy,
    pub candidate_ids: Vec<String>,
    pub selected: Vec<EvaluatedDocument>,
    pub required_in_candidates: Vec<String>,
    pub required_in_selected: Vec<String>,
    pub excluded_in_selected: Vec<String>,
    pub body_requirements: Vec<BodyRequirementReport>,
    pub gate_passed: bool,
    pub candidate_recall: f64,
    pub selected_recall: f64,
    pub selected_precision: f64,
    pub runtime: EvaluationRuntime,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct BodyRequirementReport {
    pub id: String,
    pub passed: bool,
    pub matching_document_ids: Vec<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct EvaluatedDocument {
    pub id: String,
    pub source: RetrievalSource,
    pub depth: u8,
    pub estimated_tokens: usize,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct EvaluationRuntime {
    pub search_elapsed_us: u64,
    pub retrieval_elapsed_us: u64,
    pub total_elapsed_us: u64,
    pub seed_count: usize,
    pub candidate_count: usize,
    pub selected_count: usize,
    pub estimated_tokens: usize,
    pub spill: bool,
    pub budget_exhausted: bool,
    pub document_cap_reached: bool,
    pub candidate_cap_reached: bool,
    pub missing_documents: usize,
    pub selected_depth_0: usize,
    pub selected_depth_1: usize,
    pub selected_depth_2: usize,
    pub selected_incoming: usize,
}

/// sync済みDBを1つのread transactionで固定し、各queryを両戦略で比較する。
pub fn evaluate(conn: &Connection, suite: &GoldenSuite) -> Result<EvaluationReport> {
    let transaction = conn
        .unchecked_transaction()
        .context("retrieval評価用snapshotを開始できない")?;
    validate_suite(&transaction, suite)?;

    let mut cases = Vec::new();
    for case in &suite.cases {
        for (surface, query) in case.queries.iter() {
            let mut report = evaluate_case(&transaction, case, surface, query)?;
            for _ in 1..suite.stability_runs {
                let repeated = evaluate_case(&transaction, case, surface, query)?;
                if !same_retrieval_result(&report, &repeated) {
                    report.stable = false;
                }
            }
            cases.push(report);
        }
    }

    let top3 = summarize(&cases, EvaluationStrategy::Top3);
    let linked = summarize(&cases, EvaluationStrategy::LinkedV1);
    let delta = StrategyDelta {
        macro_candidate_recall: linked.macro_candidate_recall - top3.macro_candidate_recall,
        macro_selected_recall: linked.macro_selected_recall - top3.macro_selected_recall,
        macro_selected_precision: linked.macro_selected_precision - top3.macro_selected_precision,
        excluded_violations: signed_delta(
            linked.excluded_violations as u64,
            top3.excluded_violations as u64,
        ),
        average_selected_documents: linked.average_selected_documents
            - top3.average_selected_documents,
        average_estimated_tokens: linked.average_estimated_tokens - top3.average_estimated_tokens,
        spill_cases: signed_delta(linked.spill_cases as u64, top3.spill_cases as u64),
        p95_elapsed_us: signed_delta(linked.p95_elapsed_us, top3.p95_elapsed_us),
    };

    // read-only transactionを明示終了し、呼び出し側が同じConnectionを続けて使えるようにする。
    transaction.rollback()?;
    let failed_cases = cases
        .iter()
        .filter(|case| {
            !case.stable
                || !case.search_degraded.is_empty()
                || !case
                    .strategies
                    .iter()
                    .find(|result| result.strategy == EvaluationStrategy::LinkedV1)
                    .is_some_and(|result| result.gate_passed)
        })
        .map(case_key)
        .collect::<Vec<_>>();
    Ok(EvaluationReport {
        schema_version: EVALUATION_SCHEMA_VERSION,
        core_version: crate::CORE_VERSION,
        case_count: cases.len(),
        search_configuration: SearchConfiguration {
            ranked_hit_limit: AUTO_SEED_LIMIT,
            any_terms: true,
        },
        strategy_configurations: [EvaluationStrategy::Top3, EvaluationStrategy::LinkedV1]
            .into_iter()
            .map(configuration_for)
            .collect(),
        summaries: vec![top3, linked],
        linked_minus_top3: delta,
        gate: GateReport {
            strategy: EvaluationStrategy::LinkedV1,
            passed: failed_cases.is_empty(),
            failed_cases,
        },
        cases,
    })
}

fn evaluate_case(
    conn: &Connection,
    case: &GoldenCase,
    surface: EvaluationSurface,
    query: &str,
) -> Result<CaseReport> {
    let search_started = Instant::now();
    let outcome = crate::search::search_mode(conn, query, AUTO_SEED_LIMIT, true);
    let search_elapsed_us = micros(search_started.elapsed());
    let ranked_hit_ids = outcome
        .hits
        .iter()
        .map(|hit| hit.id.clone())
        .collect::<Vec<_>>();

    let mut strategies = Vec::with_capacity(2);
    for strategy in [EvaluationStrategy::Top3, EvaluationStrategy::LinkedV1] {
        let options = configuration_for(strategy).retrieval_options();
        let bundle = match strategy {
            EvaluationStrategy::Top3 => context_documents(conn, &ranked_hit_ids, options)?,
            EvaluationStrategy::LinkedV1 => {
                context_documents_for_query(conn, &ranked_hit_ids, query, options)?
            }
        };
        strategies.push(score_case(case, strategy, search_elapsed_us, bundle));
    }

    Ok(CaseReport {
        id: case.id.clone(),
        surface,
        query: query.to_string(),
        required: case.required.clone(),
        relevant: case.relevant.clone(),
        excluded: case.excluded.clone(),
        search_degraded: outcome.degraded,
        stable: true,
        strategies,
    })
}

fn configuration_for(strategy: EvaluationStrategy) -> StrategyConfiguration {
    match strategy {
        EvaluationStrategy::Top3 => StrategyConfiguration {
            strategy,
            seed_limit: 3,
            max_depth: 0,
            candidate_limit: 3,
            document_limit: 3,
            estimated_token_budget: None,
            include_incoming: false,
        },
        EvaluationStrategy::LinkedV1 => {
            let options = RetrievalOptions::default();
            StrategyConfiguration {
                strategy,
                seed_limit: options.seed_limit,
                max_depth: options.max_depth,
                candidate_limit: options.candidate_limit,
                document_limit: options.document_limit,
                estimated_token_budget: Some(options.estimated_token_budget),
                include_incoming: options.include_incoming,
            }
        }
    }
}

fn score_case(
    case: &GoldenCase,
    strategy: EvaluationStrategy,
    search_elapsed_us: u64,
    bundle: RetrievalBundle,
) -> CaseStrategyReport {
    let candidate_ids = bundle
        .candidates
        .iter()
        .map(|candidate| candidate.id.clone())
        .collect::<Vec<_>>();
    let candidate_set = candidate_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let selected_set = bundle
        .documents
        .iter()
        .map(|document| document.id.as_str())
        .collect::<HashSet<_>>();
    let relevant_set = case
        .required
        .iter()
        .chain(&case.relevant)
        .map(String::as_str)
        .collect::<HashSet<_>>();

    let required_in_candidates = case
        .required
        .iter()
        .filter(|id| candidate_set.contains(id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let required_in_selected = case
        .required
        .iter()
        .filter(|id| selected_set.contains(id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let excluded_in_selected = case
        .excluded
        .iter()
        .filter(|id| selected_set.contains(id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let relevant_selected = bundle
        .documents
        .iter()
        .filter(|document| relevant_set.contains(document.id.as_str()))
        .count();

    let body_requirements = case
        .body_requirements
        .iter()
        .map(|requirement| score_body_requirement(requirement, &bundle))
        .collect::<Vec<_>>();
    let gate_passed = required_in_selected.len() == case.required.len()
        && excluded_in_selected.is_empty()
        && body_requirements
            .iter()
            .all(|requirement| requirement.passed)
        && bundle.stats.missing_documents == 0;
    let retrieval_elapsed_us = bundle.stats.elapsed_us;
    let total_elapsed_us = search_elapsed_us.saturating_add(retrieval_elapsed_us);
    let selected = bundle
        .documents
        .iter()
        .map(|document| EvaluatedDocument {
            id: document.id.clone(),
            source: document.source,
            depth: document.depth,
            estimated_tokens: document.estimated_tokens,
        })
        .collect::<Vec<_>>();
    let selected_count = bundle.stats.selected_count;
    let estimated_tokens = bundle.stats.estimated_tokens;

    CaseStrategyReport {
        strategy,
        candidate_ids,
        selected,
        candidate_recall: ratio(required_in_candidates.len(), case.required.len()),
        selected_recall: ratio(required_in_selected.len(), case.required.len()),
        selected_precision: ratio(relevant_selected, selected_count),
        required_in_candidates,
        required_in_selected,
        excluded_in_selected,
        body_requirements,
        gate_passed,
        runtime: EvaluationRuntime {
            search_elapsed_us,
            retrieval_elapsed_us,
            total_elapsed_us,
            seed_count: bundle.stats.seed_count,
            candidate_count: bundle.stats.candidate_count,
            selected_count,
            estimated_tokens,
            spill: estimated_tokens > HOOK_SPILL_THRESHOLD_TOKENS,
            budget_exhausted: bundle.stats.budget_exhausted,
            document_cap_reached: bundle.stats.document_cap_reached,
            candidate_cap_reached: bundle.stats.candidate_cap_reached,
            missing_documents: bundle.stats.missing_documents,
            selected_depth_0: bundle.stats.selected_depth_0,
            selected_depth_1: bundle.stats.selected_depth_1,
            selected_depth_2: bundle.stats.selected_depth_2,
            selected_incoming: bundle.stats.selected_incoming,
        },
    }
}

fn score_body_requirement(
    requirement: &BodyRequirement,
    bundle: &RetrievalBundle,
) -> BodyRequirementReport {
    let all_terms_found = requirement.all_terms.iter().all(|term| {
        bundle
            .documents
            .iter()
            .any(|document| contains_term(&document.text, term))
    });
    let any_terms_found = requirement.any_terms.is_empty()
        || requirement.any_terms.iter().any(|term| {
            bundle
                .documents
                .iter()
                .any(|document| contains_term(&document.text, term))
        });
    let matching_document_ids = bundle
        .documents
        .iter()
        .filter(|document| {
            requirement
                .all_terms
                .iter()
                .chain(&requirement.any_terms)
                .any(|term| contains_term(&document.text, term))
        })
        .map(|document| document.id.clone())
        .collect();
    BodyRequirementReport {
        id: requirement.id.clone(),
        passed: all_terms_found && any_terms_found,
        matching_document_ids,
    }
}

fn contains_term(text: &str, term: &str) -> bool {
    text.to_lowercase().contains(&term.to_lowercase())
}

fn same_retrieval_result(left: &CaseReport, right: &CaseReport) -> bool {
    left.search_degraded == right.search_degraded
        && left
            .strategies
            .iter()
            .zip(&right.strategies)
            .all(|(left, right)| {
                left.strategy == right.strategy
                    && left.candidate_ids == right.candidate_ids
                    && left
                        .selected
                        .iter()
                        .map(|document| &document.id)
                        .eq(right.selected.iter().map(|document| &document.id))
                    && left.body_requirements == right.body_requirements
            })
}

fn case_key(case: &CaseReport) -> String {
    format!("{}@{}", case.id, case.surface.label())
}

const fn default_stability_runs() -> usize {
    1
}

fn summarize(cases: &[CaseReport], strategy: EvaluationStrategy) -> StrategySummary {
    let results = cases
        .iter()
        .filter_map(|case| {
            case.strategies
                .iter()
                .find(|result| result.strategy == strategy)
        })
        .collect::<Vec<_>>();
    let count = results.len();
    let mut elapsed = results
        .iter()
        .map(|result| result.runtime.total_elapsed_us)
        .collect::<Vec<_>>();
    elapsed.sort_unstable();

    StrategySummary {
        strategy,
        cases: count,
        macro_candidate_recall: mean(results.iter().map(|result| result.candidate_recall), count),
        macro_selected_recall: mean(results.iter().map(|result| result.selected_recall), count),
        macro_selected_precision: mean(
            results.iter().map(|result| result.selected_precision),
            count,
        ),
        excluded_violations: results
            .iter()
            .map(|result| result.excluded_in_selected.len())
            .sum(),
        average_selected_documents: mean(
            results
                .iter()
                .map(|result| result.runtime.selected_count as f64),
            count,
        ),
        average_estimated_tokens: mean(
            results
                .iter()
                .map(|result| result.runtime.estimated_tokens as f64),
            count,
        ),
        spill_cases: results.iter().filter(|result| result.runtime.spill).count(),
        candidate_cap_cases: results
            .iter()
            .filter(|result| result.runtime.candidate_cap_reached)
            .count(),
        document_cap_cases: results
            .iter()
            .filter(|result| result.runtime.document_cap_reached)
            .count(),
        budget_exhausted_cases: results
            .iter()
            .filter(|result| result.runtime.budget_exhausted)
            .count(),
        selected_depth_0: results
            .iter()
            .map(|result| result.runtime.selected_depth_0)
            .sum(),
        selected_depth_1: results
            .iter()
            .map(|result| result.runtime.selected_depth_1)
            .sum(),
        selected_depth_2: results
            .iter()
            .map(|result| result.runtime.selected_depth_2)
            .sum(),
        selected_incoming: results
            .iter()
            .map(|result| result.runtime.selected_incoming)
            .sum(),
        p50_elapsed_us: percentile(&elapsed, 50),
        p95_elapsed_us: percentile(&elapsed, 95),
    }
}

/// 人が差分を読める最小レポート。完全な候補列や劣化detailはJSON出力に残す。
pub fn render_markdown(report: &EvaluationReport) -> String {
    let mut out = String::from("# Retrieval evaluation\n\n");
    out.push_str(&format!(
        "- schema: `{}`\n- core: `{}`\n- cases: {}\n- gate: **{}** (`linked_v1`)\n- shared search: top {}, any_terms={}\n\n",
        report.schema_version,
        report.core_version,
        report.case_count,
        if report.gate.passed { "PASS" } else { "FAIL" },
        report.search_configuration.ranked_hit_limit,
        report.search_configuration.any_terms,
    ));
    out.push_str(
        "| strategy | seed | depth | candidates | documents | token budget | incoming |\n",
    );
    out.push_str("| --- | ---: | ---: | ---: | ---: | ---: | --- |\n");
    for configuration in &report.strategy_configurations {
        let token_budget = configuration
            .estimated_token_budget
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".into());
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            configuration.strategy.label(),
            configuration.seed_limit,
            configuration.max_depth,
            configuration.candidate_limit,
            configuration.document_limit,
            token_budget,
            configuration.include_incoming,
        ));
    }
    out.push('\n');
    out.push_str("| strategy | candidate recall | selected recall | precision | excluded | avg docs | avg tokens | spill | p50 μs | p95 μs |\n");
    out.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for summary in &report.summaries {
        out.push_str(&format!(
            "| {} | {:.1}% | {:.1}% | {:.1}% | {} | {:.2} | {:.0} | {} | {} | {} |\n",
            summary.strategy.label(),
            summary.macro_candidate_recall * 100.0,
            summary.macro_selected_recall * 100.0,
            summary.macro_selected_precision * 100.0,
            summary.excluded_violations,
            summary.average_selected_documents,
            summary.average_estimated_tokens,
            summary.spill_cases,
            summary.p50_elapsed_us,
            summary.p95_elapsed_us,
        ));
    }
    out.push_str("\n## linked_v1 − top3\n\n");
    out.push_str(&format!(
        "- candidate recall: {:+.1}pt\n- selected recall: {:+.1}pt\n- precision: {:+.1}pt\n- avg docs: {:+.2}\n- avg tokens: {:+.0}\n- spill cases: {:+}\n- p95: {:+} μs\n",
        report.linked_minus_top3.macro_candidate_recall * 100.0,
        report.linked_minus_top3.macro_selected_recall * 100.0,
        report.linked_minus_top3.macro_selected_precision * 100.0,
        report.linked_minus_top3.average_selected_documents,
        report.linked_minus_top3.average_estimated_tokens,
        report.linked_minus_top3.spill_cases,
        report.linked_minus_top3.p95_elapsed_us,
    ));
    out.push_str("\n## Cases\n\n");
    out.push_str("| case | surface | strategy | gate | stable | candidate recall | selected recall | precision | selected IDs | token | missing | total μs |\n");
    out.push_str(
        "| --- | --- | --- | --- | --- | ---: | ---: | ---: | --- | ---: | ---: | ---: |\n",
    );
    for case in &report.cases {
        for result in &case.strategies {
            let selected = result
                .selected
                .iter()
                .map(|document| document.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {:.1}% | {:.1}% | {:.1}% | {} | {} | {} | {} |\n",
                escape_table(&case.id),
                case.surface.label(),
                result.strategy.label(),
                if result.gate_passed { "PASS" } else { "FAIL" },
                case.stable,
                result.candidate_recall * 100.0,
                result.selected_recall * 100.0,
                result.selected_precision * 100.0,
                escape_table(&selected),
                result.runtime.estimated_tokens,
                result.runtime.missing_documents,
                result.runtime.total_elapsed_us,
            ));
        }
    }
    out
}

fn validate_suite(conn: &Connection, suite: &GoldenSuite) -> Result<()> {
    if suite.schema_version != EVALUATION_SCHEMA_VERSION {
        bail!(
            "Golden Query schema_versionは{}である必要がある: {}",
            EVALUATION_SCHEMA_VERSION,
            suite.schema_version
        );
    }
    if suite.cases.is_empty() {
        bail!("Golden Queryは1件以上必要");
    }
    if !(1..=10).contains(&suite.stability_runs) {
        bail!("stability_runsは1以上10以下にする");
    }

    let mut case_ids = BTreeSet::new();
    for case in &suite.cases {
        if case.id.trim().is_empty() || case.id != case.id.trim() {
            bail!("Golden Queryのidは前後空白なしの非空文字列にする");
        }
        if !case_ids.insert(case.id.as_str()) {
            bail!("Golden Queryのidが重複している: {}", case.id);
        }
        for (surface, query) in case.queries.iter() {
            if query.trim().is_empty() {
                bail!("{}: {:?} queryは空にできない", case.id, surface);
            }
        }
        if case.required.is_empty() {
            bail!("{}: requiredは1件以上必要", case.id);
        }
        let required = checked_ids(&case.id, "required", &case.required)?;
        let relevant = checked_ids(&case.id, "relevant", &case.relevant)?;
        let excluded = checked_ids(&case.id, "excluded", &case.excluded)?;
        if let Some(id) = required.intersection(&relevant).next() {
            bail!("{}: {id}をrequiredとrelevantへ重複指定できない", case.id);
        }
        if let Some(id) = required.union(&relevant).find(|id| excluded.contains(*id)) {
            bail!("{}: {id}を適合と除外へ同時指定できない", case.id);
        }
        let mut requirement_ids = BTreeSet::new();
        for requirement in &case.body_requirements {
            if requirement.id.trim().is_empty() || requirement.id != requirement.id.trim() {
                bail!(
                    "{}: body_requirements.idは前後空白なしの非空文字列にする",
                    case.id
                );
            }
            if !requirement_ids.insert(requirement.id.as_str()) {
                bail!(
                    "{}: body_requirements.idが重複している: {}",
                    case.id,
                    requirement.id
                );
            }
            if requirement.all_terms.is_empty() && requirement.any_terms.is_empty() {
                bail!(
                    "{}: {}にはall_termsまたはany_termsが必要",
                    case.id,
                    requirement.id
                );
            }
            checked_terms(&case.id, &requirement.id, &requirement.all_terms)?;
            checked_terms(&case.id, &requirement.id, &requirement.any_terms)?;
        }
        for id in required.iter().chain(&relevant).chain(&excluded) {
            let status = conn
                .query_row("SELECT status FROM notes WHERE id = ?1", [id], |row| {
                    row.get::<_, String>(0)
                })
                .optional()?;
            let Some(status) = status else {
                bail!("{}: 評価対象ノートがDBにない: {id}", case.id);
            };
            if status == "deprecated" {
                bail!("{}: deprecatedノートは評価対象にできない: {id}", case.id);
            }
        }
    }
    Ok(())
}

fn checked_terms(case_id: &str, requirement_id: &str, terms: &[String]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for term in terms {
        if term.trim().is_empty() || term != term.trim() {
            bail!("{case_id}: {requirement_id}のtermは前後空白なしの非空文字列にする");
        }
        if !seen.insert(term) {
            bail!("{case_id}: {requirement_id}のtermが重複している: {term}");
        }
    }
    Ok(())
}

fn checked_ids(case_id: &str, label: &str, ids: &[String]) -> Result<HashSet<String>> {
    let mut seen = HashSet::new();
    for id in ids {
        if id.trim().is_empty() || id != id.trim() {
            bail!("{case_id}: {label}のnote IDは前後空白なしの非空文字列にする");
        }
        if !seen.insert(id.clone()) {
            bail!("{case_id}: {label}のnote IDが重複している: {id}");
        }
    }
    Ok(seen)
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn mean(values: impl Iterator<Item = f64>, count: usize) -> f64 {
    if count == 0 {
        0.0
    } else {
        values.sum::<f64>() / count as f64
    }
}

fn percentile(sorted: &[u64], percent: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = sorted.len().saturating_mul(percent).div_ceil(100).max(1);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn signed_delta(left: u64, right: u64) -> i64 {
    let delta = i128::from(left) - i128::from(right);
    i64::try_from(delta).unwrap_or(if delta.is_negative() {
        i64::MIN
    } else {
        i64::MAX
    })
}

fn micros(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn escape_table(value: &str) -> String {
    value.replace('|', "\\|").replace(['\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE notes(
                 id TEXT PRIMARY KEY, note_uid TEXT, title TEXT, description TEXT, status TEXT,
                 origin TEXT, generated_by TEXT, generated_at TEXT,
                 mtime INTEGER, body TEXT, tags TEXT DEFAULT '', created TEXT,
                 document TEXT NOT NULL DEFAULT '', namespace TEXT,
                 authority_role TEXT, authority_status TEXT, authority_scope TEXT
             );
             CREATE TABLE links(src TEXT, dst TEXT, PRIMARY KEY(src, dst));
             CREATE INDEX links_dst ON links(dst);
             CREATE TABLE note_relations(
                 src_uid TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 target_uid TEXT NOT NULL,
                 PRIMARY KEY(src_uid, kind, target_uid)
             );
             CREATE INDEX note_relations_target ON note_relations(target_uid);
             CREATE TABLE note_vecs(id TEXT PRIMARY KEY, stamp TEXT, embedding BLOB);
             CREATE VIRTUAL TABLE fts_main USING fts5(id UNINDEXED, text, tokenize='unicode61');
             CREATE VIRTUAL TABLE fts_tri USING fts5(id UNINDEXED, text, tokenize='trigram');
             CREATE VIRTUAL TABLE fts_anchor USING fts5(
                 src UNINDEXED, dst UNINDEXED, text, tokenize='unicode61'
             );",
        )
        .unwrap();
        conn
    }

    fn add_note(conn: &Connection, id: &str, searchable: &str) {
        let document = format!("---\ntitle: {id}\n---\n{searchable}");
        conn.execute(
            "INSERT INTO notes(id, title, description, status, origin, body, tags, document)
             VALUES (?1, ?1, '', 'stable', 'agent', ?2, 'kb-app', ?3)",
            rusqlite::params![id, searchable, document],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO fts_main(id, text) VALUES (?1, ?2)",
            rusqlite::params![id, crate::tokenize::wakati(searchable)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO fts_tri(id, text) VALUES (?1, ?2)",
            rusqlite::params![id, searchable],
        )
        .unwrap();
    }

    fn suite() -> GoldenSuite {
        GoldenSuite {
            schema_version: EVALUATION_SCHEMA_VERSION.into(),
            stability_runs: 2,
            cases: vec![GoldenCase {
                id: "one-hop".into(),
                queries: SurfaceQueries {
                    codex: "retrieval beacon".into(),
                    claude_code: "retrieval beacon".into(),
                    chatgpt: "retrieval beacon".into(),
                },
                required: vec!["notes/target".into()],
                relevant: vec!["notes/seed".into()],
                excluded: vec!["notes/wrong".into()],
                body_requirements: vec![BodyRequirement {
                    id: "linked-policy".into(),
                    all_terms: vec!["linked".into()],
                    any_terms: vec!["context".into(), "policy".into()],
                }],
            }],
        }
    }

    #[test]
    fn linked_strategy_recovers_required_note_that_top3_cannot_see() {
        let conn = setup();
        add_note(&conn, "notes/seed", "retrieval beacon");
        add_note(&conn, "notes/target", "linked context");
        add_note(&conn, "notes/wrong", "unrelated");
        conn.execute(
            "INSERT INTO links(src, dst) VALUES ('notes/seed', 'notes/target')",
            [],
        )
        .unwrap();

        let report = evaluate(&conn, &suite()).unwrap();
        let top3 = &report.cases[0].strategies[0];
        let linked = &report.cases[0].strategies[1];
        assert_eq!(top3.strategy, EvaluationStrategy::Top3);
        assert_eq!(top3.selected_recall, 0.0);
        assert_eq!(linked.strategy, EvaluationStrategy::LinkedV1);
        assert_eq!(linked.candidate_recall, 1.0);
        assert_eq!(linked.selected_recall, 1.0);
        assert_eq!(linked.selected_precision, 1.0);
        assert_eq!(linked.runtime.selected_depth_1, 1);
        assert!(linked.gate_passed);
        assert!(report.gate.passed);
        assert!(report.cases[0].stable);
        assert_eq!(report.linked_minus_top3.macro_selected_recall, 1.0);
    }

    #[test]
    fn invalid_or_conflicting_expectations_are_rejected_before_evaluation() {
        let conn = setup();
        add_note(&conn, "notes/seed", "retrieval beacon");
        let mut conflicting_suite = suite();
        conflicting_suite.cases[0].required = vec!["notes/seed".into()];
        conflicting_suite.cases[0].relevant = vec!["notes/seed".into()];
        assert!(evaluate(&conn, &conflicting_suite).is_err());

        let mut missing_suite = suite();
        missing_suite.cases[0].required = vec!["notes/missing".into()];
        missing_suite.cases[0].relevant.clear();
        assert!(evaluate(&conn, &missing_suite).is_err());
    }

    #[test]
    fn linked_gate_fails_when_selected_bodies_lack_required_semantics() {
        let conn = setup();
        add_note(&conn, "notes/seed", "retrieval beacon");
        add_note(&conn, "notes/target", "linked context");
        add_note(&conn, "notes/wrong", "unrelated");
        conn.execute(
            "INSERT INTO links(src, dst) VALUES ('notes/seed', 'notes/target')",
            [],
        )
        .unwrap();
        let mut suite = suite();
        suite.cases[0].body_requirements[0].all_terms = vec!["missing policy".into()];

        let report = evaluate(&conn, &suite).unwrap();
        let linked = &report.cases[0].strategies[1];
        assert_eq!(linked.selected_recall, 1.0);
        assert!(!linked.body_requirements[0].passed);
        assert!(!linked.gate_passed);
        assert!(!report.gate.passed);
        assert_eq!(
            report.gate.failed_cases,
            ["one-hop@codex", "one-hop@claude_code", "one-hop@chatgpt"]
        );
    }

    #[test]
    fn report_serializes_without_note_bodies_and_markdown_shows_the_comparison() {
        let conn = setup();
        add_note(&conn, "notes/seed", "retrieval beacon");
        add_note(&conn, "notes/target", "secret body text");
        add_note(&conn, "notes/wrong", "unrelated");
        conn.execute(
            "INSERT INTO links(src, dst) VALUES ('notes/seed', 'notes/target')",
            [],
        )
        .unwrap();

        let report = evaluate(&conn, &suite()).unwrap();
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("secret body text"));
        assert!(json.contains("linked_v1"));
        let markdown = render_markdown(&report);
        assert!(markdown.contains("linked_v1 − top3"));
        assert!(markdown.contains("notes/target"));
    }

    #[test]
    fn percentile_uses_nearest_rank() {
        assert_eq!(percentile(&[10, 20, 30, 40], 50), 20);
        assert_eq!(percentile(&[10, 20, 30, 40], 95), 40);
        assert_eq!(percentile(&[], 95), 0);
    }

    #[test]
    fn documented_example_follows_the_machine_readable_input_contract() {
        let example = include_str!("../../../schemas/examples/retrieval-eval.example.json");
        let suite: GoldenSuite = serde_json::from_str(example).unwrap();
        assert_eq!(suite.schema_version, EVALUATION_SCHEMA_VERSION);
        assert_eq!(suite.cases.len(), 1);
        assert_eq!(suite.cases[0].queries.iter().len(), 3);
    }
}
