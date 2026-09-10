//! 自動retrieval向けの候補展開と本文予算選択。
//!
//! 検索順位だけで固定件数を返さず、上位seedからDB内の有向リンクを辿って候補を広げる。
//! 探索候補数とモデル可視本文量は別々に制限し、数千〜10,000ノートでもグラフ全体や
//! 全文をコンテキストへ流さない。

use std::collections::HashSet;
use std::time::Instant;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

use crate::retrieval_profile::PassagePolicy;

pub const AUTO_SEED_LIMIT: usize = 5;
pub const AUTO_MAX_DEPTH: u8 = 2;
pub const AUTO_CANDIDATE_LIMIT: usize = 50;
pub const AUTO_DOCUMENT_LIMIT: usize = 10;
/// 検索側で選ぶ本文の予算。host向けstdout全体の上限は`hook_delivery`で別に適用する。
pub const AUTO_ESTIMATED_TOKEN_BUDGET: usize = 10_000;

/// 候補展開と本文選択の予算。既定は契約 8 の hook 数値(= `RetrievalProfile::SessionAuto`)。
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub struct RetrievalOptions {
    pub seed_limit: usize,
    pub max_depth: u8,
    pub candidate_limit: usize,
    pub document_limit: usize,
    pub estimated_token_budget: usize,
    pub include_incoming: bool,
    /// 長文ノートの passage 縮約予算。query の無い `context_documents` では使わない。
    pub passage: PassagePolicy,
}

