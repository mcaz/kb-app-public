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
};

pub const EVALUATION_SCHEMA_VERSION: &str = "1.0.0";
pub const HOOK_SPILL_THRESHOLD_TOKENS: usize = 12_000;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenSuite {
    pub schema_version: String,
    pub cases: Vec<GoldenCase>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenCase {
    pub id: String,
    pub query: String,
    pub required: Vec<String>,
    #[serde(default)]
    pub relevant: Vec<String>,
    #[serde(default)]
    pub excluded: Vec<String>,
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
    pub cases: Vec<CaseReport>,
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
    pub query: String,
    pub required: Vec<String>,
    pub relevant: Vec<String>,
    pub excluded: Vec<String>,
    pub search_degraded: Vec<Degradation>,
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
    pub candidate_recall: f64,
    pub selected_recall: f64,
    pub selected_precision: f64,
    pub runtime: EvaluationRuntime,
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

    let mut cases = Vec::with_capacity(suite.cases.len());
    for case in &suite.cases {
        cases.push(evaluate_case(&transaction, case)?);
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
        cases,
    })
}

fn evaluate_case(conn: &Connection, case: &GoldenCase) -> Result<CaseReport> {
    let search_started = Instant::now();
    let outcome = crate::search::search_mode(conn, &case.query, AUTO_SEED_LIMIT, true);
    let search_elapsed_us = micros(search_started.elapsed());
    let ranked_hit_ids = outcome
        .hits
        .iter()
        .map(|hit| hit.id.clone())
        .collect::<Vec<_>>();

    let mut strategies = Vec::with_capacity(2);
    for strategy in [EvaluationStrategy::Top3, EvaluationStrategy::LinkedV1] {
        let bundle = context_documents(
            conn,
            &ranked_hit_ids,
            configuration_for(strategy).retrieval_options(),
        )?;
        strategies.push(score_case(case, strategy, search_elapsed_us, bundle));
    }

    Ok(CaseReport {
        id: case.id.clone(),
        query: case.query.clone(),
        required: case.required.clone(),
        relevant: case.relevant.clone(),
        excluded: case.excluded.clone(),
        search_degraded: outcome.degraded,
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
            selected_depth_0: bundle.stats.selected_depth_0,
            selected_depth_1: bundle.stats.selected_depth_1,
            selected_depth_2: bundle.stats.selected_depth_2,
            selected_incoming: bundle.stats.selected_incoming,
        },
    }
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
        "- schema: `{}`\n- core: `{}`\n- cases: {}\n- shared search: top {}, any_terms={}\n\n",
        report.schema_version,
        report.core_version,
        report.case_count,
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
    out.push_str("| case | strategy | candidate recall | selected recall | precision | selected IDs | token | total μs |\n");
    out.push_str("| --- | --- | ---: | ---: | ---: | --- | ---: | ---: |\n");
    for case in &report.cases {
        for result in &case.strategies {
            let selected = result
                .selected
                .iter()
                .map(|document| document.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "| {} | {} | {:.1}% | {:.1}% | {:.1}% | {} | {} | {} |\n",
                escape_table(&case.id),
                result.strategy.label(),
                result.candidate_recall * 100.0,
                result.selected_recall * 100.0,
                result.selected_precision * 100.0,
                escape_table(&selected),
                result.runtime.estimated_tokens,
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

    let mut case_ids = BTreeSet::new();
    for case in &suite.cases {
        if case.id.trim().is_empty() || case.id != case.id.trim() {
            bail!("Golden Queryのidは前後空白なしの非空文字列にする");
        }
        if !case_ids.insert(case.id.as_str()) {
            bail!("Golden Queryのidが重複している: {}", case.id);
        }
        if case.query.trim().is_empty() {
            bail!("{}: queryは空にできない", case.id);
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
             CREATE VIRTUAL TABLE fts_tri USING fts5(id UNINDEXED, text, tokenize='trigram');",
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
            cases: vec![GoldenCase {
                id: "one-hop".into(),
                query: "retrieval beacon".into(),
                required: vec!["notes/target".into()],
                relevant: vec!["notes/seed".into()],
                excluded: vec!["notes/wrong".into()],
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
    }
}
