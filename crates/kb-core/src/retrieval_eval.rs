//! リンク連鎖retrievalの再現可能な比較評価。
//!
//! 本番経路は`linked_v1`のまま維持し、旧`top3`はこの評価器の中だけで再現する。
//! 実利用の発話を自動収集せず、本人が用意したGolden Queryだけを端末内で読む。
//! 2.1.0では配信profile × rerankの比較軸(`EvaluationStrategy::Profile`)、routine surface、
//! candidate gate、familyごとの集計、required rank、tokens per requiredを足した。
//! 2.0.0 suiteは従来どおり`top3` / `linked_v1`だけで評価し、出力の構造も変えない。

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension};

use crate::degradation::Degradation;
use crate::retrieval::{
    RetrievalBundle, RetrievalOptions, RetrievalSource, context_documents,
    context_documents_for_query,
};
use crate::retrieval_profile::{PassagePolicy, RerankMode, RetrievalProfile};

pub const EVALUATION_SCHEMA_VERSION: &str = "2.1.0";
/// routine surface・gate_mode・familyを持たない従来形式。構造一致の基準として残す。
pub const LEGACY_EVALUATION_SCHEMA_VERSION: &str = "2.0.0";
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
    /// 同じ仮説を測るcase群の名前。report側でfamily × strategyの集計軸になる(2.1.0)
    #[serde(default)]
    pub family: Option<String>,
    pub queries: SurfaceQueries,
    pub required: Vec<String>,
    #[serde(default)]
    pub relevant: Vec<String>,
    #[serde(default)]
    pub excluded: Vec<String>,
    #[serde(default)]
    pub body_requirements: Vec<BodyRequirement>,
    /// 省略時は`selected`(2.1.0)
    #[serde(default)]
    pub gate_mode: Option<GateMode>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceQueries {
    pub codex: String,
    pub claude_code: String,
    pub chatgpt: String,
    /// 定型routine起動を模した4番目のsurface(2.1.0、任意)
    #[serde(default)]
    pub routine: Option<String>,
}

impl SurfaceQueries {
    fn surfaces(&self) -> Vec<(EvaluationSurface, &str)> {
        let mut surfaces = vec![
            (EvaluationSurface::Codex, self.codex.as_str()),
            (EvaluationSurface::ClaudeCode, self.claude_code.as_str()),
            (EvaluationSurface::Chatgpt, self.chatgpt.as_str()),
        ];
        if let Some(routine) = &self.routine {
            surfaces.push((EvaluationSurface::Routine, routine.as_str()));
        }
        surfaces
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
    Routine,
}

impl EvaluationSurface {
    fn label(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude_code",
            Self::Chatgpt => "chatgpt",
            Self::Routine => "routine",
        }
    }
}

/// `selected`はrequired全件の本文選択、`candidates`はrequired全件の候補化で合格にする。
/// `candidates`では本文要件を報告だけに留める(本文0件のroutine変種を測るため)。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateMode {
    #[default]
    Selected,
    Candidates,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvaluationStrategy {
    Top3,
    LinkedV1,
    /// 配信profile × rerankの比較軸。baseでは全profileが`linked_v1`と同じ挙動
    Profile {
        profile: RetrievalProfile,
        rerank: RerankMode,
    },
}

impl EvaluationStrategy {
    pub fn label(self) -> String {
        match self {
            Self::Top3 => "top3".into(),
            Self::LinkedV1 => "linked_v1".into(),
            Self::Profile { profile, rerank } => {
                format!("profile:{}:rerank_{}", profile.label(), rerank.label())
            }
        }
    }
}

// JSONではstrategyを文字列labelのまま出し、2.0.0時代の`"linked_v1"`と同じ形を保つ。
impl serde::Serialize for EvaluationStrategy {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.label())
    }
}

/// 1 suiteで比較する戦略の並び。先頭2つは常に`top3` / `linked_v1`で、差分と構造一致の基準にする。
#[derive(Clone, Debug)]
pub struct EvaluationPlan {
    strategies: Vec<EvaluationStrategy>,
}

impl EvaluationPlan {
    pub fn classic() -> Self {
        Self {
            strategies: vec![EvaluationStrategy::Top3, EvaluationStrategy::LinkedV1],
        }
    }

    pub fn with_profiles(profiles: &[RetrievalProfile], rerank: RerankMode) -> Self {
        let mut plan = Self::classic();
        for profile in profiles {
            let strategy = EvaluationStrategy::Profile {
                profile: *profile,
                rerank,
            };
            if !plan.strategies.contains(&strategy) {
                plan.strategies.push(strategy);
            }
        }
        plan
    }

    pub fn strategies(&self) -> &[EvaluationStrategy] {
        &self.strategies
    }