impl Default for RetrievalOptions {
    fn default() -> Self {
        Self {
            seed_limit: AUTO_SEED_LIMIT,
            max_depth: AUTO_MAX_DEPTH,
            candidate_limit: AUTO_CANDIDATE_LIMIT,
            document_limit: AUTO_DOCUMENT_LIMIT,
            estimated_token_budget: AUTO_ESTIMATED_TOKEN_BUDGET,
            include_incoming: true,
            passage: PassagePolicy::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalSource {
    Search,
    OutgoingLink,
    IncomingLink,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct RetrievalDocument {
    pub id: String,
    pub text: String,
    pub source: RetrievalSource,
    pub depth: u8,
    pub seed: String,
    pub estimated_tokens: usize,
    /// 誰がいつ書いたかの1行(契約20)。イベントが1件も無いノートは None。
    /// 本文には混ぜない — 混ぜると蒸留・検索の入力が履歴文で汚れる(ADR-0023)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance_line: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct RetrievalCandidate {
    pub id: String,
    pub title: Option<String>,
    pub source: RetrievalSource,
    pub depth: u8,
    pub seed: String,
    pub selected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub omitted_reason: Option<&'static str>,
    /// card-lite(`OutputShape::CardLite`)の素材。本文を読まずに候補の authority を
    /// 見られるようにする。envelope の無い legacy note では出力しない。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authority_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authority_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authority_scope: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct RetrievalStats {
    pub seed_count: usize,
    pub candidate_count: usize,
    pub candidate_limit: usize,
    pub selected_count: usize,
    pub document_limit: usize,
    pub estimated_tokens: usize,
    pub estimated_token_budget: usize,
    pub selected_depth_0: usize,
    pub selected_depth_1: usize,
    pub selected_depth_2: usize,
    pub selected_incoming: usize,
    pub skipped_for_budget: usize,
    pub missing_documents: usize,
    pub unselected_count: usize,
    pub budget_exhausted: bool,
    pub document_cap_reached: bool,
    pub candidate_cap_reached: bool,
    pub elapsed_us: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct RetrievalBundle {
    pub documents: Vec<RetrievalDocument>,
    pub candidates: Vec<RetrievalCandidate>,
    pub stats: RetrievalStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub judgment_context: Option<crate::judgment_context::JudgmentContext>,
}

#[derive(Clone, Debug)]
struct Candidate {
    id: String,
    source: RetrievalSource,
    depth: u8,
    seed: String,
}

struct CandidateRow {
    title: Option<String>,
    document: String,
    note_uid: Option<String>,
    namespace: Option<String>,
    authority_role: Option<String>,
    authority_status: Option<String>,
    authority_scope: Option<String>,
}

/// 検索上位seed → 出リンク最大2ホップ → seedへの被リンク、の順に候補化し、
/// 総量予算内の本文だけを同じDB snapshotから返す。
pub fn context_documents(
    conn: &Connection,
    ranked_hit_ids: &[String],
    options: RetrievalOptions,
) -> Result<RetrievalBundle> {
    context_documents_inner(conn, ranked_hit_ids, None, options, None, true)
}

/// 検索queryに合う見出し・passageだけへ長文ノートを縮約して返す本番retrieval経路。
pub fn context_documents_for_query(
    conn: &Connection,
    ranked_hit_ids: &[String],
    query: &str,
    options: RetrievalOptions,
) -> Result<RetrievalBundle> {
    context_documents_inner(conn, ranked_hit_ids, Some(query), options, None, true)
}

/// scopeは呼出元の構造化入力だけを使い、自由文queryから適用済みと推測しない。
pub fn context_documents_for_query_in_scope(
    conn: &Connection,
    ranked_hit_ids: &[String],
    query: &str,
    options: RetrievalOptions,
    context_scope: Option<&str>,
) -> Result<RetrievalBundle> {
    context_documents_inner(
        conn,
        ranked_hit_ids,
        Some(query),
        options,
        context_scope,
        true,
    )
}

/// 任意の判断情報が壊れても、呼出元が劣化を通知したうえで通常本文を返せる退避口。
pub(crate) fn context_documents_for_query_without_judgment(
    conn: &Connection,
    ranked_hit_ids: &[String],
    query: &str,
    options: RetrievalOptions,
) -> Result<RetrievalBundle> {
    context_documents_inner(conn, ranked_hit_ids, Some(query), options, None, false)
}

fn context_documents_inner(
    conn: &Connection,
    ranked_hit_ids: &[String],
    query: Option<&str>,
    options: RetrievalOptions,
    context_scope: Option<&str>,
    include_judgment: bool,
) -> Result<RetrievalBundle> {
    let started = Instant::now();
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();

    let mut visible_seed = conn.prepare_cached(
        "SELECT 1 FROM notes WHERE id = ?1 AND status != 'deprecated'
           AND normal_reference_allowed = 1",
    )?;
    for id in ranked_hit_ids {
        if candidates.len() >= options.seed_limit {
            break;
        }
        // 既知IDを渡された場合も候補案内へ露出させず、非表示票でseed枠を消費しない。
        if visible_seed
            .query_row([id], |_| Ok(()))
            .optional()?
            .is_none()
        {
            continue;
        }
        push_candidate(
            &mut candidates,
            &mut seen,
            Candidate {
                id: id.clone(),
                source: RetrievalSource::Search,
                depth: 0,
                seed: id.clone(),
            },
            options.candidate_limit,
        );
    }
    let seed_count = candidates.len();
    let seeds = candidates.clone();
    let mut frontier = seeds.clone();

    for depth in 1..=options.max_depth {
        if candidates.len() >= options.candidate_limit {
            break;
        }
        let mut next = Vec::new();
        for parent in &frontier {
            for id in outgoing_ids(conn, &parent.id)? {
                let candidate = Candidate {
                    id,
                    source: RetrievalSource::OutgoingLink,
                    depth,
                    seed: parent.seed.clone(),
                };
                if push_candidate(
                    &mut candidates,
                    &mut seen,
                    candidate.clone(),
                    options.candidate_limit,
                ) {
                    next.push(candidate);
                }
                if candidates.len() >= options.candidate_limit {
                    break;
                }
            }
            if candidates.len() >= options.candidate_limit {
                break;
            }
        }
        frontier = next;
        if frontier.is_empty() {
            break;
        }
    }

    // 被リンクは「本文で明示された出リンク」より弱い候補として最後に置く。
    if options.include_incoming && candidates.len() < options.candidate_limit {
        for seed in &seeds {
            for id in incoming_ids(conn, &seed.id)? {
                push_candidate(
                    &mut candidates,
                    &mut seen,
                    Candidate {
                        id,
                        source: RetrievalSource::IncomingLink,
                        depth: 1,
                        seed: seed.id.clone(),
                    },
                    options.candidate_limit,
                );
                if candidates.len() >= options.candidate_limit {
                    break;
                }
            }
            if candidates.len() >= options.candidate_limit {
                break;
            }
        }
    }

    let candidate_count = candidates.len();
    let mut documents = Vec::new();
    let mut candidate_summaries = Vec::with_capacity(candidate_count);
    let mut total_tokens = 0usize;
    let mut skipped_for_budget = 0usize;
    let mut missing_documents = 0usize;
    let mut document_cap_reached = false;

    for candidate in candidates {
        let row = conn
            .query_row(
                "SELECT title, document, note_uid, namespace, authority_role, authority_status,
                        authority_scope
                 FROM notes WHERE id = ?1 AND status != 'deprecated'
                   AND normal_reference_allowed = 1",
                [&candidate.id],
                |row| {
                    Ok(CandidateRow {
                        title: row.get(0)?,
                        document: row.get(1)?,
                        note_uid: row.get(2)?,
                        namespace: row.get(3)?,
                        authority_role: row.get(4)?,
                        authority_status: row.get(5)?,
                        authority_scope: row.get(6)?,
                    })
                },
            )
            .optional()?;
        let mut summary = RetrievalCandidate {
            id: candidate.id.clone(),
            title: None,
            source: candidate.source,
            depth: candidate.depth,
            seed: candidate.seed.clone(),
            selected: false,
            omitted_reason: None,
            namespace: None,
            authority_role: None,
            authority_status: None,
            authority_scope: None,
        };
        // 呼出元がsnapshotを持たない場合の途中更新でも、非表示IDを省略候補へ出さない。
        let Some(row) = row else {
            continue;
        };
        if row.document.is_empty() {
            missing_documents += 1;
            summary.omitted_reason = Some("missing_document");
            candidate_summaries.push(summary);
            continue;
        }
        summary.title = row.title;
        let note_uid = row.note_uid;
        summary.namespace = row.namespace;
        summary.authority_role = row.authority_role;
        summary.authority_status = row.authority_status;
        summary.authority_scope = row.authority_scope;
        if documents.len() >= options.document_limit {
            document_cap_reached = true;
            summary.omitted_reason = Some("document_limit");
            candidate_summaries.push(summary);
            continue;
        }
        let text = query
            .filter(|query| !query.trim().is_empty())
            .map(|query| rank_document_passages(&row.document, query, options.passage))
            .unwrap_or(row.document);
        let tokens = estimate_tokens(&text);
        // 最上位seedが巨大でも空応答にはしない。Codex hookのspillが最後の安全網になる。
        if !documents.is_empty()
            && total_tokens.saturating_add(tokens) > options.estimated_token_budget
        {
            skipped_for_budget += 1;
            summary.omitted_reason = Some("token_budget");
            candidate_summaries.push(summary);
            continue;
        }
        total_tokens = total_tokens.saturating_add(tokens);
        documents.push(RetrievalDocument {
            id: candidate.id,
            text,
            source: candidate.source,
            depth: candidate.depth,
            seed: candidate.seed,
            estimated_tokens: tokens,
            provenance_line: provenance_line(conn, &summary.id, note_uid.as_deref())?,
        });
        summary.selected = true;
        candidate_summaries.push(summary);
    }

    let selected_depth_0 = documents.iter().filter(|doc| doc.depth == 0).count();
    let selected_depth_1 = documents
        .iter()
        .filter(|doc| doc.depth == 1 && doc.source != RetrievalSource::IncomingLink)
        .count();
    let selected_depth_2 = documents.iter().filter(|doc| doc.depth == 2).count();
    let selected_incoming = documents
        .iter()
        .filter(|doc| doc.source == RetrievalSource::IncomingLink)
        .count();

    let judgment_context = if include_judgment && options.document_limit > 0 {
        let ids = candidate_summaries
            .iter()
            .map(|candidate| candidate.id.clone())
            .collect::<Vec<_>>();
        let context = crate::judgment_context::context_for_notes(conn, &ids, context_scope)?;
        context.has_material().then_some(context)
    } else {
        None
    };

    Ok(RetrievalBundle {
        stats: RetrievalStats {
            seed_count,
            candidate_count,
            candidate_limit: options.candidate_limit,
            selected_count: documents.len(),
            document_limit: options.document_limit,
            estimated_tokens: total_tokens,
            estimated_token_budget: options.estimated_token_budget,
            selected_depth_0,
            selected_depth_1,
            selected_depth_2,
            selected_incoming,
            skipped_for_budget,
            missing_documents,
            unselected_count: candidate_count.saturating_sub(documents.len()),
            budget_exhausted: skipped_for_budget > 0
                || total_tokens > options.estimated_token_budget,
            document_cap_reached,
            candidate_cap_reached: candidate_count >= options.candidate_limit,
            elapsed_us: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
        },
        documents,
        candidates: candidate_summaries,
        judgment_context,
    })
}

#[derive(Debug)]
struct RankedPassage {
    index: usize,
    text: String,
    exact_query: bool,
    matched_terms: usize,
}

/// 予算内へ選んだ本文にだけ来歴の1行を付ける。イベントが無いノート(移行前・
/// backfill未実施)は None を返し、「記録なし」という文言でトークンを使わない。
fn provenance_line(
    conn: &Connection,
    note_id: &str,
    note_uid: Option<&str>,
) -> Result<Option<String>> {
    let events = crate::provenance::events_for_note(conn, note_id, note_uid, usize::MAX)?;
    if events.is_empty() {
        return Ok(None);
    }
    Ok(Some(crate::provenance::provenance_line(
        &crate::provenance::summarize(&events),
    )))
}

fn rank_document_passages(document: &str, query: &str, policy: PassagePolicy) -> String {
    if estimate_tokens(document) <= policy.trigger_tokens {
        return document.to_string();
    }
    let Ok(mut note) = crate::frontmatter::Note::parse(document) else {
        return document.to_string();
    };
    let query_lower = query.to_lowercase();
    let terms = passage_query_terms(query);
    let mut seen = HashSet::new();
    let mut passages = split_markdown_passages(&note.body, policy.max_bytes)
        .into_iter()
        .enumerate()
        .filter_map(|(index, text)| {
            let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if normalized.is_empty() || !seen.insert(normalized) {
                return None;
            }
            let lower = text.to_lowercase();
            Some(RankedPassage {
                index,
                exact_query: lower.contains(&query_lower),
                matched_terms: terms.iter().filter(|term| lower.contains(*term)).count(),
                text,
            })
        })
        .collect::<Vec<_>>();
    if passages.is_empty() {
        return document.to_string();
    }
    passages.sort_by_key(|passage| {
        (
            !passage.exact_query,
            std::cmp::Reverse(passage.matched_terms),
            passage.index,
        )
    });

    let has_match = passages.iter().any(|passage| passage.matched_terms > 0);
    let mut selected = Vec::new();
    let mut selected_tokens = 0usize;
    for passage in passages {
        if selected.len() >= policy.document_limit || (has_match && passage.matched_terms == 0) {
            continue;
        }
        let tokens = estimate_tokens(&passage.text);
        if !selected.is_empty()
            && selected_tokens.saturating_add(tokens) > policy.document_token_budget
        {
            continue;
        }
        selected_tokens = selected_tokens.saturating_add(tokens);
        selected.push(passage);
    }
    selected.sort_by_key(|passage| passage.index);
    note.body = format!(
        "<!-- kb-app: query-ranked passages; omitted unrelated sections -->\n\n{}",
        selected
            .into_iter()
            .map(|passage| passage.text.trim().to_string())
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    note.to_file_string()
        .unwrap_or_else(|_| document.to_string())
}

fn passage_query_terms(query: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    crate::tokenize::wakati(query)
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|term| {
            term.chars().any(char::is_alphanumeric)
                && (term.chars().count() >= 2 || term.is_ascii())
                && seen.insert(term.clone())
        })
        .collect()
}

fn split_markdown_passages(body: &str, max_bytes: usize) -> Vec<String> {
    let mut sections = Vec::new();
    let mut current = String::new();
    for line in body.split_inclusive('\n') {
        if is_markdown_heading(line) && !current.trim().is_empty() {
            sections.push(std::mem::take(&mut current));
        }
        current.push_str(line);
    }
    if !current.trim().is_empty() {
        sections.push(current);
    }

    sections
        .into_iter()
        .flat_map(|section| split_oversized_passage(&section, max_bytes))
        .collect()
}

fn is_markdown_heading(line: &str) -> bool {
    let trimmed = line.trim_start();
    let marks = trimmed
        .chars()
        .take_while(|character| *character == '#')
        .count();
    (1..=6).contains(&marks) && trimmed.chars().nth(marks).is_some_and(char::is_whitespace)
}

fn split_oversized_passage(section: &str, max_bytes: usize) -> Vec<String> {
    let (heading, mut remaining) = section
        .split_once('\n')
        .filter(|(first, _)| is_markdown_heading(first))
        .map(|(first, rest)| (Some(first.trim_end()), rest))
        .unwrap_or((None, section));
    let prefix_bytes = heading.map_or(0, |value| value.len() + 2);
    let content_limit = max_bytes.saturating_sub(prefix_bytes).max(64);
    let mut chunks = Vec::new();
    while !remaining.trim().is_empty() {
        let end = passage_boundary(remaining, content_limit);
        let content = remaining[..end].trim();
        if !content.is_empty() {
            chunks.push(match heading {
                Some(heading) => format!("{heading}\n\n{content}"),
                None => content.to_string(),
            });
        }
        remaining = remaining[end..].trim_start();
    }
    chunks
}

fn passage_boundary(value: &str, max_bytes: usize) -> usize {
    if value.len() <= max_bytes {
        return value.len();
    }
    let mut end = max_bytes.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    let prefix = &value[..end];
    ["\n\n", "\n", "。", ". "]
        .into_iter()
        .filter_map(|delimiter| {
            prefix
                .rfind(delimiter)
                .map(|position| position + delimiter.len())
        })
        .filter(|position| *position >= end / 2)
        .max()
        .unwrap_or(end)
}

/// Codex tokenizerはkb-appの必須依存にしない。UTF-8 2 bytes/tokenを保守的な近似とし、
/// 英文では多め、日本語では概ね同程度〜やや多めに見積もる。
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(2)
}

fn push_candidate(
    candidates: &mut Vec<Candidate>,
    seen: &mut HashSet<String>,
    candidate: Candidate,
    limit: usize,
) -> bool {
    if candidates.len() >= limit || !seen.insert(candidate.id.clone()) {
        return false;
    }
    candidates.push(candidate);
    true
}

fn outgoing_ids(conn: &Connection, id: &str) -> Result<Vec<String>> {
    let mut statement = conn.prepare_cached(
        "SELECT other FROM (
             SELECT l.dst AS other, 1 AS edge_priority FROM links l
             JOIN notes n ON n.id = l.dst
             JOIN notes root ON root.id = l.src
             WHERE l.src = ?1 AND n.status != 'deprecated' AND n.normal_reference_allowed = 1
               AND root.status != 'deprecated' AND root.normal_reference_allowed = 1
             UNION ALL
             SELECT target.id AS other,
                    CASE relation.kind
                        WHEN 'derived_from' THEN 0
                        WHEN 'supports' THEN 0
                        WHEN 'updates' THEN 0
                        WHEN 'contradicts' THEN 0
                        WHEN 'supersedes' THEN 0
                        WHEN 'mentions' THEN 2
                        ELSE 1
                    END AS edge_priority
             FROM notes source
             JOIN note_relations relation ON relation.src_uid = source.note_uid
             JOIN notes target ON target.note_uid = relation.target_uid
             WHERE source.id = ?1 AND target.status != 'deprecated'
               AND target.normal_reference_allowed = 1
               AND source.status != 'deprecated' AND source.normal_reference_allowed = 1
         ) GROUP BY other ORDER BY MIN(edge_priority), other",
    )?;
    let rows = statement.query_map([id], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn incoming_ids(conn: &Connection, id: &str) -> Result<Vec<String>> {
    let mut statement = conn.prepare_cached(
        "SELECT other FROM (
             SELECT l.src AS other, 1 AS edge_priority FROM links l
             JOIN notes n ON n.id = l.src
             JOIN notes root ON root.id = l.dst
             WHERE l.dst = ?1 AND n.status != 'deprecated' AND n.normal_reference_allowed = 1
               AND root.status != 'deprecated' AND root.normal_reference_allowed = 1
             UNION ALL
             SELECT source.id AS other,
                    CASE relation.kind
                        WHEN 'derived_from' THEN 0
                        WHEN 'supports' THEN 0
                        WHEN 'updates' THEN 0
                        WHEN 'contradicts' THEN 0
                        WHEN 'supersedes' THEN 0
                        WHEN 'mentions' THEN 2
                        ELSE 1
                    END AS edge_priority
             FROM notes target
             JOIN note_relations relation ON relation.target_uid = target.note_uid
             JOIN notes source ON source.note_uid = relation.src_uid
             WHERE target.id = ?1 AND source.status != 'deprecated'
               AND source.normal_reference_allowed = 1
               AND target.status != 'deprecated' AND target.normal_reference_allowed = 1
         ) GROUP BY other ORDER BY MIN(edge_priority), other",
    )?;
    let rows = statement.query_map([id], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE notes(
                 id TEXT PRIMARY KEY,
                 note_uid TEXT,
                 title TEXT,
                 status TEXT NOT NULL,
                 document TEXT NOT NULL,
                 namespace TEXT,
                 authority_role TEXT,
                 authority_status TEXT,
                 authority_scope TEXT,
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

    fn add_note(conn: &Connection, id: &str, text: &str) {
        conn.execute(
            "INSERT INTO notes(id, title, status, document, normal_reference_allowed) VALUES (?1, ?1, 'stable', ?2, 1)",
            rusqlite::params![id, text],
        )
        .unwrap();
    }

    fn add_note_with_uid(conn: &Connection, id: &str, uid: &str) {
        conn.execute(
            "INSERT INTO notes(id, note_uid, title, status, document, normal_reference_allowed)
             VALUES (?1, ?2, ?1, 'stable', ?1, 1)",
            rusqlite::params![id, uid],
        )
        .unwrap();
    }

    fn add_event(conn: &Connection, note_id: &str, note_uid: Option<&str>, at: &str, kind: &str) {
        let payload = serde_json::json!({
            "v": 1,
            "event_id": format!("event:{note_id}:{at}"),
            "note_uid": note_uid,
            "note_id": note_id,
            "at": at,
            "operation": if kind == "create" { "propose" } else { "update" },
            "actor": {
                "client": "claude-code",
                "client_basis": "handshake",
                "model": "claude-fable-5-1",
                "model_basis": "self_reported"
            },
            "kind": kind,
        });
        conn.execute(
            "INSERT INTO note_events(
                 event_id, note_uid, note_id, at, operation, actor_client, actor_surface,
                 actor_model, actor_model_basis, kind, payload
             ) VALUES(?1, ?2, ?3, ?4, 'propose', 'claude-code', 'unknown',
                      'claude-fable-5-1', 'self_reported', ?5, ?6)",
            rusqlite::params![
                format!("event:{note_id}:{at}"),
                note_uid,
                note_id,
                at,
                kind,
                payload.to_string()
            ],
        )
        .unwrap();
    }

    /// 予算内へ選んだ本文にだけ来歴の1行を付ける。記録の無いノートは行を持たない
    /// (「記録なし」でtokenを使わない)。本文自体は変えない。
    #[test]
    fn selected_documents_carry_a_provenance_line_without_touching_the_body() {
        let conn = setup();
        add_note_with_uid(&conn, "with-events", "uid-1");
        add_note(&conn, "no-events", "本文だけ");
        add_event(
            &conn,
            "with-events",
            Some("uid-1"),
            "2026-09-01T00:00:00Z",
            "create",
        );
        // 改名前のnote_idで積んだイベントも、安定uidで今のノートへ結び付く。
        add_event(
            &conn,
            "old-name",
            Some("uid-1"),
            "2026-09-02T00:00:00Z",
            "amend",
        );

        let bundle = context_documents(
            &conn,
            &["with-events".into(), "no-events".into()],
            RetrievalOptions::default(),
        )
        .unwrap();
        let line = bundle.documents[0]
            .provenance_line
            .as_deref()
            .expect("イベントのあるノートに来歴行がない");
        assert!(
            line.starts_with("来歴: 作成 2026-09-01 claude-code/claude-fable-5-1(自己申告)"),
            "{line}"
        );
        assert!(line.contains("更新1回"), "{line}");
        assert_eq!(bundle.documents[0].text, "with-events");
        assert_eq!(bundle.documents[1].provenance_line, None);
    }

    #[test]
    fn missing_optional_judgment_relations_can_fall_back_to_the_original_document() {
        use crate::authority::{Authority, AuthorityRole, AuthorityStatus, NoteNamespace, NoteUid};
        use crate::frontmatter::{Frontmatter, Note};
        let conn = setup();
        let mut front = Frontmatter::new_note("退避対象");
        front.note_uid = Some(NoteUid::new());
        front.authority = Some(Authority {
            namespace: NoteNamespace::Knowledge,
            role: AuthorityRole::Canonical,
            status: AuthorityStatus::Active,
            scope: "fixture/fallback".into(),
        });
        let text = Note {
            front,
            body: "元の本文を返す".into(),
        }
        .to_file_string()
        .unwrap();
        add_note(&conn, "notes/source", &text);
        conn.execute_batch("DROP TABLE note_relations").unwrap();
        let options = RetrievalOptions {
            max_depth: 0,
            include_incoming: false,
            ..RetrievalOptions::default()
        };
        let ids = ["notes/source".to_string()];
        assert!(context_documents_for_query(&conn, &ids, "本文", options).is_err());
        let fallback =
            context_documents_for_query_without_judgment(&conn, &ids, "本文", options).unwrap();
        assert_eq!(fallback.documents[0].text, text);
        assert!(fallback.judgment_context.is_none());
    }

    #[test]
    fn seeds_then_outgoing_hops_then_incoming_are_selected_deterministically() {
        let conn = setup();
        for id in ["seed", "search-2", "out-a", "out-b", "deep", "incoming"] {
            add_note(&conn, id, id);
        }
        conn.execute_batch(
            "INSERT INTO links VALUES ('seed', 'out-b');
             INSERT INTO links VALUES ('seed', 'out-a');
             INSERT INTO links VALUES ('out-a', 'deep');
             INSERT INTO links VALUES ('deep', 'seed');
             INSERT INTO links VALUES ('incoming', 'seed');",
        )
        .unwrap();

        let bundle = context_documents(
            &conn,
            &["seed".into(), "search-2".into()],
            RetrievalOptions {
                estimated_token_budget: 1_000,
                ..RetrievalOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            bundle
                .documents
                .iter()
                .map(|document| document.id.as_str())
                .collect::<Vec<_>>(),
            ["seed", "search-2", "out-a", "out-b", "deep", "incoming"]
        );
        assert_eq!(bundle.documents[2].source, RetrievalSource::OutgoingLink);
        assert_eq!(bundle.documents[4].depth, 2);
        assert_eq!(bundle.documents[5].source, RetrievalSource::IncomingLink);
        assert_eq!(bundle.stats.selected_depth_0, 2);
        assert_eq!(bundle.stats.selected_depth_1, 2);
        assert_eq!(bundle.stats.selected_depth_2, 1);
        assert_eq!(bundle.stats.selected_incoming, 1);
    }

    /// 2026-09-06: 検索を迂回した既知IDも、本文だけでなく候補案内から除外する。
    #[test]
    fn hidden_seeds_do_not_consume_limits_or_appear_in_candidate_metadata() {
        let conn = setup();
        add_note(&conn, "hidden", "非表示票の本文");
        add_note(&conn, "visible", "通常参照本文");
        conn.execute(
            "UPDATE notes SET normal_reference_allowed = 0 WHERE id = 'hidden'",
            [],
        )
        .unwrap();
        let options = RetrievalOptions {
            seed_limit: 1,
            candidate_limit: 1,
            document_limit: 1,
            max_depth: 0,
            include_incoming: false,
            ..RetrievalOptions::default()
        };
        let hidden = context_documents(&conn, &["hidden".into()], options).unwrap();
        assert!(hidden.documents.is_empty());
        assert!(hidden.candidates.is_empty());
        assert_eq!(hidden.stats.seed_count, 0);
        assert_eq!(hidden.stats.candidate_count, 0);
        let visible = context_documents_for_query(
            &conn,
            &["hidden".into(), "missing".into(), "visible".into()],
            "本文",
            options,
        )
        .unwrap();
        assert_eq!(visible.documents.len(), 1);
        assert_eq!(visible.documents[0].id, "visible");
        assert_eq!(visible.candidates.len(), 1);
        assert_eq!(visible.candidates[0].id, "visible");
    }

    /// 2026-09-06: 非表示票をリンクの中継点にしても、候補・本文・探索枠へ混ぜない。
    #[test]
    fn hidden_link_endpoints_and_bridges_are_filtered_before_candidate_limits() {
        let conn = setup();
        for id in [
            "seed",
            "hidden-out",
            "hidden-in",
            "hidden-typed",
            "bridge-target",
            "visible",
        ] {
            add_note_with_uid(&conn, id, &format!("uid-{id}"));
        }
        conn.execute_batch(
            "UPDATE notes SET normal_reference_allowed = 0 WHERE id LIKE 'hidden-%';
             INSERT INTO links VALUES ('seed','hidden-out');
             INSERT INTO links VALUES ('hidden-out','bridge-target');
             INSERT INTO links VALUES ('hidden-in','seed');
             INSERT INTO links VALUES ('seed','visible');
             INSERT INTO note_relations VALUES ('uid-seed','supports','uid-hidden-typed');
             INSERT INTO note_relations VALUES ('uid-hidden-typed','supports','uid-seed');
             INSERT INTO note_relations VALUES ('uid-hidden-typed','supports','uid-bridge-target');",
        )
        .unwrap();
        assert_eq!(outgoing_ids(&conn, "seed").unwrap(), ["visible"]);
        assert!(incoming_ids(&conn, "seed").unwrap().is_empty());
        for id in ["hidden-out", "hidden-in", "hidden-typed"] {
            assert!(outgoing_ids(&conn, id).unwrap().is_empty());
            assert!(incoming_ids(&conn, id).unwrap().is_empty());
        }
        let bundle = context_documents(
            &conn,
            &["seed".into()],
            RetrievalOptions {
                candidate_limit: 2,
                document_limit: 2,
                ..RetrievalOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            bundle
                .documents
                .iter()
                .map(|doc| doc.id.as_str())
                .collect::<Vec<_>>(),
            ["seed", "visible"]
        );
        assert_eq!(
            bundle
                .candidates
                .iter()
                .map(|doc| doc.id.as_str())
                .collect::<Vec<_>>(),
            ["seed", "visible"]
        );
    }

    #[test]
    fn token_budget_skips_large_candidate_and_keeps_later_short_candidate() {
        let conn = setup();
        add_note(&conn, "seed", "123456"); // 3 tokens
        add_note(&conn, "large", "1234567890"); // 5 tokens
        add_note(&conn, "short", "12"); // 1 token

        let bundle = context_documents(
            &conn,
            &["seed".into(), "large".into(), "short".into()],
            RetrievalOptions {
                estimated_token_budget: 4,
                include_incoming: false,
                ..RetrievalOptions::default()
            },
        )
        .unwrap();

        assert_eq!(
            bundle
                .documents
                .iter()
                .map(|document| document.id.as_str())
                .collect::<Vec<_>>(),
            ["seed", "short"]
        );
        assert_eq!(bundle.stats.estimated_tokens, 4);
        assert_eq!(bundle.stats.skipped_for_budget, 1);
        assert!(bundle.stats.budget_exhausted);
        assert_eq!(bundle.stats.unselected_count, 1);
        assert_eq!(bundle.candidates[1].omitted_reason, Some("token_budget"));
    }

    #[test]
    fn deprecated_links_cycles_and_duplicate_paths_do_not_expand() {
        let conn = setup();
        for id in ["seed", "alive", "shared"] {
            add_note(&conn, id, id);
        }
        conn.execute(
            "INSERT INTO notes(id, title, status, document, normal_reference_allowed) VALUES ('old', 'old', 'deprecated', 'old', 1)",
            [],
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO links VALUES ('seed', 'alive');
             INSERT INTO links VALUES ('seed', 'old');
             INSERT INTO links VALUES ('alive', 'seed');
             INSERT INTO links VALUES ('alive', 'shared');
             INSERT INTO links VALUES ('seed', 'shared');",
        )
        .unwrap();

        let bundle = context_documents(
            &conn,
            &["seed".into()],
            RetrievalOptions {
                include_incoming: false,
                ..RetrievalOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            bundle
                .documents
                .iter()
                .map(|document| document.id.as_str())
                .collect::<Vec<_>>(),
            ["seed", "alive", "shared"]
        );
    }

    #[test]
    fn explicit_typed_relations_rank_before_links_and_mentions() {
        let conn = setup();
        for (id, uid) in [
            ("seed", "uid-seed"),
            ("zeta-evidence", "uid-evidence"),
            ("middle-link", "uid-link"),
            ("aster-mention", "uid-mention"),
        ] {
            add_note_with_uid(&conn, id, uid);
        }
        conn.execute_batch(
            "INSERT INTO links VALUES ('seed', 'middle-link');
             INSERT INTO note_relations VALUES ('uid-seed', 'mentions', 'uid-mention');
             INSERT INTO note_relations VALUES ('uid-seed', 'supports', 'uid-evidence');",
        )
        .unwrap();

        assert_eq!(
            outgoing_ids(&conn, "seed").unwrap(),
            ["zeta-evidence", "middle-link", "aster-mention"]
        );
        let bundle = context_documents(
            &conn,
            &["seed".into()],
            RetrievalOptions {
                document_limit: 3,
                include_incoming: false,
                ..RetrievalOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            bundle
                .documents
                .iter()
                .map(|document| document.id.as_str())
                .collect::<Vec<_>>(),
            ["seed", "zeta-evidence", "middle-link"]
        );
    }

    #[test]
    fn long_markdown_keeps_only_query_ranked_passages() {
        let note = crate::frontmatter::Note {
            front: crate::frontmatter::Frontmatter::new_note("Atlas Recovery Handbook"),
            body: format!(
                "# Routine maintenance\n\n{}\n\n# Checksum recovery procedure\n\n\
                 Rotate the key after a checksum mismatch.\n\n# Appendix\n\n{}",
                "Routine filler with no incident action. ".repeat(300),
                "Unrelated appendix material. ".repeat(300),
            ),
        };
        let document = note.to_file_string().unwrap();
        assert!(estimate_tokens(&document) > PassagePolicy::default().trigger_tokens);

        let ranked = rank_document_passages(
            &document,
            "atlas recovery checksum procedure",
            PassagePolicy::default(),
        );

        assert!(ranked.contains("Rotate the key after a checksum mismatch"));
        assert!(ranked.contains("Checksum recovery procedure"));
        assert!(!ranked.contains("Unrelated appendix material"));
        assert!(estimate_tokens(&ranked) < 1_000);
    }

    #[test]
    fn repeated_unheaded_passages_are_deduplicated() {
        let note = crate::frontmatter::Note {
            front: crate::frontmatter::Frontmatter::new_note("Atlas Recovery Handbook"),
            body: "atlas recovery checksum procedure. Rotate the key after a checksum mismatch.\n"
                .repeat(350),
        };
        let document = note.to_file_string().unwrap();

        let ranked = rank_document_passages(
            &document,
            "atlas recovery checksum procedure",
            PassagePolicy::default(),
        );

        assert!(ranked.contains("Rotate the key after a checksum mismatch"));
        assert!(estimate_tokens(&ranked) < 4_000);
    }

    /// 配信 profile の passage 予算(`session_explicit` は 2 passage / 2,400 token)が
    /// 既定(3 / 3,600)より本文を狭め、required の passage は落とさないことを固定する。
    #[test]
    fn passage_policy_narrows_the_per_document_budget_without_dropping_the_match() {
        let filler = "Alpha checksum step. ".repeat(120);
        let note = crate::frontmatter::Note {
            front: crate::frontmatter::Frontmatter::new_note("Atlas Recovery Handbook"),
            body: format!(
                "# Checksum recovery procedure\n\nRotate the key after a checksum mismatch.\n\n\
                 # Checksum audit one\n\n{filler}\n\n# Checksum audit two\n\n{filler}\n\n\
                 # Checksum audit three\n\n{filler}\n\n# Appendix\n\n{}",
                "Unrelated appendix material. ".repeat(400),
            ),
        };
        let document = note.to_file_string().unwrap();
        let query = "checksum recovery procedure";
        let default_policy = PassagePolicy::default();
        let explicit_policy = crate::retrieval_profile::RetrievalProfile::SessionExplicit
            .plan()
            .retrieval
            .passage;
        assert!(estimate_tokens(&document) > explicit_policy.trigger_tokens);

        let default_ranked = rank_document_passages(&document, query, default_policy);
        let explicit_ranked = rank_document_passages(&document, query, explicit_policy);

        assert!(default_ranked.contains("Rotate the key after a checksum mismatch"));
        assert!(explicit_ranked.contains("Rotate the key after a checksum mismatch"));
        assert_eq!(default_ranked.matches("# Checksum").count(), 3);
        assert_eq!(explicit_ranked.matches("# Checksum").count(), 2);
        assert!(estimate_tokens(&explicit_ranked) < estimate_tokens(&default_ranked));
        assert!(!explicit_ranked.contains("Unrelated appendix material"));
    }

    /// card-lite の素材: 候補一覧は本文を読まずに authority を持つ。envelope の無い
    /// legacy note では列を出さない。
    #[test]
    fn candidates_carry_authority_for_card_lite_output() {
        let conn = setup();
        add_note(&conn, "legacy", "legacy body");
        conn.execute(
            "INSERT INTO notes(id, title, status, document, namespace, authority_role,
                               authority_status, authority_scope, normal_reference_allowed)
             VALUES ('canon', 'canon', 'stable', 'canon body', 'decisions', 'canonical',
                     'active', 'atlas/recovery', 1)",
            [],
        )
        .unwrap();
        conn.execute_batch("INSERT INTO links VALUES ('legacy', 'canon');")
            .unwrap();

        let bundle = context_documents(
            &conn,
            &["legacy".into()],
            RetrievalOptions {
                include_incoming: false,
                ..RetrievalOptions::default()
            },
        )
        .unwrap();

        let legacy = &bundle.candidates[0];
        assert_eq!(legacy.id, "legacy");
        assert_eq!(legacy.namespace, None);
        assert_eq!(legacy.authority_role, None);
        let canon = &bundle.candidates[1];
        assert_eq!(canon.id, "canon");
        assert_eq!(canon.namespace.as_deref(), Some("decisions"));
        assert_eq!(canon.authority_role.as_deref(), Some("canonical"));
        assert_eq!(canon.authority_status.as_deref(), Some("active"));
        assert_eq!(canon.authority_scope.as_deref(), Some("atlas/recovery"));

        let serialized = serde_json::to_value(&bundle.candidates).unwrap();
        assert!(serialized[0].get("namespace").is_none());
        assert_eq!(serialized[1]["authority_scope"], "atlas/recovery");
    }

    #[test]
    fn candidate_and_document_caps_are_independent() {
        let conn = setup();
        for id in ["seed", "a", "b", "c", "d", "e"] {
            add_note(&conn, id, id);
        }
        conn.execute_batch(
            "INSERT INTO links VALUES ('seed', 'a');
             INSERT INTO links VALUES ('seed', 'b');
             INSERT INTO links VALUES ('seed', 'c');
             INSERT INTO links VALUES ('seed', 'd');
             INSERT INTO links VALUES ('seed', 'e');",
        )
        .unwrap();

        let bundle = context_documents(
            &conn,
            &["seed".into()],
            RetrievalOptions {
                candidate_limit: 4,
                document_limit: 2,
                estimated_token_budget: 1_000,
                include_incoming: false,
                ..RetrievalOptions::default()
            },
        )
        .unwrap();

        assert_eq!(
            bundle
                .candidates
                .iter()
                .map(|candidate| candidate.id.as_str())
                .collect::<Vec<_>>(),
            ["seed", "a", "b", "c"]
        );
        assert_eq!(
            bundle
                .documents
                .iter()
                .map(|document| document.id.as_str())
                .collect::<Vec<_>>(),
            ["seed", "a"]
        );
        assert!(bundle.stats.candidate_cap_reached);
        assert!(bundle.stats.document_cap_reached);
        assert_eq!(bundle.stats.unselected_count, 2);
        assert_eq!(bundle.candidates[2].omitted_reason, Some("document_limit"));
    }
}
