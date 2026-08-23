//! 自動retrieval向けの候補展開と本文予算選択。
//!
//! 検索順位だけで固定件数を返さず、上位seedからDB内の有向リンクを辿って候補を広げる。
//! 探索候補数とモデル可視本文量は別々に制限し、数千〜10,000ノートでもグラフ全体や
//! 全文をコンテキストへ流さない。

use std::collections::HashSet;
use std::time::Instant;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

pub const AUTO_SEED_LIMIT: usize = 5;
pub const AUTO_MAX_DEPTH: u8 = 2;
pub const AUTO_CANDIDATE_LIMIT: usize = 50;
pub const AUTO_DOCUMENT_LIMIT: usize = 10;
/// Codex hook側の約12,000 token spill閾値へ、見出し等の余白を残す。
pub const AUTO_ESTIMATED_TOKEN_BUDGET: usize = 10_000;
const PASSAGE_RANKING_TRIGGER_TOKENS: usize = 4_000;
const PASSAGE_MAX_BYTES: usize = 2_400;
const PASSAGE_DOCUMENT_LIMIT: usize = 3;
const PASSAGE_DOCUMENT_TOKEN_BUDGET: usize = 3_600;

#[derive(Clone, Copy, Debug)]
pub struct RetrievalOptions {
    pub seed_limit: usize,
    pub max_depth: u8,
    pub candidate_limit: usize,
    pub document_limit: usize,
    pub estimated_token_budget: usize,
    pub include_incoming: bool,
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
}

#[derive(Clone, Debug)]
struct Candidate {
    id: String,
    source: RetrievalSource,
    depth: u8,
    seed: String,
}

/// 検索上位seed → 出リンク最大2ホップ → seedへの被リンク、の順に候補化し、
/// 総量予算内の本文だけを同じDB snapshotから返す。
pub fn context_documents(
    conn: &Connection,
    ranked_hit_ids: &[String],
    options: RetrievalOptions,
) -> Result<RetrievalBundle> {
    context_documents_inner(conn, ranked_hit_ids, None, options)
}

/// 検索queryに合う見出し・passageだけへ長文ノートを縮約して返す本番retrieval経路。
pub fn context_documents_for_query(
    conn: &Connection,
    ranked_hit_ids: &[String],
    query: &str,
    options: RetrievalOptions,
) -> Result<RetrievalBundle> {
    context_documents_inner(conn, ranked_hit_ids, Some(query), options)
}

fn context_documents_inner(
    conn: &Connection,
    ranked_hit_ids: &[String],
    query: Option<&str>,
    options: RetrievalOptions,
) -> Result<RetrievalBundle> {
    let started = Instant::now();
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();

    for id in ranked_hit_ids.iter().take(options.seed_limit) {
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
                "SELECT title, document FROM notes WHERE id = ?1 AND status != 'deprecated'",
                [&candidate.id],
                |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((title, original_text)) = row.filter(|(_, text)| !text.is_empty()) else {
            missing_documents += 1;
            candidate_summaries.push(RetrievalCandidate {
                id: candidate.id,
                title: None,
                source: candidate.source,
                depth: candidate.depth,
                seed: candidate.seed,
                selected: false,
                omitted_reason: Some("missing_document"),
            });
            continue;
        };
        if documents.len() >= options.document_limit {
            document_cap_reached = true;
            candidate_summaries.push(RetrievalCandidate {
                id: candidate.id,
                title,
                source: candidate.source,
                depth: candidate.depth,
                seed: candidate.seed,
                selected: false,
                omitted_reason: Some("document_limit"),
            });
            continue;
        }
        let text = query
            .filter(|query| !query.trim().is_empty())
            .map(|query| rank_document_passages(&original_text, query))
            .unwrap_or(original_text);
        let tokens = estimate_tokens(&text);
        // 最上位seedが巨大でも空応答にはしない。Codex hookのspillが最後の安全網になる。
        if !documents.is_empty()
            && total_tokens.saturating_add(tokens) > options.estimated_token_budget
        {
            skipped_for_budget += 1;
            candidate_summaries.push(RetrievalCandidate {
                id: candidate.id,
                title,
                source: candidate.source,
                depth: candidate.depth,
                seed: candidate.seed,
                selected: false,
                omitted_reason: Some("token_budget"),
            });
            continue;
        }
        total_tokens = total_tokens.saturating_add(tokens);
        documents.push(RetrievalDocument {
            id: candidate.id.clone(),
            text,
            source: candidate.source,
            depth: candidate.depth,
            seed: candidate.seed.clone(),
            estimated_tokens: tokens,
        });
        candidate_summaries.push(RetrievalCandidate {
            id: candidate.id,
            title,
            source: candidate.source,
            depth: candidate.depth,
            seed: candidate.seed,
            selected: true,
            omitted_reason: None,
        });
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
    })
}