    /// 統合coreのrerank軸は`off`のみ実行できる。`off`以外(現在の`on`、評価用ブランチが足す
    /// `indexed`/`auto`も同様)はContextCard実装を持つ評価用ブランチ専用の軸で、coreでは
    /// 明確なエラーにする(R4 I-5)。`RerankMode`のenum自体は比較表の語彙として残す。
    pub(crate) fn ensure_rerank_off_for_core(&self) -> Result<()> {
        for strategy in &self.strategies {
            if let EvaluationStrategy::Profile { rerank, .. } = strategy
                && *rerank != RerankMode::Off
            {
                bail!(
                    "rerank={}は統合coreでは実行できない(coreが受理するのはoffのみ)。\
                     ContextCard rerankは評価用ブランチの内部modeでだけ有効化する",
                    rerank.label()
                );
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct EvaluationReport {
    pub schema_version: &'static str,
    pub suite_schema_version: String,
    pub core_version: &'static str,
    pub case_count: usize,
    pub search_configuration: SearchConfiguration,
    pub strategy_configurations: Vec<StrategyConfiguration>,
    pub summaries: Vec<StrategySummary>,
    pub families: Vec<FamilySummary>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<RetrievalProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerank: Option<RerankMode>,
    pub seed_limit: usize,
    pub max_depth: u8,
    pub candidate_limit: usize,
    pub document_limit: usize,
    /// `None`は評価専用top3 baselineの本文量予算なしを表す。
    pub estimated_token_budget: Option<usize>,
    pub include_incoming: bool,
    /// `None`はquery非依存の全文選択(top3)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passage: Option<PassageConfiguration>,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct PassageConfiguration {
    pub trigger_tokens: usize,
    pub document_limit: usize,
    pub document_token_budget: usize,
}

// reportへはpassage予算のうち構造へ影響する3値だけを出す(2.1.0の形を保つ)。
// `max_bytes`は分割粒度の実装詳細としてprofile側(`PassagePolicy`)に留める。
impl From<PassagePolicy> for PassageConfiguration {
    fn from(policy: PassagePolicy) -> Self {
        Self {
            trigger_tokens: policy.trigger_tokens,
            document_limit: policy.document_limit,
            document_token_budget: policy.document_token_budget,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct StrategySummary {
    pub strategy: EvaluationStrategy,
    pub cases: usize,
    pub gate_passed: bool,
    pub gate_failed_cases: Vec<String>,
    pub macro_candidate_recall: f64,
    pub macro_selected_recall: f64,
    pub macro_selected_precision: f64,
    pub excluded_violations: usize,
    pub average_selected_documents: f64,
    pub average_estimated_tokens: f64,
    /// requiredを1件以上選択したsurfaceだけの平均。`None`は該当surfaceなし
    pub average_tokens_per_required: Option<f64>,
    pub tokens_per_required_cases: usize,
    /// 候補順で最初のrequiredが現れる位置(1始まり)の中央値。予算に依存しない順位指標
    pub median_required_rank: Option<u64>,
    pub required_rank_missing_cases: usize,
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
pub struct FamilySummary {
    pub family: String,
    pub surfaces: usize,
    pub summaries: Vec<StrategySummary>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    pub surface: EvaluationSurface,
    pub query: String,
    pub gate_mode: GateMode,
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
    /// 候補順で最初のrequiredが現れる位置(1始まり)。候補に無ければ`None`
    pub required_rank: Option<usize>,
    /// 選択本文の推定token ÷ 選択されたrequired件数。requiredを1件も選べなければ`None`
    pub tokens_per_required: Option<f64>,
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

/// sync済みDBを1つのread transactionで固定し、各queryを`top3` / `linked_v1`で比較する。
pub fn evaluate(conn: &Connection, suite: &GoldenSuite) -> Result<EvaluationReport> {
    evaluate_with(conn, suite, &EvaluationPlan::classic())
}

/// 同じsnapshotで、planに並んだ全戦略を各case × surfaceへ適用する。
pub fn evaluate_with(
    conn: &Connection,
    suite: &GoldenSuite,
    plan: &EvaluationPlan,
) -> Result<EvaluationReport> {
    plan.ensure_rerank_off_for_core()?;
    let transaction = conn
        .unchecked_transaction()
        .context("retrieval評価用snapshotを開始できない")?;
    validate_suite(&transaction, suite)?;

    let mut cases = Vec::new();
    for case in &suite.cases {
        for (surface, query) in case.queries.surfaces() {
            let mut report = evaluate_case(&transaction, case, surface, query, plan)?;
            for _ in 1..suite.stability_runs {
                let repeated = evaluate_case(&transaction, case, surface, query, plan)?;
                if !same_retrieval_result(&report, &repeated) {
                    report.stable = false;
                }
            }
            cases.push(report);
        }
    }

    // read-only transactionを明示終了し、呼び出し側が同じConnectionを続けて使えるようにする。
    transaction.rollback()?;

    let all = cases.iter().collect::<Vec<_>>();
    let summaries = plan
        .strategies()
        .iter()
        .map(|strategy| summarize(&all, *strategy))
        .collect::<Vec<_>>();
    let top3 = summaries
        .iter()
        .find(|summary| summary.strategy == EvaluationStrategy::Top3)
        .expect("planは常にtop3を含む");
    let linked = summaries
        .iter()
        .find(|summary| summary.strategy == EvaluationStrategy::LinkedV1)
        .expect("planは常にlinked_v1を含む");
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
    let gate = GateReport {
        strategy: EvaluationStrategy::LinkedV1,
        passed: linked.gate_passed,
        failed_cases: linked.gate_failed_cases.clone(),
    };
    let families = summarize_families(&cases, plan);

    // 評価は`evaluation` profile(= 本番hookの`session_auto`)の検索方針で引く。値は
    // profile分離前の`AUTO_SEED_LIMIT` + OR結合と同一で、profile側のtestが一致を固定する。
    let search_policy = RetrievalProfile::Evaluation.plan().search;
    Ok(EvaluationReport {
        schema_version: EVALUATION_SCHEMA_VERSION,
        suite_schema_version: suite.schema_version.clone(),
        core_version: crate::CORE_VERSION,
        case_count: cases.len(),
        search_configuration: SearchConfiguration {
            ranked_hit_limit: search_policy.limit,
            any_terms: search_policy.any_terms,
        },
        strategy_configurations: plan
            .strategies()
            .iter()
            .map(|strategy| configuration_for(*strategy))
            .collect(),
        summaries,
        families,
        linked_minus_top3: delta,
        gate,
        cases,
    })
}

fn evaluate_case(
    conn: &Connection,
    case: &GoldenCase,
    surface: EvaluationSurface,
    query: &str,
    plan: &EvaluationPlan,
) -> Result<CaseReport> {
    // 評価の検索は `evaluation` profile(= 本番 hook の `session_auto`)で引く。ここを変えると
    // 評価値が本番の挙動を指さなくなるので、profile 側の test が一致を固定している。
    let search_policy = RetrievalProfile::Evaluation.plan().search;
    let search_started = Instant::now();
    let outcome = crate::search::search_with(conn, query, &search_policy);
    let search_elapsed_us = micros(search_started.elapsed());
    let ranked_hit_ids = outcome
        .hits
        .iter()
        .map(|hit| hit.id.clone())
        .collect::<Vec<_>>();

    let mut strategies = Vec::with_capacity(plan.strategies().len());
    for strategy in plan.strategies() {
        let options = retrieval_options_for(*strategy);
        let bundle = match strategy {
            EvaluationStrategy::Top3 => context_documents(conn, &ranked_hit_ids, options)?,
            EvaluationStrategy::LinkedV1 | EvaluationStrategy::Profile { .. } => {
                context_documents_for_query(conn, &ranked_hit_ids, query, options)?
            }
        };
        strategies.push(score_case(case, *strategy, search_elapsed_us, bundle));
    }

    Ok(CaseReport {
        id: case.id.clone(),
        family: case.family.clone(),
        surface,
        query: query.to_string(),
        gate_mode: case.gate_mode.unwrap_or_default(),
        required: case.required.clone(),
        relevant: case.relevant.clone(),
        excluded: case.excluded.clone(),
        search_degraded: outcome.degraded,
        stable: true,
        strategies,
    })
}

fn retrieval_options_for(strategy: EvaluationStrategy) -> RetrievalOptions {
    match strategy {
        // 評価専用 baseline。query を渡さないので passage 予算は使われない。
        EvaluationStrategy::Top3 => RetrievalOptions {
            seed_limit: 3,
            max_depth: 0,
            candidate_limit: 3,
            document_limit: 3,
            estimated_token_budget: usize::MAX,
            include_incoming: false,
            passage: PassagePolicy::default(),
        },
        EvaluationStrategy::LinkedV1 => RetrievalProfile::Evaluation.plan().retrieval,
        // 配信profileの実体(retrieval_profile.rs)をそのまま比較軸にする。rerank軸は
        // coreではoff固定(`ensure_rerank_off_for_core`)で、optionsへは影響しない。
        EvaluationStrategy::Profile { profile, .. } => profile.plan().retrieval,
    }
}

fn configuration_for(strategy: EvaluationStrategy) -> StrategyConfiguration {
    let options = retrieval_options_for(strategy);
    let (profile, rerank) = match strategy {
        EvaluationStrategy::Profile { profile, rerank } => (Some(profile), Some(rerank)),
        _ => (None, None),
    };
    StrategyConfiguration {
        strategy,
        profile,
        rerank,
        seed_limit: options.seed_limit,
        max_depth: options.max_depth,
        candidate_limit: options.candidate_limit,
        document_limit: options.document_limit,
        estimated_token_budget: (options.estimated_token_budget != usize::MAX)
            .then_some(options.estimated_token_budget),
        include_incoming: options.include_incoming,
        passage: match strategy {
            EvaluationStrategy::Top3 => None,
            _ => Some(PassageConfiguration::from(options.passage)),
        },
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
    let required_set = case
        .required
        .iter()
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
    let required_rank = candidate_ids
        .iter()
        .position(|id| required_set.contains(id.as_str()))
        .map(|position| position + 1);

    let body_requirements = case
        .body_requirements
        .iter()
        .map(|requirement| score_body_requirement(requirement, &bundle))
        .collect::<Vec<_>>();
    let gate_mode = case.gate_mode.unwrap_or_default();
    let recall_gate = match gate_mode {
        GateMode::Selected => required_in_selected.len() == case.required.len(),
        GateMode::Candidates => required_in_candidates.len() == case.required.len(),
    };
    let body_gate = gate_mode == GateMode::Candidates
        || body_requirements
            .iter()
            .all(|requirement| requirement.passed);
    let gate_passed = recall_gate
        && excluded_in_selected.is_empty()
        && body_gate
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
    let tokens_per_required = (!required_in_selected.is_empty())
        .then(|| estimated_tokens as f64 / required_in_selected.len() as f64);

    CaseStrategyReport {
        strategy,
        candidate_ids,
        selected,
        candidate_recall: ratio(required_in_candidates.len(), case.required.len()),
        selected_recall: ratio(required_in_selected.len(), case.required.len()),
        selected_precision: ratio(relevant_selected, selected_count),
        required_rank,
        tokens_per_required,
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

/// 不安定・検索劣化・戦略gate不合格のいずれかで、そのsurfaceは当該戦略のgateを落とす。
fn strategy_failed(case: &CaseReport, result: &CaseStrategyReport) -> bool {
    !case.stable || !case.search_degraded.is_empty() || !result.gate_passed
}

const fn default_stability_runs() -> usize {
    1
}

fn summarize(cases: &[&CaseReport], strategy: EvaluationStrategy) -> StrategySummary {
    let results = cases
        .iter()
        .filter_map(|case| {
            case.strategies
                .iter()
                .find(|result| result.strategy == strategy)
                .map(|result| (*case, result))
        })
        .collect::<Vec<_>>();
    let count = results.len();
    let mut elapsed = results
        .iter()
        .map(|(_, result)| result.runtime.total_elapsed_us)
        .collect::<Vec<_>>();
    elapsed.sort_unstable();
    let gate_failed_cases = results
        .iter()
        .filter(|(case, result)| strategy_failed(case, result))
        .map(|(case, _)| case_key(case))
        .collect::<Vec<_>>();
    let tokens_per_required = results
        .iter()
        .filter_map(|(_, result)| result.tokens_per_required)
        .collect::<Vec<_>>();
    let mut required_ranks = results
        .iter()
        .filter_map(|(_, result)| result.required_rank)
        .map(|rank| rank as u64)
        .collect::<Vec<_>>();
    required_ranks.sort_unstable();

    StrategySummary {
        strategy,
        cases: count,
        gate_passed: gate_failed_cases.is_empty(),
        gate_failed_cases,
        macro_candidate_recall: mean(
            results.iter().map(|(_, result)| result.candidate_recall),
            count,
        ),
        macro_selected_recall: mean(
            results.iter().map(|(_, result)| result.selected_recall),
            count,
        ),
        macro_selected_precision: mean(
            results.iter().map(|(_, result)| result.selected_precision),
            count,
        ),
        excluded_violations: results
            .iter()
            .map(|(_, result)| result.excluded_in_selected.len())
            .sum(),
        average_selected_documents: mean(
            results
                .iter()
                .map(|(_, result)| result.runtime.selected_count as f64),
            count,
        ),
        average_estimated_tokens: mean(
            results
                .iter()
                .map(|(_, result)| result.runtime.estimated_tokens as f64),
            count,
        ),
        average_tokens_per_required: (!tokens_per_required.is_empty()).then(|| {
            mean(
                tokens_per_required.iter().copied(),
                tokens_per_required.len(),
            )
        }),
        tokens_per_required_cases: tokens_per_required.len(),
        median_required_rank: (!required_ranks.is_empty()).then(|| percentile(&required_ranks, 50)),
        required_rank_missing_cases: count.saturating_sub(required_ranks.len()),
        spill_cases: results
            .iter()
            .filter(|(_, result)| result.runtime.spill)
            .count(),
        candidate_cap_cases: results
            .iter()
            .filter(|(_, result)| result.runtime.candidate_cap_reached)
            .count(),
        document_cap_cases: results
            .iter()
            .filter(|(_, result)| result.runtime.document_cap_reached)
            .count(),
        budget_exhausted_cases: results
            .iter()
            .filter(|(_, result)| result.runtime.budget_exhausted)
            .count(),
        selected_depth_0: results
            .iter()
            .map(|(_, result)| result.runtime.selected_depth_0)
            .sum(),
        selected_depth_1: results
            .iter()
            .map(|(_, result)| result.runtime.selected_depth_1)
            .sum(),
        selected_depth_2: results
            .iter()
            .map(|(_, result)| result.runtime.selected_depth_2)
            .sum(),
        selected_incoming: results
            .iter()
            .map(|(_, result)| result.runtime.selected_incoming)
            .sum(),
        p50_elapsed_us: percentile(&elapsed, 50),
        p95_elapsed_us: percentile(&elapsed, 95),
    }
}

/// familyを持つcaseだけをfixtureの出現順で束ね、戦略ごとに同じ集計を出す。
fn summarize_families(cases: &[CaseReport], plan: &EvaluationPlan) -> Vec<FamilySummary> {
    let mut order = Vec::new();
    let mut grouped: BTreeMap<&str, Vec<&CaseReport>> = BTreeMap::new();
    for case in cases {
        let Some(family) = case.family.as_deref() else {
            continue;
        };
        if !grouped.contains_key(family) {
            order.push(family);
        }
        grouped.entry(family).or_default().push(case);
    }
    order
        .into_iter()
        .map(|family| {
            let members = &grouped[family];
            FamilySummary {
                family: family.to_string(),
                surfaces: members.len(),
                summaries: plan
                    .strategies()
                    .iter()
                    .map(|strategy| summarize(members, *strategy))
                    .collect(),
            }
        })
        .collect()
}

/// 人が差分を読める最小レポート。完全な候補列や劣化detailはJSON出力に残す。
pub fn render_markdown(report: &EvaluationReport) -> String {
    let mut out = String::from("# Retrieval evaluation\n\n");
    out.push_str(&format!(
        "- schema: `{}` (suite `{}`)\n- core: `{}`\n- cases: {}\n- gate: **{}** (`linked_v1`)\n- shared search: top {}, any_terms={}\n\n",
        report.schema_version,
        report.suite_schema_version,
        report.core_version,
        report.case_count,
        if report.gate.passed { "PASS" } else { "FAIL" },
        report.search_configuration.ranked_hit_limit,
        report.search_configuration.any_terms,
    ));
    out.push_str(
        "| strategy | profile | rerank | seed | depth | candidates | documents | token budget | incoming | passage (trigger / docs / tokens) |\n",
    );
    out.push_str("| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | --- | --- |\n");
    for configuration in &report.strategy_configurations {
        let token_budget = configuration
            .estimated_token_budget
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".into());
        let passage = configuration
            .passage
            .map(|passage| {
                format!(
                    "{} / {} / {}",
                    passage.trigger_tokens, passage.document_limit, passage.document_token_budget
                )
            })
            .unwrap_or_else(|| "none".into());
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            configuration.strategy.label(),
            configuration
                .profile
                .map(RetrievalProfile::label)
                .unwrap_or("-"),
            configuration.rerank.map(RerankMode::label).unwrap_or("-"),
            configuration.seed_limit,
            configuration.max_depth,
            configuration.candidate_limit,
            configuration.document_limit,
            token_budget,
            configuration.include_incoming,
            passage,
        ));
    }
    out.push('\n');
    out.push_str(&summary_table(&report.summaries));
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
    if !report.families.is_empty() {
        out.push_str("\n## Families\n\n");
        out.push_str("| family | surfaces | strategy | gate | failed | candidate recall | selected recall | precision | excluded | avg docs | avg tokens | tokens/required | required rank p50 |\n");
        out.push_str(
            "| --- | ---: | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n",
        );
        for family in &report.families {
            for summary in &family.summaries {
                out.push_str(&format!(
                    "| {} | {} | {} | {} | {} | {:.1}% | {:.1}% | {:.1}% | {} | {:.2} | {:.0} | {} | {} |\n",
                    escape_table(&family.family),
                    family.surfaces,
                    summary.strategy.label(),
                    if summary.gate_passed { "PASS" } else { "FAIL" },
                    summary.gate_failed_cases.len(),
                    summary.macro_candidate_recall * 100.0,
                    summary.macro_selected_recall * 100.0,
                    summary.macro_selected_precision * 100.0,
                    summary.excluded_violations,
                    summary.average_selected_documents,
                    summary.average_estimated_tokens,
                    optional_number(summary.average_tokens_per_required),
                    summary
                        .median_required_rank
                        .map(|rank| rank.to_string())
                        .unwrap_or_else(|| "-".into()),
                ));
            }
        }
    }
    out.push_str("\n## Cases\n\n");
    out.push_str("| case | surface | strategy | gate | stable | candidate recall | selected recall | precision | required rank | selected IDs | token | missing | total μs |\n");
    out.push_str(
        "| --- | --- | --- | --- | --- | ---: | ---: | ---: | ---: | --- | ---: | ---: | ---: |\n",
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
                "| {} | {} | {} | {} | {} | {:.1}% | {:.1}% | {:.1}% | {} | {} | {} | {} | {} |\n",
                escape_table(&case.id),
                case.surface.label(),
                result.strategy.label(),
                if result.gate_passed { "PASS" } else { "FAIL" },
                case.stable,
                result.candidate_recall * 100.0,
                result.selected_recall * 100.0,
                result.selected_precision * 100.0,
                result
                    .required_rank
                    .map(|rank| rank.to_string())
                    .unwrap_or_else(|| "-".into()),
                escape_table(&selected),
                result.runtime.estimated_tokens,
                result.runtime.missing_documents,
                result.runtime.total_elapsed_us,
            ));
        }
    }
    out
}

fn summary_table(summaries: &[StrategySummary]) -> String {
    let mut out = String::new();
    out.push_str("| strategy | gate | failed | candidate recall | selected recall | precision | excluded | avg docs | avg tokens | tokens/required | required rank p50 | spill | p50 μs | p95 μs |\n");
    out.push_str(
        "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n",
    );
    for summary in summaries {
        out.push_str(&format!(
            "| {} | {} | {} | {:.1}% | {:.1}% | {:.1}% | {} | {:.2} | {:.0} | {} | {} | {} | {} | {} |\n",
            summary.strategy.label(),
            if summary.gate_passed { "PASS" } else { "FAIL" },
            summary.gate_failed_cases.len(),
            summary.macro_candidate_recall * 100.0,
            summary.macro_selected_recall * 100.0,
            summary.macro_selected_precision * 100.0,
            summary.excluded_violations,
            summary.average_selected_documents,
            summary.average_estimated_tokens,
            optional_number(summary.average_tokens_per_required),
            summary
                .median_required_rank
                .map(|rank| rank.to_string())
                .unwrap_or_else(|| "-".into()),
            summary.spill_cases,
            summary.p50_elapsed_us,
            summary.p95_elapsed_us,
        ));
    }
    out
}

fn optional_number(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.0}"))
        .unwrap_or_else(|| "-".into())
}

fn validate_suite(conn: &Connection, suite: &GoldenSuite) -> Result<()> {
    let legacy = suite.schema_version == LEGACY_EVALUATION_SCHEMA_VERSION;
    if !legacy && suite.schema_version != EVALUATION_SCHEMA_VERSION {
        bail!(
            "Golden Query schema_versionは{}または{}である必要がある: {}",
            LEGACY_EVALUATION_SCHEMA_VERSION,
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
        if legacy
            && (case.family.is_some() || case.gate_mode.is_some() || case.queries.routine.is_some())
        {
            bail!(
                "{}: family / gate_mode / queries.routineはschema_version {}でだけ使える",
                case.id,
                EVALUATION_SCHEMA_VERSION
            );
        }
        if let Some(family) = &case.family
            && (family.trim().is_empty() || family != family.trim())
        {
            bail!("{}: familyは前後空白なしの非空文字列にする", case.id);
        }
        for (surface, query) in case.queries.surfaces() {
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
                 authority_role TEXT, authority_status TEXT, authority_scope TEXT,
                 normal_reference_allowed INTEGER NOT NULL DEFAULT 0
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
             );
             CREATE TABLE note_events(
                 event_id TEXT PRIMARY KEY, note_uid TEXT, note_id TEXT NOT NULL,
                 at TEXT NOT NULL, operation TEXT NOT NULL, actor_client TEXT NOT NULL,
                 actor_client_version TEXT, actor_surface TEXT NOT NULL, actor_model TEXT,
                 actor_model_basis TEXT NOT NULL, kind TEXT NOT NULL, summary TEXT,
                 payload TEXT NOT NULL, exported INTEGER NOT NULL DEFAULT 0
             );",
        )
        .unwrap();
        conn
    }

    fn add_note(conn: &Connection, id: &str, searchable: &str) {
        let document = format!("---\ntitle: {id}\n---\n{searchable}");
        conn.execute(
            "INSERT INTO notes(id, title, description, status, origin, body, tags, document,
                               normal_reference_allowed)
             VALUES (?1, ?1, '', 'stable', 'agent', ?2, 'kb-app', ?3, 1)",
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

    fn one_hop_case() -> GoldenCase {
        GoldenCase {
            id: "one-hop".into(),
            family: None,
            queries: SurfaceQueries {
                codex: "retrieval beacon".into(),
                claude_code: "retrieval beacon".into(),
                chatgpt: "retrieval beacon".into(),
                routine: None,
            },
            required: vec!["notes/target".into()],
            relevant: vec!["notes/seed".into()],
            excluded: vec!["notes/wrong".into()],
            body_requirements: vec![BodyRequirement {
                id: "linked-policy".into(),
                all_terms: vec!["linked".into()],
                any_terms: vec!["context".into(), "policy".into()],
            }],
            gate_mode: None,
        }
    }

    fn suite() -> GoldenSuite {
        GoldenSuite {
            schema_version: LEGACY_EVALUATION_SCHEMA_VERSION.into(),
            stability_runs: 2,
            cases: vec![one_hop_case()],
        }
    }

    fn one_hop_corpus() -> Connection {
        let conn = setup();
        add_note(&conn, "notes/seed", "retrieval beacon");
        add_note(&conn, "notes/target", "linked context");
        add_note(&conn, "notes/wrong", "unrelated");
        conn.execute(
            "INSERT INTO links(src, dst) VALUES ('notes/seed', 'notes/target')",
            [],
        )
        .unwrap();
        conn
    }

    #[test]
    fn linked_strategy_recovers_required_note_that_top3_cannot_see() {
        let conn = one_hop_corpus();

        let report = evaluate(&conn, &suite()).unwrap();
        let top3 = &report.cases[0].strategies[0];
        let linked = &report.cases[0].strategies[1];
        assert_eq!(top3.strategy, EvaluationStrategy::Top3);
        assert_eq!(top3.selected_recall, 0.0);
        assert_eq!(top3.required_rank, None);
        assert_eq!(top3.tokens_per_required, None);
        assert_eq!(linked.strategy, EvaluationStrategy::LinkedV1);
        assert_eq!(linked.candidate_recall, 1.0);
        assert_eq!(linked.selected_recall, 1.0);
        assert_eq!(linked.selected_precision, 1.0);
        assert_eq!(linked.runtime.selected_depth_1, 1);
        assert_eq!(linked.required_rank, Some(2));
        assert_eq!(
            linked.tokens_per_required,
            Some(linked.runtime.estimated_tokens as f64)
        );
        assert!(linked.gate_passed);
        assert!(report.gate.passed);
        assert!(report.cases[0].stable);
        assert_eq!(report.linked_minus_top3.macro_selected_recall, 1.0);
        assert_eq!(report.summaries[1].median_required_rank, Some(2));
        assert_eq!(report.summaries[0].required_rank_missing_cases, 3);
        assert!(report.families.is_empty());
        assert_eq!(
            report.suite_schema_version,
            LEGACY_EVALUATION_SCHEMA_VERSION
        );
    }

    /// 2026-09-08: #93の完了質問・過去の分類結果が方針ノートに埋もれる構造を、
    /// 実KBの発話や本文を含まない2題材で再現する。検索1位と本文先頭を別々に検査し、
    /// 「結果」を含む現行手順の照会を実施記録へ取り違えないことも固定する。
    #[test]
    fn completion_and_result_evidence_reaches_early_bodies_on_all_surfaces() {
        let conn = setup();
        let add = |id: &str, title: &str, description: &str, body: &str, role: &str| {
            add_note(&conn, id, &format!("{title}\n{description}\n{body}"));
            conn.execute(
                "UPDATE notes SET title=?2, description=?3, body=?4, document=?5,
                    namespace=?6, authority_role=?7, authority_status='active', authority_scope=?1
                 WHERE id=?1",
                rusqlite::params![
                    id,
                    title,
                    description,
                    body,
                    format!("---\ntitle: {title}\n---\n{body}"),
                    if role == "record" {
                        "records"
                    } else {
                        "procedures"
                    },
                    role,
                ],
            )
            .unwrap();
        };
        add(
            "notes/lumen-result",
            "Lumen 移行の実施結果",
            "Lumen 移行を実施し、復元検証が完了した結果を残す。",
            "Lumen 移行は完了した。三つの対象すべてで復元一致を確認した。",
            "record",
        );
        add(
            "notes/vega-result",
            "Vega 調査の分類結果",
            "Vega 調査で3項目を分類した結果を残す。",
            "Vega 調査では3項目を重要、参考、対象外に分類した。再調査は不要と判定した。",
            "record",
        );
        add(
            "notes/vega-classification-policy",
            "Vega 調査の分類基準",
            "Vega 調査結果を分類するための現行方針と判断手順。",
            "Vega 調査結果の分類方法は機密性と保存期間を判断軸にする。個別の分類結果は別の実施記録に残す。",
            "canonical",
        );
        for (topic, subject) in [("lumen", "移行"), ("vega", "調査")] {
            for (index, facet) in ["受付", "対象選定", "実施", "照合"].iter().enumerate()
            {
                add(
                    &format!("notes/{topic}-policy-{index}"),
                    &format!("{topic} {subject}の{facet}手順"),
                    &format!("{topic} {subject}の現行方針。{facet}を担当する。"),
                    &format!(
                        "{topic} {subject}の{facet}手順。完了条件と3項目の分類方法を定める。\n\
                         この文書は運用規則であり、実施結果を記録するものではない。"
                    ),
                    "canonical",
                );
            }
        }
        add_note(&conn, "notes/unrelated", "別の天体に関する観測資料");

        let make_case = |id: &str, query: &str, required: &str, evidence: &str| GoldenCase {
            id: id.into(),
            family: Some("completion-results".into()),
            queries: SurfaceQueries {
                codex: query.into(),
                claude_code: query.into(),
                chatgpt: query.into(),
                routine: None,
            },
            required: vec![required.into()],
            relevant: Vec::new(),
            excluded: vec!["notes/unrelated".into()],
            body_requirements: vec![BodyRequirement {
                id: "answer-evidence".into(),
                all_terms: vec![evidence.into()],
                any_terms: Vec::new(),
            }],
            gate_mode: None,
        };
        let suite = GoldenSuite {
            schema_version: EVALUATION_SCHEMA_VERSION.into(),
            stability_runs: 3,
            cases: vec![
                make_case(
                    "completion-question",
                    "lumen移行は完了した？",
                    "notes/lumen-result",
                    "復元一致を確認した",
                ),
                make_case(
                    "classification-result",
                    "vega調査で3項目をどう分類したか",
                    "notes/vega-result",
                    "重要、参考、対象外に分類した",
                ),
                make_case(
                    "current-policy-control",
                    "lumen移行の現在の受付方針は？",
                    "notes/lumen-policy-0",
                    "受付手順",
                ),
                make_case(
                    "result-word-in-current-procedure-control",
                    "現在のVega調査結果の分類方法",
                    "notes/vega-classification-policy",
                    "機密性と保存期間を判断軸にする",
                ),
            ],
        };
        let report = evaluate(&conn, &suite).unwrap();
        assert_eq!(report.cases.len(), suite.cases.len() * 3);
        assert!(report.gate.passed, "{:?}", report.gate.failed_cases);
        for case in &report.cases {
            let required = &case.required[0];
            let search = crate::search::search_with(
                &conn,
                &case.query,
                &RetrievalProfile::Evaluation.plan().search,
            );
            assert!(search.degraded.is_empty(), "{}", case_key(case));
            assert_eq!(
                search.hits.first().map(|hit| &hit.id),
                Some(required),
                "{}: 期待する根拠が検索1位ではない: {:?}",
                case_key(case),
                search.hits.iter().map(|hit| &hit.id).collect::<Vec<_>>()
            );
            assert!(case.stable, "{}", case_key(case));
            assert!(case.search_degraded.is_empty(), "{}", case_key(case));
            for result in &case.strategies {
                let options = retrieval_options_for(result.strategy);
                assert!(result.gate_passed, "{}", case_key(case));
                assert_eq!(
                    result.selected.first().map(|doc| &doc.id),
                    Some(required),
                    "{}: {}で期待する根拠が本文先頭ではない: {:?}",
                    case_key(case),
                    result.strategy.label(),
                    result
                        .selected
                        .iter()
                        .map(|doc| &doc.id)
                        .collect::<Vec<_>>()
                );
                assert_eq!(result.runtime.missing_documents, 0);
                assert!(!result.runtime.budget_exhausted);
                assert!(!result.runtime.spill);
                assert!(result.runtime.estimated_tokens <= options.estimated_token_budget);
                assert!(result.runtime.seed_count <= options.seed_limit);
                assert!(result.runtime.selected_count <= options.document_limit);
                assert!(result.runtime.candidate_count <= options.candidate_limit);
            }
        }
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

    /// 2.0.0 fixtureは従来形式のまま固定する。新しい軸が紛れ込むと、構造一致の基準が動く。
    #[test]
    fn legacy_suite_rejects_routine_gate_mode_and_family() {
        let conn = one_hop_corpus();
        let mut with_family = suite();
        with_family.cases[0].family = Some("one-hop".into());
        assert!(evaluate(&conn, &with_family).is_err());

        let mut with_gate_mode = suite();
        with_gate_mode.cases[0].gate_mode = Some(GateMode::Selected);
        assert!(evaluate(&conn, &with_gate_mode).is_err());

        let mut with_routine = suite();
        with_routine.cases[0].queries.routine = Some("retrieval beacon".into());
        assert!(evaluate(&conn, &with_routine).is_err());

        let mut unknown_version = suite();
        unknown_version.schema_version = "2.2.0".into();
        assert!(evaluate(&conn, &unknown_version).is_err());
    }

    #[test]
    fn routine_surface_is_evaluated_only_when_present() {
        let conn = one_hop_corpus();
        let mut suite = suite();
        suite.schema_version = EVALUATION_SCHEMA_VERSION.into();
        suite.cases[0].family = Some("one-hop".into());
        suite.cases[0].queries.routine = Some("定例確認: retrieval beacon を確認する".into());

        let report = evaluate(&conn, &suite).unwrap();
        assert_eq!(report.case_count, 4);
        assert_eq!(report.cases[3].surface, EvaluationSurface::Routine);
        assert_eq!(report.families.len(), 1);
        assert_eq!(report.families[0].family, "one-hop");
        assert_eq!(report.families[0].surfaces, 4);
        assert_eq!(report.families[0].summaries.len(), 2);
        assert!(report.gate.passed);
        assert_eq!(report.gate.failed_cases, Vec::<String>::new());
        assert!(render_markdown(&report).contains("| one-hop | 4 | linked_v1 | PASS |"));
    }

    /// candidate gateは「候補には入るが本文予算で選ばれない」requiredを合格にする。
    /// routine変種(本文0件)の候補recallを測るための区別で、selected gateとは別に数える。
    #[test]
    fn candidates_gate_mode_passes_when_required_is_only_a_candidate() {
        let conn = setup();
        add_note(&conn, "notes/seed", "retrieval beacon");
        for index in 1..=11 {
            let id = format!("notes/t{index:02}");
            add_note(&conn, &id, "linked context");
            conn.execute(
                "INSERT INTO links(src, dst) VALUES ('notes/seed', ?1)",
                [&id],
            )
            .unwrap();
        }
        let mut case = one_hop_case();
        case.required = vec!["notes/t11".into()];
        case.relevant = vec!["notes/seed".into()];
        case.excluded.clear();
        case.body_requirements.clear();
        let mut suite = GoldenSuite {
            schema_version: EVALUATION_SCHEMA_VERSION.into(),
            stability_runs: 1,
            cases: vec![case],
        };

        let selected_mode = evaluate(&conn, &suite).unwrap();
        let linked = &selected_mode.cases[0].strategies[1];
        assert_eq!(linked.candidate_recall, 1.0);
        assert_eq!(linked.selected_recall, 0.0);
        assert_eq!(linked.required_rank, Some(12));
        assert!(!linked.gate_passed);
        assert!(!selected_mode.gate.passed);

        suite.cases[0].gate_mode = Some(GateMode::Candidates);
        let candidates_mode = evaluate(&conn, &suite).unwrap();
        let linked = &candidates_mode.cases[0].strategies[1];
        assert_eq!(candidates_mode.cases[0].gate_mode, GateMode::Candidates);
        assert!(linked.gate_passed);
        assert!(candidates_mode.gate.passed);
        assert!(!candidates_mode.cases[0].strategies[0].gate_passed);
    }

    /// 実験base: `session_auto`は`linked_v1`と候補列・選択・gateが完全に一致する。
    /// 他のprofileもbaseでは同じ挙動だが、比較軸(label・configuration)はreportに残る。
    #[test]
    fn profile_plan_keeps_session_auto_identical_to_linked_v1() {
        let conn = one_hop_corpus();
        let plan = EvaluationPlan::with_profiles(
            &[
                RetrievalProfile::SessionAuto,
                RetrievalProfile::SessionExplicit,
                RetrievalProfile::SessionAuto,
            ],
            RerankMode::Off,
        );
        assert_eq!(plan.strategies().len(), 4);

        let report = evaluate_with(&conn, &suite(), &plan).unwrap();
        assert_eq!(report.strategy_configurations.len(), 4);
        let configuration = &report.strategy_configurations[2];
        assert_eq!(configuration.profile, Some(RetrievalProfile::SessionAuto));
        assert_eq!(configuration.rerank, Some(RerankMode::Off));
        assert_eq!(configuration.passage.unwrap().document_token_budget, 3_600);
        for case in &report.cases {
            let linked = &case.strategies[1];
            let session_auto = &case.strategies[2];
            assert_eq!(
                session_auto.strategy.label(),
                "profile:session_auto:rerank_off"
            );
            assert_eq!(session_auto.candidate_ids, linked.candidate_ids);
            assert_eq!(
                session_auto
                    .selected
                    .iter()
                    .map(|document| &document.id)
                    .collect::<Vec<_>>(),
                linked
                    .selected
                    .iter()
                    .map(|document| &document.id)
                    .collect::<Vec<_>>()
            );
            assert_eq!(session_auto.gate_passed, linked.gate_passed);
            assert_eq!(session_auto.body_requirements, linked.body_requirements);
            assert_eq!(session_auto.required_rank, linked.required_rank);
        }
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"strategy\":\"profile:session_explicit:rerank_off\""));
        assert!(json.contains("\"strategy\":\"linked_v1\""));
        assert!(report.summaries[3].gate_passed);
    }

    #[test]
    fn linked_gate_fails_when_selected_bodies_lack_required_semantics() {
        let conn = one_hop_corpus();
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
        assert_eq!(
            report.summaries[1].gate_failed_cases,
            report.gate.failed_cases
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
        assert!(markdown.contains("required rank p50"));
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
        assert_eq!(suite.schema_version, LEGACY_EVALUATION_SCHEMA_VERSION);
        assert_eq!(suite.cases.len(), 1);
        assert_eq!(suite.cases[0].queries.surfaces().len(), 3);
    }
}
