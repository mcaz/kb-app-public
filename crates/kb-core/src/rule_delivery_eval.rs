//! Rule Delivery Matrixの評価専用fixture、配信計画、trace判定。
//!
//! production用Rule保存形式ではない。固定suiteを同じ条件でCodex／Claudeへ渡し、
//! delivery modeだけを切り替えて比較するための実験境界。

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::vault::{NoteProposal, Vault};

pub const RULE_DELIVERY_SCHEMA_VERSION: &str = "1.0.0";
pub const OFFICIAL_CASE_IDS: [&str; 20] = [
    "A1", "A2", "A3", "A4", "A5", "T1", "T2", "T3", "T4", "T5", "E1", "E2", "E3", "E4", "E5", "C1",
    "C2", "C3", "C4", "C5",
];
const TOP_K: usize = 3;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    SemanticOnly,
    RulesTopK,
    AlwaysTopic,
    AlwaysTopicEvent,
}

impl DeliveryMode {
    pub const ALL: [Self; 4] = [
        Self::SemanticOnly,
        Self::RulesTopK,
        Self::AlwaysTopic,
        Self::AlwaysTopicEvent,
    ];
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleScope {
    Always,
    Topic,
    Event,
    Config,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuleDeliverySuite {
    pub schema_version: String,
    pub rules: Vec<RuleFixture>,
    #[serde(default)]
    pub notes: Vec<NoteFixture>,
    pub cases: Vec<RuleCase>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuleFixture {
    pub id: String,
    pub scope: RuleScope,
    pub instruction: String,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub event_tools: Vec<String>,
    #[serde(default)]
    pub broken: bool,
    pub conflict_group: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NoteFixture {
    pub expected_id: String,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuleCase {
    pub id: String,
    pub category: String,
    pub prompt: String,
    #[serde(default)]
    pub relevant_rule_ids: Vec<String>,
    #[serde(default)]
    pub injected_degradations: Vec<String>,
    pub expected: CaseExpectation,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CaseExpectation {
    pub required_tools: Vec<String>,
    pub forbidden_tools: Vec<String>,
    pub required_note_ids: Vec<String>,
    pub response_must_include: Vec<String>,
    pub response_must_not_include: Vec<String>,
    pub pre_tool_must_include: Vec<String>,
    pub required_write_tags: Vec<String>,
    pub forbidden_write_tags: Vec<String>,
    pub required_conversation_links: usize,
    pub required_conversation_events: Vec<String>,
    pub require_artifact_identity: bool,
    pub require_degraded: bool,
    pub skip_kb_tools: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeliveryPlan {
    pub schema_version: &'static str,
    pub mode: DeliveryMode,
    pub cases: Vec<PreparedCase>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PreparedCase {
    pub case_id: String,
    pub category: String,
    pub prompt: String,
    pub prompt_context: String,
    pub delivered_rule_ids: Vec<String>,
    pub event_rules: Vec<PreparedEventRule>,
    pub degraded_codes: Vec<String>,
    pub injected_degradations: Vec<String>,
    pub estimated_rule_tokens: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PreparedEventRule {
    pub rule_id: String,
    pub after_tools: Vec<String>,
    pub instruction: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TraceSuite {
    pub schema_version: String,
    pub traces: Vec<RunTrace>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunTrace {
    pub case_id: String,
    pub mode: DeliveryMode,
    pub client_surface: String,
    pub model: String,
    pub model_version: Option<String>,
    pub os: String,
    pub run: u32,
    #[serde(default)]
    pub delivered_rule_ids: Vec<String>,
    #[serde(default)]
    pub event_rule_ids: Vec<String>,
    #[serde(default)]
    pub degraded_codes: Vec<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCallTrace>,
    pub response: String,
    pub input_tokens: Option<u64>,
    pub latency_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCallTrace {
    pub name: String,
    #[serde(default)]
    pub pre_tool_text: Option<String>,
    #[serde(default)]
    pub arguments: Value,
    #[serde(default)]
    pub result: Value,
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct RuleDeliveryReport {
    pub schema_version: &'static str,
    pub core_version: &'static str,
    pub trace_count: usize,
    pub summaries: Vec<ModeSummary>,
    pub traces: Vec<TraceReport>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModeSummary {
    pub mode: DeliveryMode,
    pub traces: usize,
    pub passed: usize,
    pub hard_failures: usize,
    pub irrelevant_rules: usize,
    pub average_rule_tokens: f64,
    pub average_input_tokens: Option<f64>,
    pub average_latency_ms: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TraceReport {
    pub case_id: String,
    pub mode: DeliveryMode,
    pub client_surface: String,
    pub model: String,
    pub os: String,
    pub run: u32,
    pub passed: bool,
    pub failures: Vec<String>,
    pub expected_rule_ids: Vec<String>,
    pub delivered_rule_ids: Vec<String>,
    pub irrelevant_rule_ids: Vec<String>,
    pub estimated_rule_tokens: usize,
    pub input_tokens: Option<u64>,
    pub latency_ms: Option<u64>,
}

pub fn validate_official_suite(suite: &RuleDeliverySuite) -> Result<()> {
    validate_suite(suite)?;
    let actual = suite
        .cases
        .iter()
        .map(|case| case.id.as_str())
        .collect::<BTreeSet<_>>();
    let expected = OFFICIAL_CASE_IDS.into_iter().collect::<BTreeSet<_>>();
    if actual != expected {
        bail!("公式Rule Delivery suiteはA1〜A5/T1〜T5/E1〜E5/C1〜C5の20件が必要");
    }
    Ok(())
}

pub fn validate_suite(suite: &RuleDeliverySuite) -> Result<()> {
    if suite.schema_version != RULE_DELIVERY_SCHEMA_VERSION {
        bail!(
            "unsupported Rule Delivery schema_version: {}",
            suite.schema_version
        );
    }
    if suite.rules.is_empty() || suite.cases.is_empty() {
        bail!("rulesとcasesは1件以上必要");
    }
    unique("rule", suite.rules.iter().map(|rule| rule.id.as_str()))?;
    unique("case", suite.cases.iter().map(|case| case.id.as_str()))?;
    unique(
        "note fixture",
        suite.notes.iter().map(|note| note.expected_id.as_str()),
    )?;

    let rule_ids = suite
        .rules
        .iter()
        .map(|rule| rule.id.as_str())
        .collect::<BTreeSet<_>>();
    for case in &suite.cases {
        for id in &case.relevant_rule_ids {
            if !rule_ids.contains(id.as_str()) {
                bail!("case {} が未知のRule {}を参照している", case.id, id);
            }
        }
    }
    for rule in &suite.rules {
        if rule.scope == RuleScope::Event && rule.event_tools.is_empty() {
            bail!("event rule {}にはevent_toolsが必要", rule.id);
        }
        if rule.instruction.trim().is_empty() {
            bail!("rule {}のinstructionが空", rule.id);
        }
    }
    Ok(())
}

pub fn prepare(suite: &RuleDeliverySuite, mode: DeliveryMode) -> Result<DeliveryPlan> {
    validate_suite(suite)?;
    let cases = suite
        .cases
        .iter()
        .map(|case| prepare_case(suite, mode, case))
        .collect::<Result<Vec<_>>>()?;
    Ok(DeliveryPlan {
        schema_version: RULE_DELIVERY_SCHEMA_VERSION,
        mode,
        cases,
    })
}

/// 固定suiteのノートだけを持つ使い捨てVaultを作る。既存directoryへ混ぜず、
/// 実KBを評価対象にしないための境界。
pub fn create_fixture(suite: &RuleDeliverySuite, path: &Path) -> Result<Vault> {
    validate_official_suite(suite)?;
    if path.exists() && std::fs::read_dir(path)?.next().is_some() {
        bail!("評価fixtureの出力先が空でない: {}", path.display());
    }
    let vault = Vault::create(path)?;
    let conn = crate::index::open_db(&vault)?;
    for note in &suite.notes {
        let id = vault.propose(
            &conn,
            NoteProposal {
                title: &note.title,
                body: &note.body,
                description: Some("Rule Delivery Matrix evaluation fixture"),
                tags: &note.tags,
                allow_new_tags: true,
                client: "rule-delivery-eval/fixture",
            },
        )?;
        if id != note.expected_id {
            bail!(
                "fixture note IDが不一致: expected={} actual={}",
                note.expected_id,
                id
            );
        }
    }
    crate::index::sync(&vault, &conn)?;
    Ok(vault)
}

pub fn score(suite: &RuleDeliverySuite, traces: &TraceSuite) -> Result<RuleDeliveryReport> {
    validate_suite(suite)?;
    if traces.schema_version != RULE_DELIVERY_SCHEMA_VERSION {
        bail!(
            "trace schema_versionが一致しない: {}",
            traces.schema_version
        );
    }
    let cases = suite
        .cases
        .iter()
        .map(|case| (case.id.as_str(), case))
        .collect::<HashMap<_, _>>();
    let mut reports = Vec::with_capacity(traces.traces.len());
    for trace in &traces.traces {
        let case = cases
            .get(trace.case_id.as_str())
            .with_context(|| format!("traceが未知のcase {}を参照している", trace.case_id))?;
        let prepared = prepare_case(suite, trace.mode, case)?;
        reports.push(score_trace(suite, case, &prepared, trace));
    }
    let summaries = DeliveryMode::ALL
        .into_iter()
        .filter_map(|mode| summarize(mode, &reports))
        .collect();
    Ok(RuleDeliveryReport {
        schema_version: RULE_DELIVERY_SCHEMA_VERSION,
        core_version: crate::CORE_VERSION,
        trace_count: reports.len(),
        summaries,
        traces: reports,
    })
}

fn unique<'a>(label: &str, values: impl Iterator<Item = &'a str>) -> Result<()> {
    let mut seen = BTreeSet::new();
    for value in values {
        if value.trim().is_empty() {
            bail!("{label} IDが空");
        }
        if !seen.insert(value) {
            bail!("{label} IDが重複している: {value}");
        }
    }
    Ok(())
}

fn prepare_case(
    suite: &RuleDeliverySuite,
    mode: DeliveryMode,
    case: &RuleCase,
) -> Result<PreparedCase> {
    let mut degraded = Vec::new();
    let matching = suite
        .rules
        .iter()
        .filter(|rule| rule_matches(rule, &case.prompt))
        .collect::<Vec<_>>();

    if mode != DeliveryMode::SemanticOnly {
        for rule in &matching {
            if rule.broken {
                degraded.push(format!("rule_broken:{}", rule.id));
            }
        }
    }

    let mut conflicted = BTreeSet::new();
    let mut groups: BTreeMap<&str, Vec<&RuleFixture>> = BTreeMap::new();
    if mode != DeliveryMode::SemanticOnly {
        for rule in matching.iter().filter(|rule| !rule.broken) {
            if let Some(group) = rule.conflict_group.as_deref() {
                groups.entry(group).or_default().push(rule);
            }
        }
    }
    for (group, rules) in groups {
        if rules.len() > 1 {
            degraded.push(format!("rule_conflict:{group}"));
            conflicted.extend(rules.into_iter().map(|rule| rule.id.as_str()));
        }
    }

    let mut delivered = match mode {
        DeliveryMode::SemanticOnly => Vec::new(),
        DeliveryMode::RulesTopK => {
            // 「Rulesを毎回top-k検索」は0 hitにせず、低関連でもk件を配送する比較条件。
            // これにより一般質問での無関係Rule混入を実測できる。
            let mut ranked = suite
                .rules
                .iter()
                .filter(|rule| rule.scope != RuleScope::Event)
                .filter(|rule| !rule.broken && !conflicted.contains(rule.id.as_str()))
                .collect::<Vec<_>>();
            ranked
                .sort_by_key(|rule| (std::cmp::Reverse(match_score(rule, &case.prompt)), &rule.id));
            ranked.into_iter().take(TOP_K).collect()
        }
        DeliveryMode::AlwaysTopic | DeliveryMode::AlwaysTopicEvent => {
            let always = suite
                .rules
                .iter()
                .filter(|rule| rule.scope == RuleScope::Always)
                .filter(|rule| !rule.broken && !conflicted.contains(rule.id.as_str()));
            let topical = matching
                .iter()
                .copied()
                .filter(|rule| matches!(rule.scope, RuleScope::Topic | RuleScope::Config))
                .filter(|rule| !rule.broken && !conflicted.contains(rule.id.as_str()));
            always.chain(topical).collect::<Vec<_>>()
        }
    };
    delivered.sort_by(|left, right| left.id.cmp(&right.id));
    delivered.dedup_by(|left, right| left.id == right.id);

    let event_rules = if mode == DeliveryMode::AlwaysTopicEvent {
        suite
            .rules
            .iter()
            .filter(|rule| rule.scope == RuleScope::Event && !rule.broken)
            .filter(|rule| {
                rule.event_tools
                    .iter()
                    .any(|tool| case.expected.required_tools.contains(tool))
            })
            .map(|rule| PreparedEventRule {
                rule_id: rule.id.clone(),
                after_tools: rule.event_tools.clone(),
                instruction: rule.instruction.clone(),
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    degraded.sort();
    degraded.dedup();
    let prompt_context = render_context(&delivered, &degraded);
    let estimated_rule_tokens = estimate_tokens(&prompt_context);
    Ok(PreparedCase {
        case_id: case.id.clone(),
        category: case.category.clone(),
        prompt: case.prompt.clone(),
        prompt_context,
        delivered_rule_ids: delivered.iter().map(|rule| rule.id.clone()).collect(),
        event_rules,
        degraded_codes: degraded,
        injected_degradations: case.injected_degradations.clone(),
        estimated_rule_tokens,
    })
}

fn rule_matches(rule: &RuleFixture, prompt: &str) -> bool {
    rule.triggers
        .iter()
        .any(|trigger| !trigger.is_empty() && prompt.contains(trigger))
}

fn match_score(rule: &RuleFixture, prompt: &str) -> usize {
    rule.triggers
        .iter()
        .filter(|trigger| !trigger.is_empty() && prompt.contains(trigger.as_str()))
        .map(|trigger| trigger.chars().count())
        .sum()
}

fn render_context(rules: &[&RuleFixture], degraded: &[String]) -> String {
    if rules.is_empty() && degraded.is_empty() {
        return String::new();
    }
    let mut lines = vec!["[Rule Delivery Matrix — evaluation context]".to_string()];
    lines.extend(rules.iter().map(|rule| {
        format!(
            "- [{}|{:?}] {}",
            rule.id,
            rule.scope,
            rule.instruction.trim()
        )
    }));
    lines.extend(
        degraded
            .iter()
            .map(|code| format!("⚠ Rule delivery degraded: {code}")),
    );
    lines.join("\n")
}

fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

fn score_trace(
    suite: &RuleDeliverySuite,
    case: &RuleCase,
    prepared: &PreparedCase,
    trace: &RunTrace,
) -> TraceReport {
    let mut failures = Vec::new();
    if trace.delivered_rule_ids != prepared.delivered_rule_ids {
        failures.push("delivered_rule_ids_mismatch".into());
    }
    let expected_event_ids = prepared
        .event_rules
        .iter()
        .map(|rule| rule.rule_id.clone())
        .collect::<Vec<_>>();
    if trace.event_rule_ids != expected_event_ids {
        failures.push("event_rule_ids_mismatch".into());
    }

    let actual_tools = trace
        .tool_calls
        .iter()
        .map(|call| call.name.as_str())
        .collect::<Vec<_>>();
    if !ordered_subsequence(&case.expected.required_tools, &actual_tools) {
        failures.push("required_tool_sequence_missing".into());
    }
    if case
        .expected
        .forbidden_tools
        .iter()
        .any(|tool| actual_tools.contains(&tool.as_str()))
    {
        failures.push("forbidden_tool_called".into());
    }
    if case.expected.skip_kb_tools
        && actual_tools.iter().any(|tool| {
            matches!(
                *tool,
                "search"
                    | "get"
                    | "recent"
                    | "propose"
                    | "update"
                    | "prepare_remove"
                    | "commit_remove"
                    | "attach"
            )
        })
    {
        failures.push("unexpected_kb_tool".into());
    }
    if actual_tools.iter().any(|tool| tool.starts_with("builtin.")) {
        failures.push("unapproved_builtin_tool".into());
    }
    if trace.tool_calls.iter().any(|call| call.is_error) {
        failures.push("tool_error".into());
    }
    if !case.expected.pre_tool_must_include.is_empty() {
        let notice = trace
            .tool_calls
            .iter()
            .find(|call| {
                matches!(
                    call.name.as_str(),
                    "propose" | "update" | "prepare_remove" | "commit_remove" | "attach"
                )
            })
            .and_then(|call| call.pre_tool_text.as_deref())
            .unwrap_or("");
        for required in &case.expected.pre_tool_must_include {
            if !notice.contains(required) {
                failures.push(format!("pre_tool_notice_missing:{required}"));
            }
        }
    }

    let trace_json = serde_json::to_string(&trace.tool_calls).unwrap_or_default();
    for note in &case.expected.required_note_ids {
        if !trace_json.contains(note) {
            failures.push(format!("required_note_missing:{note}"));
        }
    }
    for required in &case.expected.response_must_include {
        if !trace.response.contains(required) {
            failures.push(format!("response_missing:{required}"));
        }
    }
    for excluded in &case.expected.response_must_not_include {
        if trace.response.contains(excluded) {
            failures.push(format!("response_contains_forbidden:{excluded}"));
        }
    }
    let successful_write_tags = trace
        .tool_calls
        .iter()
        .filter(|call| !call.is_error && matches!(call.name.as_str(), "propose" | "update"))
        .flat_map(|call| {
            call.arguments
                .get("tags")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
        })
        .collect::<BTreeSet<_>>();
    for required in &case.expected.required_write_tags {
        if !successful_write_tags.contains(required.as_str()) {
            failures.push(format!("required_write_tag_missing:{required}"));
        }
    }
    for forbidden in &case.expected.forbidden_write_tags {
        if successful_write_tags.contains(forbidden.as_str()) {
            failures.push(format!("forbidden_write_tag:{forbidden}"));
        }
    }
    let mut allowed_note_ids = suite
        .notes
        .iter()
        .map(|note| note.expected_id.clone())
        .collect::<BTreeSet<_>>();
    allowed_note_ids.extend(collect_string_fields(&trace.tool_calls, "note_id"));
    for note_id in response_note_references(&trace.response) {
        if !allowed_note_ids.contains(&note_id) {
            failures.push(format!("unexpected_note_reference:{note_id}"));
        }
    }

    let delivered_links = required_note_link_events(trace);
    if delivered_links.len() < case.expected.required_conversation_links {
        failures.push("required_conversation_link_event_missing".into());
    }
    let conversation_events = required_conversation_events(trace);
    for required in &case.expected.required_conversation_events {
        if !conversation_events.contains(required) {
            failures.push(format!("conversation_event_missing:{required}"));
        }
    }
    if case.expected.require_artifact_identity && !artifact_identity_reported(trace) {
        failures.push("artifact_identity_missing_from_response".into());
    }
    if case.expected.require_degraded
        && !trace.response.contains("劣化")
        && !trace.response.to_ascii_lowercase().contains("degraded")
    {
        failures.push("degradation_not_reported".into());
    }

    let relevant = case
        .relevant_rule_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let irrelevant_rule_ids = trace
        .delivered_rule_ids
        .iter()
        .filter(|id| !relevant.contains(id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    TraceReport {
        case_id: trace.case_id.clone(),
        mode: trace.mode,
        client_surface: trace.client_surface.clone(),
        model: trace.model.clone(),
        os: trace.os.clone(),
        run: trace.run,
        passed: failures.is_empty(),
        failures,
        expected_rule_ids: prepared.delivered_rule_ids.clone(),
        delivered_rule_ids: trace.delivered_rule_ids.clone(),
        irrelevant_rule_ids,
        estimated_rule_tokens: prepared.estimated_rule_tokens,
        input_tokens: trace.input_tokens,
        latency_ms: trace.latency_ms,
    }
}

fn ordered_subsequence(required: &[String], actual: &[&str]) -> bool {
    let mut index = 0;
    for tool in actual {
        if required.get(index).is_some_and(|expected| expected == tool) {
            index += 1;
        }
    }
    index == required.len()
}

fn collect_string_fields<T: Serialize>(value: &T, key: &str) -> BTreeSet<String> {
    fn walk(value: &Value, key: &str, out: &mut BTreeSet<String>) {
        match value {
            Value::Object(map) => {
                if let Some(value) = map.get(key).and_then(Value::as_str) {
                    out.insert(value.to_string());
                }
                for value in map.values() {
                    walk(value, key, out);
                }
            }
            Value::Array(values) => {
                for value in values {
                    walk(value, key, out);
                }
            }
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    if let Ok(value) = serde_json::to_value(value) {
        walk(&value, key, &mut out);
    }
    out
}

fn required_note_link_events(trace: &RunTrace) -> BTreeSet<String> {
    trace
        .tool_calls
        .iter()
        .filter_map(|call| {
            call.result
                .pointer("/structuredContent/conversation_events")
                .and_then(Value::as_array)
        })
        .flatten()
        .filter(|event| {
            event.get("type").and_then(Value::as_str) == Some("note_link")
                && event.get("required").and_then(Value::as_bool) == Some(true)
        })
        .filter_map(|event| event.get("conversation_link").and_then(Value::as_str))
        .filter(|link| !link.is_empty())
        .map(str::to_string)
        .collect()
}

fn required_conversation_events(trace: &RunTrace) -> BTreeSet<String> {
    trace
        .tool_calls
        .iter()
        .filter_map(|call| {
            call.result
                .pointer("/structuredContent/conversation_events")
                .and_then(Value::as_array)
        })
        .flatten()
        .filter(|event| event.get("required").and_then(Value::as_bool) == Some(true))
        .filter_map(|event| event.get("event").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

fn response_note_references(response: &str) -> BTreeSet<String> {
    response
        .match_indices("/notes/")
        .filter_map(|(index, _)| {
            let rest = &response[index + 1..];
            let end = rest.find(".md")?;
            let note_id = &rest[..end];
            (!note_id
                .chars()
                .any(|ch| matches!(ch, '\n' | '\r' | '(' | ')' | '[' | ']' | '`' | ' ')))
            .then(|| note_id.to_string())
        })
        .collect()
}

fn artifact_identity_reported(trace: &RunTrace) -> bool {
    let Some(call) = trace.tool_calls.iter().find(|call| call.name == "attach") else {
        return false;
    };
    ["artifact_id", "ref_name", "availability"]
        .into_iter()
        .all(|key| {
            let values = collect_string_fields(&call.result, key);
            !values.is_empty()
                && values
                    .iter()
                    .all(|value| trace.response.contains(value.as_str()))
        })
}

fn summarize(mode: DeliveryMode, reports: &[TraceReport]) -> Option<ModeSummary> {
    let selected = reports
        .iter()
        .filter(|report| report.mode == mode)
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return None;
    }
    let average = |values: Vec<u64>| {
        (!values.is_empty())
            .then(|| values.iter().map(|value| *value as f64).sum::<f64>() / values.len() as f64)
    };
    Some(ModeSummary {
        mode,
        traces: selected.len(),
        passed: selected.iter().filter(|report| report.passed).count(),
        hard_failures: selected.iter().filter(|report| !report.passed).count(),
        irrelevant_rules: selected
            .iter()
            .map(|report| report.irrelevant_rule_ids.len())
            .sum(),
        average_rule_tokens: selected
            .iter()
            .map(|report| report.estimated_rule_tokens as f64)
            .sum::<f64>()
            / selected.len() as f64,
        average_input_tokens: average(
            selected
                .iter()
                .filter_map(|report| report.input_tokens)
                .collect(),
        ),
        average_latency_ms: average(
            selected
                .iter()
                .filter_map(|report| report.latency_ms)
                .collect(),
        ),
    })
}

pub fn render_report_markdown(report: &RuleDeliveryReport) -> String {
    let mut out = format!(
        "# Rule Delivery evaluation\n\n- traces: {}\n- schema: {}\n\n| mode | pass | hard failures | irrelevant rules | avg rule tokens | avg input tokens | avg latency ms |\n|---|---:|---:|---:|---:|---:|---:|\n",
        report.trace_count, report.schema_version
    );
    for summary in &report.summaries {
        out.push_str(&format!(
            "| {:?} | {}/{} | {} | {} | {:.1} | {} | {} |\n",
            summary.mode,
            summary.passed,
            summary.traces,
            summary.hard_failures,
            summary.irrelevant_rules,
            summary.average_rule_tokens,
            optional_number(summary.average_input_tokens),
            optional_number(summary.average_latency_ms),
        ));
    }
    out.push_str("\n## Failures\n\n");
    let mut failures = 0;
    for trace in report.traces.iter().filter(|trace| !trace.passed) {
        failures += 1;
        out.push_str(&format!(
            "- {} / {:?} / run {}: {}\n",
            trace.case_id,
            trace.mode,
            trace.run,
            trace.failures.join(", ")
        ));
    }
    if failures == 0 {
        out.push_str("- none\n");
    }
    out
}

fn optional_number(value: Option<f64>) -> String {
    value.map_or_else(|| "—".into(), |value| format!("{value:.1}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suite() -> RuleDeliverySuite {
        RuleDeliverySuite {
            schema_version: RULE_DELIVERY_SCHEMA_VERSION.into(),
            notes: Vec::new(),
            rules: vec![
                RuleFixture {
                    id: "always.search-personal".into(),
                    scope: RuleScope::Always,
                    instruction: "個人の話題は検索する".into(),
                    triggers: vec!["姉".into()],
                    event_tools: Vec::new(),
                    broken: false,
                    conflict_group: None,
                },
                RuleFixture {
                    id: "topic.os".into(),
                    scope: RuleScope::Topic,
                    instruction: "OS方針を参照する".into(),
                    triggers: vec!["OS".into()],
                    event_tools: Vec::new(),
                    broken: false,
                    conflict_group: None,
                },
                RuleFixture {
                    id: "event.link".into(),
                    scope: RuleScope::Event,
                    instruction: "取得後にリンクを出す".into(),
                    triggers: Vec::new(),
                    event_tools: vec!["get".into()],
                    broken: false,
                    conflict_group: None,
                },
            ],
            cases: vec![RuleCase {
                id: "A1".into(),
                category: "always".into(),
                prompt: "姉のOSは？".into(),
                relevant_rule_ids: vec!["always.search-personal".into(), "topic.os".into()],
                injected_degradations: Vec::new(),
                expected: CaseExpectation {
                    required_tools: vec!["search".into(), "get".into()],
                    required_note_ids: vec!["notes/profile".into()],
                    response_must_include: vec!["Windows".into()],
                    required_conversation_links: 1,
                    required_conversation_events: vec!["note_read".into()],
                    ..Default::default()
                },
            }],
        }
    }

    #[test]
    fn modes_keep_always_and_event_delivery_separate() {
        let suite = suite();
        let baseline = prepare(&suite, DeliveryMode::SemanticOnly).unwrap();
        assert!(baseline.cases[0].delivered_rule_ids.is_empty());

        let mixed = prepare(&suite, DeliveryMode::AlwaysTopic).unwrap();
        assert_eq!(
            mixed.cases[0].delivered_rule_ids,
            ["always.search-personal", "topic.os"]
        );
        assert!(mixed.cases[0].event_rules.is_empty());

        let event = prepare(&suite, DeliveryMode::AlwaysTopicEvent).unwrap();
        assert_eq!(event.cases[0].event_rules[0].rule_id, "event.link");
        assert_eq!(event.cases[0].event_rules[0].after_tools, ["get"]);
    }

    #[test]
    fn scorer_requires_tool_order_and_link_event() {
        let mut suite = suite();
        let prepared = prepare(&suite, DeliveryMode::AlwaysTopicEvent).unwrap();
        let link = "/tmp/eval/notes/profile.md";
        let mut traces = TraceSuite {
            schema_version: RULE_DELIVERY_SCHEMA_VERSION.into(),
            traces: vec![RunTrace {
                case_id: "A1".into(),
                mode: DeliveryMode::AlwaysTopicEvent,
                client_surface: "codex".into(),
                model: "test".into(),
                model_version: None,
                os: "macos".into(),
                run: 1,
                delivered_rule_ids: prepared.cases[0].delivered_rule_ids.clone(),
                event_rule_ids: vec!["event.link".into()],
                degraded_codes: Vec::new(),
                tool_calls: vec![
                    ToolCallTrace {
                        name: "search".into(),
                        pre_tool_text: None,
                        arguments: serde_json::json!({"query": "姉 OS"}),
                        result: serde_json::json!({"hits": [{"id": "notes/profile"}]}),
                        is_error: false,
                    },
                    ToolCallTrace {
                        name: "get".into(),
                        pre_tool_text: None,
                        arguments: serde_json::json!({"note": "notes/profile"}),
                        result: serde_json::json!({"structuredContent": {
                            "note_id": "notes/profile",
                            "conversation_link": link,
                            "conversation_events": [{
                                "type": "note_link",
                                "event": "note_read",
                                "required": true,
                                "conversation_link": link
                            }]
                        }}),
                        is_error: false,
                    },
                ],
                response: "Windowsです。".into(),
                input_tokens: Some(120),
                latency_ms: Some(800),
            }],
        };
        let report = score(&suite, &traces).unwrap();
        assert!(report.traces[0].passed, "{:?}", report.traces[0].failures);

        traces.traces[0].tool_calls[1].result["structuredContent"]["conversation_events"] =
            serde_json::json!([]);
        let report = score(&suite, &traces).unwrap();
        assert!(
            report.traces[0]
                .failures
                .contains(&"required_conversation_link_event_missing".to_string())
        );
        assert!(
            report.traces[0]
                .failures
                .contains(&"conversation_event_missing:note_read".to_string())
        );
        traces.traces[0].tool_calls[1].result["structuredContent"]["conversation_events"] = serde_json::json!([{
            "type": "note_link",
            "event": "note_read",
            "required": true,
            "conversation_link": link
        }]);

        suite.cases[0].expected.require_degraded = true;
        traces.traces[0].degraded_codes = vec!["fixture:degraded".into()];
        let report = score(&suite, &traces).unwrap();
        assert!(
            report.traces[0]
                .failures
                .contains(&"degradation_not_reported".to_string())
        );
        suite.cases[0].expected.require_degraded = false;
        traces.traces[0].degraded_codes.clear();

        suite.cases[0].expected.required_write_tags = vec!["review".into()];
        let report = score(&suite, &traces).unwrap();
        assert!(
            report.traces[0]
                .failures
                .contains(&"required_write_tag_missing:review".to_string())
        );
        traces.traces[0].tool_calls.push(ToolCallTrace {
            name: "propose".into(),
            pre_tool_text: None,
            arguments: serde_json::json!({"tags": ["review", "assistant-eval"]}),
            result: serde_json::json!({}),
            is_error: false,
        });
        suite.cases[0].expected.forbidden_write_tags = vec!["assistant-eval".into()];
        let report = score(&suite, &traces).unwrap();
        assert!(
            report.traces[0]
                .failures
                .contains(&"forbidden_write_tag:assistant-eval".to_string())
        );
        suite.cases[0].expected.required_write_tags.clear();
        suite.cases[0].expected.forbidden_write_tags.clear();
        traces.traces[0].tool_calls.pop();

        traces.traces[0]
            .response
            .push_str(" [outside](/notes/real-kb-note.md)");
        let report = score(&suite, &traces).unwrap();
        assert!(
            report.traces[0]
                .failures
                .contains(&"unexpected_note_reference:notes/real-kb-note".to_string())
        );

        traces.traces[0].tool_calls.push(ToolCallTrace {
            name: "builtin.command_execution".into(),
            pre_tool_text: None,
            arguments: serde_json::json!({"command": "rg notes"}),
            result: serde_json::json!({}),
            is_error: false,
        });
        let report = score(&suite, &traces).unwrap();
        assert!(
            report.traces[0]
                .failures
                .contains(&"unapproved_builtin_tool".to_string())
        );
    }

    #[test]
    fn official_twenty_case_suite_materializes_in_an_isolated_vault() {
        let suite: RuleDeliverySuite = serde_json::from_str(include_str!(
            "../../../schemas/examples/rule-delivery-eval.example.json"
        ))
        .unwrap();
        validate_official_suite(&suite).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture");
        let vault = create_fixture(&suite, &path).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let note = vault.read_note_from_db(&conn, "notes/テスト環境").unwrap();
        assert!(note.body.contains("姉夫婦"));
        assert_eq!(
            crate::tags::vocabulary(&conn).unwrap(),
            BTreeSet::from([
                "governance".to_string(),
                "kb-app".to_string(),
                "mcp".to_string(),
                "review".to_string(),
            ])
        );
        assert_eq!(suite.cases.len(), 20);

        let baseline = prepare(&suite, DeliveryMode::SemanticOnly).unwrap();
        let baseline_c4 = baseline
            .cases
            .iter()
            .find(|case| case.case_id == "C4")
            .unwrap();
        assert!(baseline_c4.prompt_context.is_empty());
        assert!(baseline_c4.degraded_codes.is_empty());

        let rule_delivery = prepare(&suite, DeliveryMode::AlwaysTopic).unwrap();
        let delivered_c4 = rule_delivery
            .cases
            .iter()
            .find(|case| case.case_id == "C4")
            .unwrap();
        assert_eq!(delivered_c4.degraded_codes.len(), 2);
        assert!(delivered_c4.prompt_context.contains("rule_broken"));
        assert!(delivered_c4.prompt_context.contains("rule_conflict"));
    }
}