#[derive(Debug)]
struct RankedPassage {
    index: usize,
    text: String,
    exact_query: bool,
    matched_terms: usize,
}

fn rank_document_passages(document: &str, query: &str) -> String {
    if estimate_tokens(document) <= PASSAGE_RANKING_TRIGGER_TOKENS {
        return document.to_string();
    }
    let Ok(mut note) = crate::frontmatter::Note::parse(document) else {
        return document.to_string();
    };
    let query_lower = query.to_lowercase();
    let terms = passage_query_terms(query);
    let mut seen = HashSet::new();
    let mut passages = split_markdown_passages(&note.body)
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
        if selected.len() >= PASSAGE_DOCUMENT_LIMIT || (has_match && passage.matched_terms == 0) {
            continue;
        }
        let tokens = estimate_tokens(&passage.text);
        if !selected.is_empty()
            && selected_tokens.saturating_add(tokens) > PASSAGE_DOCUMENT_TOKEN_BUDGET
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

fn split_markdown_passages(body: &str) -> Vec<String> {
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
        .flat_map(|section| split_oversized_passage(&section))
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

fn split_oversized_passage(section: &str) -> Vec<String> {
    let (heading, mut remaining) = section
        .split_once('\n')
        .filter(|(first, _)| is_markdown_heading(first))
        .map(|(first, rest)| (Some(first.trim_end()), rest))
        .unwrap_or((None, section));
    let prefix_bytes = heading.map_or(0, |value| value.len() + 2);
    let content_limit = PASSAGE_MAX_BYTES.saturating_sub(prefix_bytes).max(64);
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
             WHERE l.src = ?1 AND n.status != 'deprecated'
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
             WHERE l.dst = ?1 AND n.status != 'deprecated'
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
                 document TEXT NOT NULL
             );
             CREATE TABLE links(src TEXT, dst TEXT, PRIMARY KEY(src, dst));
             CREATE INDEX links_dst ON links(dst);
             CREATE TABLE note_relations(
                 src_uid TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 target_uid TEXT NOT NULL,
                 PRIMARY KEY(src_uid, kind, target_uid)
             );
             CREATE INDEX note_relations_target ON note_relations(target_uid);",
        )
        .unwrap();
        conn
    }

    fn add_note(conn: &Connection, id: &str, text: &str) {
        conn.execute(
            "INSERT INTO notes(id, title, status, document) VALUES (?1, ?1, 'stable', ?2)",
            rusqlite::params![id, text],
        )
        .unwrap();
    }

    fn add_note_with_uid(conn: &Connection, id: &str, uid: &str) {
        conn.execute(
            "INSERT INTO notes(id, note_uid, title, status, document)
             VALUES (?1, ?2, ?1, 'stable', ?1)",
            rusqlite::params![id, uid],
        )
        .unwrap();
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
            "INSERT INTO notes(id, title, status, document) VALUES ('old', 'old', 'deprecated', 'old')",
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
        assert!(estimate_tokens(&document) > PASSAGE_RANKING_TRIGGER_TOKENS);

        let ranked = rank_document_passages(&document, "atlas recovery checksum procedure");

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

        let ranked = rank_document_passages(&document, "atlas recovery checksum procedure");

        assert!(ranked.contains("Rotate the key after a checksum mismatch"));
        assert!(estimate_tokens(&ranked) < 4_000);
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
