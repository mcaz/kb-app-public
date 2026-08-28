//! ハイブリッド検索(段0: FTS のみが正常形)。
//! 主経路 = lindera 分かち書き+bm25。レスキュー経路 = trigram / LIKE(部分語)。
//! 検索 API は Result で全滅させず「結果+劣化情報」を返す(fail-open を型で強制、原則4)。

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

use crate::degradation::Degradation;
use crate::tokenize::match_expr;

const FIELD_RANKING_CANDIDATE_MULTIPLIER: usize = 8;
const DIVERSITY_MAX_NORMALIZED_CHARS: usize = 2_048;
const DIVERSITY_MIN_SHINGLES: usize = 8;
const DIVERSITY_SIMILARITY_PERCENT: usize = 85;
const TITLE_TERM_WEIGHT: usize = 64;
const DESCRIPTION_TERM_WEIGHT: usize = 16;
const TAG_TERM_WEIGHT: usize = 12;
const SCOPE_TERM_WEIGHT: usize = 8;
const BODY_TERM_WEIGHT: usize = 1;

#[derive(Debug, Clone, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Hit {
    pub id: String,
    pub title: Option<String>,
    pub status: String,
    pub snippet: String,
    /// "main"(分かち書き bm25)/ "anchor"(リンク文言)/ "vec"(意味検索)/
    /// "rescue"(trigram/LIKE)と融合形
    pub via: &'static str,
    /// 意味検索のコサイン距離(関連判定は RRF でなく生距離で — 旧 KB の実測教訓)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance: Option<f32>,
    /// 所有(原則9): "human" | "agent"
    pub origin: Option<String>,
    /// 分類タグ(空白区切りを分解済み)
    pub tags: Vec<String>,
    /// 作成日時・最終更新(一覧でも見えるように)
    pub created: Option<String>,
    pub updated: Option<String>,
    pub note_uid: Option<String>,
    pub namespace: Option<String>,
    pub authority_role: Option<String>,
    pub authority_status: Option<String>,
    pub authority_scope: Option<String>,
}

impl Hit {
    fn authority_priority(&self) -> u8 {
        match (
            self.authority_role.as_deref(),
            self.authority_status.as_deref(),
        ) {
            (Some("canonical"), Some("active")) => 0,
            (None, None) => 1,
            (Some("proposal"), _) | (_, Some("superseded")) => 3,
            _ => 2,
        }
    }

    fn intent_alignment(&self, intent: QueryIntent) -> u8 {
        intent.alignment(AuthorityValues {
            namespace: self.namespace.as_deref(),
            role: self.authority_role.as_deref(),
            status: self.authority_status.as_deref(),
        })
    }

    fn anchor_matched(&self) -> bool {
        matches!(self.via, "anchor" | "main_anchor" | "both_anchor")
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SearchOutcome {
    pub hits: Vec<Hit>,
    /// 上位ヒットから1ホップのリンク先・被リンク(id, title)
    pub related: Vec<(String, Option<String>)>,
    /// 劣化情報(空 = 全経路正常)。UI/クライアントに必ず見せる
    pub degraded: Vec<Degradation>,
}

pub fn search(conn: &Connection, query: &str, limit: usize) -> SearchOutcome {
    search_mode(conn, query, limit, false)
}

/// `any = true` で語を OR 結合(フックの前出しなど、文まるごとを投げる用途。
/// bm25 が多く当たった文書を上位に出す)。false は従来どおり AND。
pub fn search_mode(conn: &Connection, query: &str, limit: usize, any: bool) -> SearchOutcome {
    let mut hits: Vec<Hit> = Vec::new();
    let mut degraded = Vec::new();
    let intent = QueryIntent::from_query(query);

    // 主経路: lindera 分かち書き + bm25
    match main_search(conn, query, limit, any) {
        Ok(main_hits) => hits.extend(main_hits),
        Err(error) => degraded.push(Degradation::MainSearch {
            detail: error.to_string(),
        }),
    }

    // リンク文言はリンク先自身に語が無い別名を拾うための弱い索引として扱う。
    match anchor_search(conn, query, limit) {
        Ok(anchor_hits) => merge_anchor_hits(&mut hits, anchor_hits),
        Err(error) => degraded.push(Degradation::AnchorSearch {
            detail: error.to_string(),
        }),
    }
    rank_hits(&mut hits, query, intent);
    hits.truncate(limit);

    // 意味検索(段1)。モデル未導入なら黙って全文のみ(段0 の正常形)。
    // 導入済みで失敗した場合は必ず劣化として見せる(沈黙停止の教訓)。
    match vec_search(conn, query, limit) {
        Ok(Some(vec_hits)) => hits = fuse(hits, vec_hits, limit),
        Ok(None) => {}
        Err(error) => degraded.push(Degradation::SemanticSearch {
            detail: error.to_string(),
        }),
    }

    // レスキュー経路: 主経路で拾えない部分語・未知語形(常に実行し、差分だけ足す)
    match rescue_search(conn, query, limit) {
        Ok(rescue_hits) => {
            for h in rescue_hits {
                if hits.len() >= limit {
                    break;
                }
                if !hits.iter().any(|x| x.id == h.id) {
                    hits.push(h);
                }
            }
        }
        Err(error) => degraded.push(Degradation::RescueSearch {
            detail: error.to_string(),
        }),
    }

    // 完全タイトル一致はlocatorとしての明示性が最も高い。明示的なquery intentがある場合だけ
    // authorityの既定順を上書きし、該当しない候補間ではactive canonicalを優先する。
    rank_hits(&mut hits, query, intent);
    match diversify_hits(conn, &hits, limit) {
        Ok(diverse_hits) => hits = diverse_hits,
        Err(error) => degraded.push(Degradation::DiversityRanking {
            detail: error.to_string(),
        }),
    }
    hits.truncate(limit);

    let related = match related_of(conn, hits.first().map(|hit| hit.id.as_str())) {
        Ok(related) => related,
        Err(error) => {
            degraded.push(Degradation::RelatedNotes {
                detail: error.to_string(),
            });
            Vec::new()
        }
    };
    SearchOutcome {
        hits,
        related,
        degraded,
    }
}

fn rank_hits(hits: &mut [Hit], query: &str, intent: QueryIntent) {
    hits.sort_by_key(|hit| {
        (
            !exact_title_match(query, hit.title.as_deref()),
            std::cmp::Reverse(hit.intent_alignment(intent)),
            !hit.anchor_matched(),
            hit.authority_priority(),
        )
    });
}

fn merge_anchor_hits(hits: &mut Vec<Hit>, anchor_hits: Vec<Hit>) {
    for anchor in anchor_hits {
        if let Some(existing) = hits.iter_mut().find(|hit| hit.id == anchor.id) {
            existing.via = match existing.via {
                "main" => "main_anchor",
                "both" => "both_anchor",
                other => other,
            };
        } else {
            hits.push(anchor);
        }
    }
}

fn main_search(conn: &Connection, query: &str, limit: usize, any: bool) -> Result<Vec<Hit>> {
    let expr = if any {
        crate::tokenize::match_expr_any(query)
    } else {
        match_expr(query)
    };
    if expr.is_empty() {
        return Ok(Vec::new());
    }
    let field_terms = field_terms(query);
    let intent = QueryIntent::from_query(query);
    // 再順位付け対象を最終件数より広く取る。8倍は10k fixtureでも最大40行に留まり、
    // 本文反復だけが強い候補の外からtitle一致を回収できる実測上の最小余裕。
    let candidate_limit = limit.saturating_mul(FIELD_RANKING_CANDIDATE_MULTIPLIER);
    let mut stmt = conn.prepare_cached(
        "SELECT f.id, n.title, n.status,
                snippet(fts_main, 1, '[', ']', '…', 12), n.origin, n.tags, n.created, n.generated_at,
                n.note_uid, n.namespace, n.authority_role, n.authority_status, n.authority_scope,
                n.description, n.body
         FROM fts_main f JOIN notes n ON n.id = f.id
         WHERE fts_main MATCH ?1 AND n.status != 'deprecated'
         ORDER BY rank LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![expr, candidate_limit as i64], |r| {
        let title = r.get::<_, Option<String>>(1)?;
        let tags = r.get::<_, Option<String>>(5)?;
        let namespace = r.get::<_, Option<String>>(9)?;
        let authority_scope = r.get::<_, Option<String>>(12)?;
        let description = r.get::<_, Option<String>>(13)?;
        let body = r.get::<_, String>(14)?;
        let authority_role = r.get::<_, Option<String>>(10)?;
        let authority_status = r.get::<_, Option<String>>(11)?;
        let field_score = field_score(
            query,
            &field_terms,
            FieldValues {
                title: title.as_deref(),
                description: description.as_deref(),
                tags: tags.as_deref(),
                namespace: namespace.as_deref(),
                scope: authority_scope.as_deref(),
                body: &body,
            },
        );
        let intent_alignment = intent.alignment(AuthorityValues {
            namespace: namespace.as_deref(),
            role: authority_role.as_deref(),
            status: authority_status.as_deref(),
        });
        let diversity = DiversitySignature::new(
            authority_scope.as_deref(),
            authority_role.as_deref(),
            authority_status.as_deref(),
            &body,
        );
        Ok((
            Hit {
                id: r.get(0)?,
                title,
                status: r.get(2)?,
                snippet: r.get(3)?,
                via: "main",
                distance: None,
                origin: r.get(4)?,
                tags: split_tags(tags),
                created: r.get(6)?,
                updated: r.get(7)?,
                note_uid: r.get(8)?,
                namespace,
                authority_role,
                authority_status,
                authority_scope,
            },
            RankingScore {
                exact_title: field_score.exact_title,
                intent_alignment,
                weighted_term_matches: field_score.weighted_term_matches,
            },
            diversity,
        ))
    })?;
    let mut candidates = rows
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .enumerate()
        .map(|(bm25_rank, (hit, field_score, diversity))| (hit, field_score, bm25_rank, diversity))
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| {
                left.0
                    .authority_priority()
                    .cmp(&right.0.authority_priority())
            })
            .then_with(|| left.2.cmp(&right.2))
    });
    Ok(diversify_main_candidates(candidates, limit))
}

#[derive(Debug)]
struct DiversitySignature {
    scope: Option<String>,
    role: Option<String>,
    status: Option<String>,
    normalized: String,
    trigrams: Vec<(char, char, char)>,
}

impl DiversitySignature {
    fn new(scope: Option<&str>, role: Option<&str>, status: Option<&str>, body: &str) -> Self {
        let normalized = body
            .chars()
            .flat_map(char::to_lowercase)
            .filter(|character| character.is_alphanumeric())
            .take(DIVERSITY_MAX_NORMALIZED_CHARS)
            .collect::<String>();
        let characters = normalized.chars().collect::<Vec<_>>();
        let mut trigrams = characters
            .windows(3)
            .map(|window| (window[0], window[1], window[2]))
            .collect::<Vec<_>>();
        trigrams.sort_unstable();
        trigrams.dedup();
        Self {
            scope: scope.map(str::to_string),
            role: role.map(str::to_string),
            status: status.map(str::to_string),
            normalized,
            trigrams,
        }
    }

    fn same_cluster(&self, other: &Self) -> bool {
        let same_authority_facet = self.role == other.role && self.status == other.status;
        if self.scope.is_some() && self.scope == other.scope && same_authority_facet {
            return true;
        }
        if !same_authority_facet {
            return false;
        }
        if !self.normalized.is_empty() && self.normalized == other.normalized {
            return true;
        }
        if self.trigrams.len() < DIVERSITY_MIN_SHINGLES
            || other.trigrams.len() < DIVERSITY_MIN_SHINGLES
        {
            return false;
        }
        let mut left = 0;
        let mut right = 0;
        let mut intersection = 0;
        while left < self.trigrams.len() && right < other.trigrams.len() {
            match self.trigrams[left].cmp(&other.trigrams[right]) {
                std::cmp::Ordering::Less => left += 1,
                std::cmp::Ordering::Greater => right += 1,
                std::cmp::Ordering::Equal => {
                    intersection += 1;
                    left += 1;
                    right += 1;
                }
            }
        }
        let union = self.trigrams.len() + other.trigrams.len() - intersection;
        intersection.saturating_mul(100) >= union.saturating_mul(DIVERSITY_SIMILARITY_PERCENT)
    }
}

fn diversify_main_candidates(
    candidates: Vec<(Hit, RankingScore, usize, DiversitySignature)>,
    limit: usize,
) -> Vec<Hit> {
    let mut selected: Vec<(Hit, DiversitySignature)> = Vec::new();
    for (hit, _, _, signature) in candidates {
        if selected
            .iter()
            .any(|(_, existing)| existing.same_cluster(&signature))
        {
            continue;
        }
        selected.push((hit, signature));
        if selected.len() >= limit {
            break;
        }
    }
    selected.into_iter().map(|(hit, _)| hit).collect()
}

fn diversify_hits(conn: &Connection, hits: &[Hit], limit: usize) -> Result<Vec<Hit>> {
    let mut selected: Vec<(Hit, DiversitySignature)> = Vec::new();
    for hit in hits {
        // main候補の本文比較でrecallを確保しつつ、rescue/semantic融合が同じ内容を
        // 再投入するのを、最終結果でも同じ本文signatureにより防ぐ。
        let body = conn.query_row("SELECT body FROM notes WHERE id=?1", [&hit.id], |row| {
            row.get::<_, String>(0)
        })?;
        let signature = DiversitySignature::new(
            hit.authority_scope.as_deref(),
            hit.authority_role.as_deref(),
            hit.authority_status.as_deref(),
            &body,
        );
        if selected
            .iter()
            .any(|(_, existing)| existing.same_cluster(&signature))
        {
            continue;
        }
        selected.push((hit.clone(), signature));
        if selected.len() >= limit {
            break;
        }
    }
    Ok(selected.into_iter().map(|(hit, _)| hit).collect())
}

fn anchor_search(conn: &Connection, query: &str, limit: usize) -> Result<Vec<Hit>> {
    // 本文OR検索より弱い補助信号なので、別名全体が一致したときだけリンク先を昇格する。
    let expr = match_expr(query);
    if expr.is_empty() {
        return Ok(Vec::new());
    }
    let mut statement = conn.prepare_cached(
        "SELECT anchor.dst, note.title, note.status,
                snippet(fts_anchor, 2, '[', ']', '…', 12), note.origin, note.tags,
                note.created, note.generated_at, note.note_uid, note.namespace,
                note.authority_role, note.authority_status, note.authority_scope
         FROM fts_anchor anchor JOIN notes note ON note.id = anchor.dst
         WHERE fts_anchor MATCH ?1 AND note.status != 'deprecated'
         ORDER BY rank LIMIT ?2",
    )?;
    let rows = statement.query_map(rusqlite::params![expr, limit as i64], |row| {
        Ok(Hit {
            id: row.get(0)?,
            title: row.get(1)?,
            status: row.get(2)?,
            snippet: row.get(3)?,
            via: "anchor",
            distance: None,
            origin: row.get(4)?,
            tags: split_tags(row.get::<_, Option<String>>(5)?),
            created: row.get(6)?,
            updated: row.get(7)?,
            note_uid: row.get(8)?,
            namespace: row.get(9)?,
            authority_role: row.get(10)?,
            authority_status: row.get(11)?,
            authority_scope: row.get(12)?,
        })
    })?;
    let mut seen = std::collections::HashSet::new();
    Ok(rows
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|hit| seen.insert(hit.id.clone()))
        .collect())
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct FieldScore {
    exact_title: bool,
    weighted_term_matches: usize,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RankingScore {
    exact_title: bool,
    intent_alignment: u8,
    weighted_term_matches: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct QueryIntent {
    current: bool,
    historical: bool,
    record: bool,
    rationale: bool,
}

impl QueryIntent {
    fn from_query(query: &str) -> Self {
        let query = query.to_lowercase();
        let latin_terms = query
            .split(|character: char| !character.is_ascii_alphanumeric())
            .filter(|term| !term.is_empty())
            .collect::<Vec<_>>();
        Self {
            current: contains_intent_marker(
                &query,
                &latin_terms,
                &["現行", "現在", "最新", "いま", "今の"],
                &["current", "latest", "now"],
            ),
            historical: contains_intent_marker(
                &query,
                &latin_terms,
                &["当時", "過去", "以前", "履歴", "旧版"],
                &["historical", "history", "previous", "former"],
            ),
            record: contains_intent_marker(
                &query,
                &latin_terms,
                &["記録", "ログ", "監査", "日付", "日時", "いつ"],
                &["record", "records", "log", "logs", "audit", "date", "when"],
            ),
            rationale: contains_intent_marker(
                &query,
                &latin_terms,
                &["理由", "根拠", "経緯", "なぜ", "どうして"],
                &["rationale", "reason", "reasons", "why", "decision"],
            ),
        }
    }

    fn alignment(self, authority: AuthorityValues<'_>) -> u8 {
        let mut score = 0;
        if self.current {
            score += match authority.status {
                Some("active") => 32,
                _ => 0,
            };
            score += match authority.role {
                Some("canonical") => 16,
                _ => 0,
            };
        }
        if self.historical {
            score += match authority.status {
                Some("historical") => 32,
                Some("superseded") => 16,
                _ => 0,
            };
        }
        if self.record {
            score += match authority.role {
                Some("record") => 16,
                _ => 0,
            };
            score += match authority.namespace {
                Some("records") => 8,
                _ => 0,
            };
        }
        if self.rationale {
            score += match authority.namespace {
                Some("decisions" | "records") => 8,
                _ => 0,
            };
        }
        score
    }
}

#[derive(Clone, Copy)]
struct AuthorityValues<'a> {
    namespace: Option<&'a str>,
    role: Option<&'a str>,
    status: Option<&'a str>,
}

fn contains_intent_marker(
    query: &str,
    latin_terms: &[&str],
    japanese_markers: &[&str],
    latin_markers: &[&str],
) -> bool {
    japanese_markers.iter().any(|marker| query.contains(marker))
        || latin_markers
            .iter()
            .any(|marker| latin_terms.contains(marker))
}

struct FieldValues<'a> {
    title: Option<&'a str>,
    description: Option<&'a str>,
    tags: Option<&'a str>,
    namespace: Option<&'a str>,
    scope: Option<&'a str>,
    body: &'a str,
}

fn field_score(query: &str, terms: &[String], fields: FieldValues<'_>) -> FieldScore {
    let title = fields.title.unwrap_or_default();
    FieldScore {
        exact_title: exact_title_match(query, Some(title)),
        weighted_term_matches: matched_terms(title, terms) * TITLE_TERM_WEIGHT
            + matched_terms(fields.description.unwrap_or_default(), terms)
                * DESCRIPTION_TERM_WEIGHT
            + matched_terms(fields.tags.unwrap_or_default(), terms) * TAG_TERM_WEIGHT
            + (matched_terms(fields.namespace.unwrap_or_default(), terms)
                + matched_terms(fields.scope.unwrap_or_default(), terms))
                * SCOPE_TERM_WEIGHT
            + matched_terms(fields.body, terms) * BODY_TERM_WEIGHT,
    }
}

fn field_terms(query: &str) -> Vec<String> {
    let mut terms = crate::tokenize::wakati(query)
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|term| term.chars().any(char::is_alphanumeric))
        .collect::<Vec<_>>();
    terms.sort();
    terms.dedup();
    terms
}

fn matched_terms(value: &str, terms: &[String]) -> usize {
    let value = value.to_lowercase();
    terms.iter().filter(|term| value.contains(*term)).count()
}

fn exact_title_match(query: &str, title: Option<&str>) -> bool {
    let query = normalized_phrase(query);
    !query.is_empty() && title.is_some_and(|title| query == normalized_phrase(title))
}

fn normalized_phrase(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

/// 意味検索(埋め込み KNN)。モデル未導入なら Ok(None)。
/// 関連判定は生コサイン距離 ≤ RELATED_DISTANCE(RRF スコアでは判定しない)。
fn vec_search(conn: &Connection, query: &str, limit: usize) -> Result<Option<Vec<(Hit, f32)>>> {
    use crate::embed;
    if !embed::model_installed() {
        return Ok(None); // 段0 の正常形
    }
    let qv = embed::embed_text(query)?; // 失敗は呼び側で劣化表示
    let neighbors = embed::knn(conn, &qv, limit * 2)?;
    let mut out = Vec::new();
    let mut stmt = conn.prepare_cached(
        "SELECT title, status, coalesce(description, substr(body,1,80)), origin, tags, created, generated_at,
                note_uid, namespace, authority_role, authority_status, authority_scope
         FROM notes WHERE id = ?1 AND status != 'deprecated'",
    )?;
    for (id, dist) in neighbors {
        if dist > embed::RELATED_DISTANCE {
            break; // 近い順なので以降は全て閾値外
        }
        let row = stmt.query_row([&id], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<String>>(11)?,
            ))
        });
        if let Ok((
            title,
            status,
            snippet,
            origin,
            tags,
            created,
            updated,
            note_uid,
            namespace,
            authority_role,
            authority_status,
            authority_scope,
        )) = row
        {
            out.push((
                Hit {
                    id,
                    title,
                    status,
                    snippet: snippet.replace('\n', " "),
                    via: "vec",
                    distance: Some(dist),
                    origin,
                    tags: split_tags(tags),
                    created,
                    updated,
                    note_uid,
                    namespace,
                    authority_role,
                    authority_status,
                    authority_scope,
                },
                dist,
            ));
        }
    }
    Ok(Some(out))
}

/// FTS(bm25 順)と意味検索(距離順)を RRF で融合。距離は Hit に残す。
fn fuse(fts: Vec<Hit>, vec_hits: Vec<(Hit, f32)>, limit: usize) -> Vec<Hit> {
    let rrf = |rank: usize| 1.0f32 / (60.0 + rank as f32);
    let mut score: std::collections::HashMap<String, f32> = std::collections::HashMap::new();
    let mut byid: std::collections::HashMap<String, Hit> = std::collections::HashMap::new();
    for (i, h) in fts.into_iter().enumerate() {
        *score.entry(h.id.clone()).or_default() += rrf(i);
        byid.insert(h.id.clone(), h);
    }
    for (j, (h, dist)) in vec_hits.into_iter().enumerate() {
        *score.entry(h.id.clone()).or_default() += rrf(j);
        byid.entry(h.id.clone())
            .and_modify(|e| {
                e.distance = Some(dist);
                e.via = if e.anchor_matched() {
                    "both_anchor"
                } else {
                    "both"
                };
            })
            .or_insert(h);
    }
    let mut ranked: Vec<(String, f32)> = score.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked
        .into_iter()
        .take(limit)
        .filter_map(|(id, _)| byid.remove(&id))
        .collect()
}

/// trigram MATCH(3文字以上の語)+ LIKE(2文字以下の語)の AND。
/// trigram 単独は2字語が黙って落ちるため単独では使わない(PoC ② の実測)。
fn rescue_search(conn: &Connection, query: &str, limit: usize) -> Result<Vec<Hit>> {
    let terms: Vec<&str> = query.split_whitespace().collect();
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let mut conds = Vec::new();
    let mut params: Vec<String> = Vec::new();
    for t in &terms {
        if t.chars().count() >= 3 {
            params.push(format!("\"{}\"", t.replace('"', "")));
            conds.push(format!(
                "n.id IN (SELECT id FROM fts_tri WHERE fts_tri MATCH ?{})",
                params.len()
            ));
        } else {
            params.push(format!("%{t}%"));
            conds.push(format!(
                "n.id IN (SELECT id FROM fts_tri WHERE text LIKE ?{})",
                params.len()
            ));
        }
    }
    let sql = format!(
        "SELECT n.id, n.title, n.status, substr(n.body, 1, 80), n.origin, n.tags, n.created, n.generated_at,
                n.note_uid, n.namespace, n.authority_role, n.authority_status, n.authority_scope FROM notes n
         WHERE n.status != 'deprecated' AND {} LIMIT {}",
        conds.join(" AND "),
        limit
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
        Ok(Hit {
            id: r.get(0)?,
            title: r.get(1)?,
            status: r.get(2)?,
            snippet: r.get::<_, String>(3)?.replace('\n', " "),
            via: "rescue",
            distance: None,
            origin: r.get(4)?,
            tags: split_tags(r.get::<_, Option<String>>(5)?),
            created: r.get(6)?,
            updated: r.get(7)?,
            note_uid: r.get(8)?,
            namespace: r.get(9)?,
            authority_role: r.get(10)?,
            authority_status: r.get(11)?,
            authority_scope: r.get(12)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn split_tags(t: Option<String>) -> Vec<String> {
    t.unwrap_or_default()
        .split_whitespace()
        .map(String::from)
        .collect()
}

/// タグの使用数(deprecated 除く・多い順)。一覧のフィルタチップ用。
pub fn tag_counts(conn: &Connection, limit: usize) -> Result<Vec<(String, usize)>> {
    let mut stmt = conn.prepare_cached("SELECT tags FROM notes WHERE status != 'deprecated'")?;
    let rows = stmt.query_map([], |r| r.get::<_, Option<String>>(0))?;
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for row in rows {
        for t in split_tags(row?) {
            *counts.entry(t).or_default() += 1;
        }
    }
    let mut v: Vec<(String, usize)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.truncate(limit);
    Ok(v)
}

/// タグの一覧(使用数+説明)。**説明はアプリが持たず KB の「タグ運用」ノートから読む**
/// (タグの意味づけは AI とユーザーの会話で決まる — 2026-08-10 方針)。
/// 表(| タグ | 説明 |)と箇条書き(- タグ — 説明 / - タグ: 説明)の両方を拾う。
#[derive(Debug, Clone, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct TagInfo {
    pub tag: String,
    pub count: usize,
    pub description: Option<String>,
}

pub fn tag_overview(conn: &Connection) -> Result<(Vec<TagInfo>, Option<String>)> {
    // 読み取りは tags::glossary が正本(`## 語彙` 節の表だけ・形式検証つき)。
    // 以前はこの関数が本文全体を舐めており、普通の箇条書きや URL が偽タグとして
    // 一覧に出た(2026-08-12)。
    let glossary = crate::tags::glossary(conn)?;
    let (desc, note_id) = (glossary.entries, glossary.note_id);
    let counts = tag_counts(conn, 500)?;
    let mut out: Vec<TagInfo> = counts
        .into_iter()
        .map(|(tag, count)| {
            let description = desc.get(&tag).cloned();
            TagInfo {
                tag,
                count,
                description,
            }
        })
        .collect();
    // 合意済みだがまだ使われていないタグも見せる(語彙として存在するため)
    for (tag, d) in desc {
        if !out.iter().any(|t| t.tag == tag) {
            out.push(TagInfo {
                tag,
                count: 0,
                description: Some(d),
            });
        }
    }
    out.sort_by(|a, b| b.count.cmp(&a.count).then(a.tag.cmp(&b.tag)));
    Ok((out, note_id))
}

/// 指定ノートと意味が近いノート(自分自身・リンク済み・退役は除く)。
/// リンクされていない関連 = Obsidian の unlinked mentions に相当し、埋め込みならではの発見。
pub fn similar_notes(
    conn: &Connection,
    id: &str,
    limit: usize,
) -> Result<Vec<(String, Option<String>, f32)>> {
    use crate::embed;
    let row: Option<(String, Vec<u8>)> = conn
        .query_row(
            "SELECT stamp, embedding FROM note_vecs WHERE id = ?1 AND stamp IS NOT NULL",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some(blob) = row.and_then(|(stamp, blob)| embed::is_current_stamp(&stamp).then_some(blob))
    else {
        return Ok(Vec::new());
    }; // 未埋め込み・旧stamp・段0 は空
    let me = embed::from_blob(&blob);
    let linked: std::collections::HashSet<String> = {
        let mut stmt = conn.prepare_cached(
            "SELECT other FROM (
                 SELECT dst AS other FROM links WHERE src = ?1
                 UNION SELECT src AS other FROM links WHERE dst = ?1
                 UNION
                 SELECT target.id AS other FROM notes source
                 JOIN note_relations relation ON relation.src_uid = source.note_uid
                 JOIN notes target ON target.note_uid = relation.target_uid
                 WHERE source.id = ?1
                 UNION
                 SELECT source.id AS other FROM notes target
                 JOIN note_relations relation ON relation.target_uid = target.note_uid
                 JOIN notes source ON source.note_uid = relation.src_uid
                 WHERE target.id = ?1
             )",
        )?;
        let rows = stmt.query_map([id], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let mut out = Vec::new();
    let mut stmt =
        conn.prepare_cached("SELECT title FROM notes WHERE id = ?1 AND status != 'deprecated'")?;
    for (nid, dist) in embed::knn(conn, &me, limit + linked.len() + 5)? {
        if out.len() >= limit {
            break;
        }
        if nid == id || linked.contains(&nid) || dist > embed::RELATED_DISTANCE {
            continue;
        }
        if let Ok(title) = stmt.query_row([&nid], |r| r.get::<_, Option<String>>(0)) {
            out.push((nid, title, dist));
        }
    }
    Ok(out)
}

/// 指定ノートの「つながり」(リンク先+被リンク、最大5件)。
pub fn related_of(conn: &Connection, id: Option<&str>) -> Result<Vec<(String, Option<String>)>> {
    let Some(id) = id else { return Ok(Vec::new()) };
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT other, (SELECT title FROM notes WHERE id = other) FROM (
             SELECT dst AS other FROM links WHERE src = ?1
             UNION SELECT src AS other FROM links WHERE dst = ?1
             UNION
             SELECT target.id AS other FROM notes source
             JOIN note_relations relation ON relation.src_uid = source.note_uid
             JOIN notes target ON target.note_uid = relation.target_uid
             WHERE source.id = ?1
             UNION
             SELECT source.id AS other FROM notes target
             JOIN note_relations relation ON relation.target_uid = target.note_uid
             JOIN notes source ON source.note_uid = relation.src_uid
             WHERE target.id = ?1
         ) LIMIT 5",
    )?;
    let rows = stmt.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// サイドバーに出すディレクトリ。count は直下だけでなく子孫ノートを含む。
#[derive(Debug, Clone, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct NoteCategory {
    pub path: String,
    pub name: String,
    pub count: usize,
}

/// カテゴリ別一覧の1行。本文全体を画面へ運ばないための軽量表現。
#[derive(Debug, Clone, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct NoteSummary {
    pub id: String,
    pub title: Option<String>,
    pub description: String,
    pub tags: Vec<String>,
    pub created: Option<String>,
    pub updated: Option<String>,
    /// リンク先と被リンクを合わせた、現存するノートの件数。
    pub linked_count: usize,
    /// 現行の埋め込みがなければ None。あれば、未リンクの近いノートがあるか。
    pub has_similar: Option<bool>,
    /// 台帳ファイルと旧添付の合計。ファイルシステム由来なので呼び出し層で補う。
    pub file_count: usize,
}

/// ID順のcursor page。全ノートを一度に画面へ渡さない。
#[derive(Debug, Clone, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct NoteListPage {
    pub notes: Vec<NoteSummary>,
    pub total: usize,
    pub next_cursor: Option<String>,
    pub degraded: Vec<crate::degradation::Degradation>,
}

pub fn note_categories(conn: &Connection) -> Result<Vec<NoteCategory>> {
    let mut stmt =
        conn.prepare_cached("SELECT id FROM notes WHERE status != 'deprecated' ORDER BY id")?;
    let ids = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut counts = std::collections::BTreeMap::<String, usize>::new();

    for id in ids {
        let mut segments: Vec<&str> = id.split('/').filter(|part| !part.is_empty()).collect();
        segments.pop();
        if segments.is_empty() {
            *counts.entry(String::new()).or_default() += 1;
            continue;
        }
        for index in 0..segments.len() {
            let path = segments[..=index].join("/");
            *counts.entry(path).or_default() += 1;
        }
    }

    Ok(counts
        .into_iter()
        .map(|(path, count)| NoteCategory {
            name: path.rsplit('/').next().unwrap_or("").to_string(),
            path,
            count,
        })
        .collect())
}

pub fn notes_in_category(
    conn: &Connection,
    category: &str,
    after: Option<&str>,
    limit: usize,
) -> Result<NoteListPage> {
    let limit = limit.clamp(1, 100);
    let after = after.unwrap_or("");
    let (total, mut notes) = if category.is_empty() {
        let total = conn.query_row(
            "SELECT count(*) FROM notes WHERE status != 'deprecated' AND instr(id, '/') = 0",
            [],
            |r| r.get::<_, i64>(0),
        )? as usize;
        let mut stmt = conn.prepare_cached(
            "SELECT id, title, coalesce(description, substr(body,1,120)), tags, created, generated_at
             FROM notes
             WHERE status != 'deprecated' AND instr(id, '/') = 0 AND id > ?1
             ORDER BY id LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![after, (limit + 1) as i64], note_summary)?;
        (total, rows.collect::<std::result::Result<Vec<_>, _>>()?)
    } else {
        let total = conn.query_row(
            "SELECT count(*) FROM notes
             WHERE status != 'deprecated' AND substr(id, 1, length(?1) + 1) = ?1 || '/'",
            [category],
            |r| r.get::<_, i64>(0),
        )? as usize;
        let mut stmt = conn.prepare_cached(
            "SELECT id, title, coalesce(description, substr(body,1,120)), tags, created, generated_at
             FROM notes
             WHERE status != 'deprecated'
               AND substr(id, 1, length(?1) + 1) = ?1 || '/'
               AND id > ?2
             ORDER BY id LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![category, after, (limit + 1) as i64],
            note_summary,
        )?;
        (total, rows.collect::<std::result::Result<Vec<_>, _>>()?)
    };

    let has_more = notes.len() > limit;
    if has_more {
        notes.truncate(limit);
    }
    populate_note_relations(conn, &mut notes)?;
    let next_cursor = has_more
        .then(|| notes.last().map(|note| note.id.clone()))
        .flatten();
    Ok(NoteListPage {
        notes,
        total,
        next_cursor,
        degraded: Vec::new(),
    })
}

fn note_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<NoteSummary> {
    Ok(NoteSummary {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get::<_, String>(2)?.replace('\n', " "),
        tags: split_tags(row.get::<_, Option<String>>(3)?),
        created: row.get(4)?,
        updated: row.get(5)?,
        linked_count: 0,
        has_similar: None,
        file_count: 0,
    })
}

/// 一覧ページにだけ必要な関係メタデータを補う。
///
/// リンク件数は逆引き用 index を使う。近いノートは全ベクトルを一度だけ読み、
/// ページ内の各ノートについて最初の候補が見つかった時点で打ち切る。
fn populate_note_relations(conn: &Connection, notes: &mut [NoteSummary]) -> Result<()> {
    let mut count_links = conn.prepare_cached(
        "SELECT count(*) FROM (
             SELECT l.dst AS other
             FROM links l JOIN notes n ON n.id = l.dst AND n.status != 'deprecated'
             WHERE l.src = ?1
             UNION
             SELECT l.src AS other
             FROM links l JOIN notes n ON n.id = l.src AND n.status != 'deprecated'
             WHERE l.dst = ?1
             UNION
             SELECT target.id AS other FROM notes source
             JOIN note_relations relation ON relation.src_uid = source.note_uid
             JOIN notes target ON target.note_uid = relation.target_uid
             WHERE source.id = ?1 AND target.status != 'deprecated'
             UNION
             SELECT source.id AS other FROM notes target
             JOIN note_relations relation ON relation.target_uid = target.note_uid
             JOIN notes source ON source.note_uid = relation.src_uid
             WHERE target.id = ?1 AND source.status != 'deprecated'
         )",
    )?;
    for note in notes.iter_mut() {
        note.linked_count = count_links.query_row([&note.id], |row| row.get::<_, i64>(0))? as usize;
    }

    if notes.is_empty() {
        return Ok(());
    }

    use crate::embed;
    let vectors: std::collections::HashMap<String, Vec<f32>> = {
        let mut stmt = conn.prepare_cached(
            "SELECT v.id, v.stamp, v.embedding
             FROM note_vecs v JOIN notes n ON n.id = v.id
             WHERE v.stamp IS NOT NULL AND n.status != 'deprecated'",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|(_, stamp, _)| embed::is_current_stamp(stamp))
            .map(|(id, _, blob)| (id, embed::from_blob(&blob)))
            .collect()
    };
    if vectors.is_empty() {
        return Ok(());
    }

    let linked: std::collections::HashMap<String, std::collections::HashSet<String>> = {
        let mut stmt = conn.prepare_cached(
            "SELECT src, dst FROM links
             UNION
             SELECT source.id, target.id FROM note_relations relation
             JOIN notes source ON source.note_uid = relation.src_uid
             JOIN notes target ON target.note_uid = relation.target_uid",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut by_note =
            std::collections::HashMap::<String, std::collections::HashSet<String>>::new();
        for row in rows {
            let (src, dst): (String, String) = row?;
            by_note.entry(src.clone()).or_default().insert(dst.clone());
            by_note.entry(dst).or_default().insert(src);
        }
        by_note
    };

    for note in notes.iter_mut() {
        let Some(me) = vectors.get(&note.id) else {
            continue;
        };
        let note_links = linked.get(&note.id);
        note.has_similar = Some(vectors.iter().any(|(other_id, other)| {
            if other_id == &note.id
                || note_links.is_some_and(|ids| ids.contains(other_id))
                || other.len() != me.len()
            {
                return false;
            }
            let similarity: f32 = me.iter().zip(other).map(|(a, b)| a * b).sum();
            1.0 - similarity <= embed::RELATED_DISTANCE
        }));
    }
    Ok(())
}

/// 健全性の要約(FR-A2 ホーム表示用)。
#[derive(Debug, Clone, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Stats {
    pub total: usize,
    pub deprecated: usize,
    /// メモ(origin: human)/ AI ノート(origin: agent)の内訳(deprecated 除く)
    pub memos: usize,
    pub agent_notes: usize,
    /// つながり(リンク)の本数
    pub links: usize,
    /// かしこい検索(段1)が導入済みか
    pub embed_enabled: bool,
    /// 現行スタンプで埋め込み済みのノート数(欠損の可視化 — 沈黙停止の教訓)
    pub embedded: usize,
}

pub fn stats(conn: &Connection) -> Result<Stats> {
    let count = |sql: &str| -> Result<usize> {
        Ok(conn.query_row(sql, [], |r| r.get::<_, i64>(0))? as usize)
    };
    let embedded = conn
        .prepare("SELECT stamp FROM note_vecs WHERE stamp IS NOT NULL")
        .and_then(|mut stmt| {
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut count = 0usize;
            for stamp in rows {
                if crate::embed::is_current_stamp(&stamp?) {
                    count += 1;
                }
            }
            Ok(count)
        })
        .unwrap_or(0);
    Ok(Stats {
        total: count("SELECT count(*) FROM notes")?,
        deprecated: count("SELECT count(*) FROM notes WHERE status='deprecated'")?,
        memos: count(
            "SELECT count(*) FROM notes WHERE status != 'deprecated' AND coalesce(origin,'human') != 'agent'",
        )?,
        agent_notes: count(
            "SELECT count(*) FROM notes WHERE status != 'deprecated' AND origin = 'agent'",
        )?,
        links: count(
            "SELECT (SELECT count(*) FROM links) + (SELECT count(*) FROM note_relations)",
        )?,
        embed_enabled: crate::embed::model_installed(),
        embedded,
    })
}

/// 直近ノート(generated_at 降順、なければ mtime 降順)。
pub fn recent(conn: &Connection, limit: usize) -> Result<Vec<Hit>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, title, status, coalesce(description, substr(body,1,80)), origin, tags, created, generated_at,
                note_uid, namespace, authority_role, authority_status, authority_scope
         FROM notes WHERE status != 'deprecated'
         ORDER BY coalesce(generated_at, created, '') DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |r| {
        Ok(Hit {
            id: r.get(0)?,
            title: r.get(1)?,
            status: r.get(2)?,
            snippet: r.get::<_, String>(3)?.replace('\n', " "),
            via: "recent",
            distance: None,
            origin: r.get(4)?,
            tags: split_tags(r.get::<_, Option<String>>(5)?),
            created: r.get(6)?,
            updated: r.get(7)?,
            note_uid: r.get(8)?,
            namespace: r.get(9)?,
            authority_role: r.get(10)?,
            authority_status: r.get(11)?,
            authority_scope: r.get(12)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::authority::{
        Authority, AuthorityRole, AuthorityStatus, NoteNamespace, NoteRelation, RelationKind,
    };
    use crate::frontmatter::{Frontmatter, Generated, Note};
    use crate::index::{open_db, sync};
    use crate::vault::{NoteProposal, NoteUpdate, Vault};

    const PERFORMANCE_NOTE_COUNT: usize = 10_000;
    const PERFORMANCE_CATEGORY_COUNT: usize = 100;
    const PERFORMANCE_VECTOR_DIM: usize = 1_024;
    const PERFORMANCE_TARGET: usize = 9_876;

    fn setup() -> (tempfile::TempDir, Vault, rusqlite::Connection) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        vault
            .propose_for_test(
                "認証設計メモ",
                "認証フローの見直しを行った。監査ログも整備する。",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        vault
            .propose_for_test(
                "運用ノート",
                "本番環境の運用手順とバックアップのライフサイクルを記録。",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        vault
            .propose_for_test(
                "無関係",
                "昨日の打ち合わせ内容を整理する。",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let conn = open_db(&vault).unwrap();
        sync(&vault, &conn).unwrap();
        (dir, vault, conn)
    }

    #[test]
    fn two_char_word_hits_via_main() {
        let (_d, _v, conn) = setup();
        let out = super::search(&conn, "認証", 10);
        assert!(out.degraded.is_empty(), "{:?}", out.degraded);
        assert_eq!(out.hits.len(), 1);
        assert_eq!(out.hits[0].via, "main");
    }

    #[test]
    fn keyword_enumeration_hits() {
        let (_d, _v, conn) = setup();
        let out = super::search(&conn, "認証 監査", 10);
        assert_eq!(out.hits.len(), 1, "{:?}", out.hits);
    }

    #[test]
    fn partial_word_hits_via_rescue() {
        let (_d, _v, conn) = setup();
        // 「サイクル」は「ライフサイクル」の内部 — 形態素側は構造的に0件、レスキューで拾う
        let out = super::search(&conn, "サイクル", 10);
        assert_eq!(out.hits.len(), 1, "{:?}", out.hits);
        assert_eq!(out.hits[0].via, "rescue");
    }

    /// 2026-08-16までは関連取得のDB失敗が空配列になり、本当の0件と区別できなかった。
    #[test]
    fn related_failure_keeps_search_hits_and_adds_a_typed_degradation() {
        let (_d, _v, conn) = setup();
        conn.execute_batch("DROP TABLE links").unwrap();

        let out = super::search(&conn, "認証", 10);
        assert_eq!(out.hits.len(), 1);
        assert!(out.related.is_empty());
        assert!(
            out.degraded
                .iter()
                .any(|item| matches!(item, crate::degradation::Degradation::RelatedNotes { .. }))
        );
    }

    /// 未埋め込みは正常な空だが、埋め込み表そのものの故障は部分失敗として返す。
    #[test]
    fn similar_notes_distinguishes_missing_data_from_a_broken_index() {
        let (_d, _v, conn) = setup();
        assert!(
            super::similar_notes(&conn, "notes/認証設計メモ", 5)
                .unwrap()
                .is_empty()
        );
        conn.execute_batch("DROP TABLE note_vecs").unwrap();
        assert!(super::similar_notes(&conn, "notes/認証設計メモ", 5).is_err());
    }

    #[test]
    fn recent_returns_all() {
        let (_d, _v, conn) = setup();
        assert_eq!(super::recent(&conn, 10).unwrap().len(), 3);
    }

    #[test]
    fn active_canonical_ranks_before_records_and_proposals() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        let create = |title: &str, authority: Authority| {
            vault
                .propose(
                    &conn,
                    NoteProposal {
                        title,
                        body: "権威順位番兵アルタイル",
                        description: None,
                        tags: &["test".into()],
                        authority,
                        relations: Vec::new(),
                        allow_new_tags: true,
                        client: "test/client",
                    },
                )
                .unwrap()
        };
        let record = create(
            "記録",
            Authority {
                namespace: NoteNamespace::Records,
                role: AuthorityRole::Record,
                status: AuthorityStatus::Active,
                scope: "test/authority-ranking".into(),
            },
        );
        let proposal = create(
            "提案",
            Authority {
                namespace: NoteNamespace::Knowledge,
                role: AuthorityRole::Proposal,
                status: AuthorityStatus::Active,
                scope: "test/authority-ranking".into(),
            },
        );
        let canonical = create(
            "正本",
            Authority {
                namespace: NoteNamespace::Knowledge,
                role: AuthorityRole::Canonical,
                status: AuthorityStatus::Active,
                scope: "test/authority-ranking".into(),
            },
        );
        assert!(
            vault
                .propose(
                    &conn,
                    NoteProposal {
                        title: "重複正本",
                        body: "権威順位番兵アルタイル",
                        description: None,
                        tags: &["test".into()],
                        authority: Authority {
                            namespace: NoteNamespace::Knowledge,
                            role: AuthorityRole::Canonical,
                            status: AuthorityStatus::Active,
                            scope: "test/authority-ranking".into(),
                        },
                        relations: Vec::new(),
                        allow_new_tags: false,
                        client: "test/client",
                    },
                )
                .is_err()
        );

        let hits = super::search(&conn, "権威順位番兵アルタイル", 10).hits;
        assert_eq!(
            hits.iter().map(|hit| hit.id.as_str()).collect::<Vec<_>>(),
            [canonical.as_str(), record.as_str(), proposal.as_str()]
        );
    }

    #[test]
    fn exact_title_ranks_before_repeated_body_and_canonical_authority() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        let target = vault
            .propose(
                &conn,
                NoteProposal {
                    title: "Nebula Launch Checklist",
                    body: "go or no-go procedure",
                    description: None,
                    tags: &["test".into()],
                    authority: Authority {
                        namespace: NoteNamespace::Records,
                        role: AuthorityRole::Record,
                        status: AuthorityStatus::Active,
                        scope: "test/nebula-target".into(),
                    },
                    relations: Vec::new(),
                    allow_new_tags: true,
                    client: "test/client",
                },
            )
            .unwrap();
        for suffix in ["a", "b", "c", "d", "e"] {
            vault
                .propose(
                    &conn,
                    NoteProposal {
                        title: &format!("Nebula Canonical {suffix}"),
                        body: "nebula launch checklist nebula launch checklist nebula launch checklist",
                        description: None,
                        tags: &["test".into()],
                        authority: Authority {
                            namespace: NoteNamespace::Knowledge,
                            role: AuthorityRole::Canonical,
                            status: AuthorityStatus::Active,
                            scope: format!("test/nebula-decoy-{suffix}"),
                        },
                        relations: Vec::new(),
                        allow_new_tags: false,
                        client: "test/client",
                    },
                )
                .unwrap();
        }

        let hits = super::search_mode(&conn, "nebula launch checklist", 5, true).hits;
        assert_eq!(
            hits.first().map(|hit| hit.id.as_str()),
            Some(target.as_str())
        );
    }

    #[test]
    fn field_weights_descend_from_title_to_body_without_counting_repetition() {
        let terms = super::field_terms("beacon");
        let score = |title, description, tags, scope, body| {
            super::field_score(
                "beacon",
                &terms,
                super::FieldValues {
                    title: Some(title),
                    description: Some(description),
                    tags: Some(tags),
                    namespace: Some("knowledge"),
                    scope: Some(scope),
                    body,
                },
            )
            .weighted_term_matches
        };
        let title = score("prefix beacon", "", "", "", "");
        let description = score("", "beacon", "", "", "");
        let tags = score("", "", "beacon", "", "");
        let scope = score("", "", "", "beacon", "");
        let body_once = score("", "", "", "", "beacon");
        let body_repeated = score("", "", "", "", "beacon beacon beacon");
        assert!(title > description);
        assert!(description > tags);
        assert!(tags > scope);
        assert!(scope > body_once);
        assert_eq!(body_once, body_repeated);
    }

    #[test]
    fn duplicate_bodies_collapse_and_leave_room_for_a_distinct_facet() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        let target = vault
            .propose(
                &conn,
                NoteProposal {
                    title: "Mercury Program Risk Facet",
                    body: "mercury budget review identifies the unique cashflow risk facet",
                    description: None,
                    tags: &["test".into()],
                    authority: Authority {
                        namespace: NoteNamespace::Records,
                        role: AuthorityRole::Record,
                        status: AuthorityStatus::Active,
                        scope: "test/mercury-risk".into(),
                    },
                    relations: Vec::new(),
                    allow_new_tags: true,
                    client: "test/client",
                },
            )
            .unwrap();
        for suffix in ["a", "b", "c", "d", "e"] {
            vault
                .propose(
                    &conn,
                    NoteProposal {
                        title: &format!("Mercury Program Digest {suffix}"),
                        body: "mercury budget review mercury budget review mercury budget review",
                        description: None,
                        tags: &["test".into()],
                        authority: Authority {
                            namespace: NoteNamespace::Knowledge,
                            role: AuthorityRole::Canonical,
                            status: AuthorityStatus::Active,
                            scope: format!("test/mercury-digest-{suffix}"),
                        },
                        relations: Vec::new(),
                        allow_new_tags: false,
                        client: "test/client",
                    },
                )
                .unwrap();
        }

        let hits = super::search_mode(&conn, "mercury budget review", 5, true).hits;
        assert!(hits.iter().any(|hit| hit.id == target));
        assert_eq!(
            hits.iter()
                .filter(|hit| hit
                    .title
                    .as_deref()
                    .is_some_and(|title| title.contains("Digest")))
                .count(),
            1
        );
    }

    #[test]
    fn explicit_historical_intent_ranks_a_record_before_active_canonical_notes() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        let target = vault
            .propose(
                &conn,
                NoteProposal {
                    title: "Orion Audit Record 2025",
                    body: "orion 当時 理由。The historical record explains the accepted exception.",
                    description: None,
                    tags: &["test".into()],
                    authority: Authority {
                        namespace: NoteNamespace::Records,
                        role: AuthorityRole::Record,
                        status: AuthorityStatus::Historical,
                        scope: "test/orion-history".into(),
                    },
                    relations: Vec::new(),
                    allow_new_tags: true,
                    client: "test/client",
                },
            )
            .unwrap();
        for suffix in ["a", "b", "c", "d", "e"] {
            vault
                .propose(
                    &conn,
                    NoteProposal {
                        title: &format!("Orion Current Policy {suffix}"),
                        body: "orion 当時 理由 orion 当時 理由 orion 当時 理由",
                        description: None,
                        tags: &["test".into()],
                        authority: Authority {
                            namespace: NoteNamespace::Knowledge,
                            role: AuthorityRole::Canonical,
                            status: AuthorityStatus::Active,
                            scope: format!("test/orion-current-{suffix}"),
                        },
                        relations: Vec::new(),
                        allow_new_tags: false,
                        client: "test/client",
                    },
                )
                .unwrap();
        }

        let hits = super::search_mode(&conn, "orion 当時 理由", 5, true).hits;
        assert_eq!(
            hits.first().map(|hit| hit.id.as_str()),
            Some(target.as_str())
        );
    }

    #[test]
    fn query_intent_alignment_is_explicit_and_neutral_queries_add_no_bias() {
        let active_canonical = super::AuthorityValues {
            namespace: Some("knowledge"),
            role: Some("canonical"),
            status: Some("active"),
        };
        let historical_record = super::AuthorityValues {
            namespace: Some("records"),
            role: Some("record"),
            status: Some("historical"),
        };
        let historical = super::QueryIntent::from_query("当時の理由");
        let current = super::QueryIntent::from_query("現行の手順");
        let neutral = super::QueryIntent::from_query("orion policy");
        let substring_only = super::QueryIntent::from_query("unknown catalog");

        assert!(historical.alignment(historical_record) > historical.alignment(active_canonical));
        assert!(current.alignment(active_canonical) > current.alignment(historical_record));
        assert_eq!(neutral.alignment(active_canonical), 0);
        assert_eq!(neutral.alignment(historical_record), 0);
        assert_eq!(substring_only.alignment(active_canonical), 0);
        assert_eq!(substring_only.alignment(historical_record), 0);
    }

    #[test]
    fn matching_anchor_text_ranks_the_link_target_without_target_term_repetition() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        let target = vault
            .propose(
                &conn,
                NoteProposal {
                    title: "Kepler Retention Procedure",
                    body: "Retained material is removed after the approved interval.",
                    description: None,
                    tags: &["test".into()],
                    authority: Authority {
                        namespace: NoteNamespace::Procedures,
                        role: AuthorityRole::Canonical,
                        status: AuthorityStatus::Active,
                        scope: "test/kepler-retention".into(),
                    },
                    relations: Vec::new(),
                    allow_new_tags: true,
                    client: "test/client",
                },
            )
            .unwrap();
        vault
            .propose(
                &conn,
                NoteProposal {
                    title: "Storage Map",
                    body: &format!("See [blue comet policy](/{target}.md)."),
                    description: None,
                    tags: &["test".into()],
                    authority: Authority {
                        namespace: NoteNamespace::Records,
                        role: AuthorityRole::Record,
                        status: AuthorityStatus::Active,
                        scope: "test/storage-map".into(),
                    },
                    relations: Vec::new(),
                    allow_new_tags: false,
                    client: "test/client",
                },
            )
            .unwrap();
        for suffix in ["a", "b", "c", "d", "e"] {
            vault
                .propose(
                    &conn,
                    NoteProposal {
                        title: &format!("Blue Comet Index {suffix}"),
                        body: "blue comet policy blue comet policy blue comet policy",
                        description: None,
                        tags: &["test".into()],
                        authority: Authority {
                            namespace: NoteNamespace::Knowledge,
                            role: AuthorityRole::Canonical,
                            status: AuthorityStatus::Active,
                            scope: format!("test/blue-comet-{suffix}"),
                        },
                        relations: Vec::new(),
                        allow_new_tags: false,
                        client: "test/client",
                    },
                )
                .unwrap();
        }

        let hits = super::search_mode(&conn, "blue comet policy", 5, true).hits;
        assert_eq!(
            hits.first().map(|hit| (hit.id.as_str(), hit.via)),
            Some((target.as_str(), "anchor"))
        );
    }

    #[test]
    fn typed_relations_expand_retrieval_and_block_dangling_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        let target = vault
            .propose(
                &conn,
                NoteProposal {
                    title: "根拠記録",
                    body: "観測結果",
                    description: None,
                    tags: &["test".into()],
                    authority: Authority {
                        namespace: NoteNamespace::Records,
                        role: AuthorityRole::Record,
                        status: AuthorityStatus::Active,
                        scope: "test/typed-relation-source".into(),
                    },
                    relations: Vec::new(),
                    allow_new_tags: true,
                    client: "test/client",
                },
            )
            .unwrap();
        let target_uid = vault
            .read_note_from_db(&conn, &target)
            .unwrap()
            .front
            .note_uid
            .unwrap();
        assert!(
            vault
                .propose(
                    &conn,
                    NoteProposal {
                        title: "参照切れ",
                        body: "保存してはいけない",
                        description: None,
                        tags: &["test".into()],
                        authority: Authority {
                            namespace: NoteNamespace::Knowledge,
                            role: AuthorityRole::Canonical,
                            status: AuthorityStatus::Active,
                            scope: "test/missing-relation-target".into(),
                        },
                        relations: vec![NoteRelation {
                            kind: RelationKind::Supports,
                            target: crate::authority::NoteUid::at(999),
                        }],
                        allow_new_tags: false,
                        client: "test/client",
                    },
                )
                .is_err()
        );
        let source = vault
            .propose(
                &conn,
                NoteProposal {
                    title: "導出知識",
                    body: "結論",
                    description: None,
                    tags: &["test".into()],
                    authority: Authority {
                        namespace: NoteNamespace::Knowledge,
                        role: AuthorityRole::Canonical,
                        status: AuthorityStatus::Active,
                        scope: "test/typed-relation-result".into(),
                    },
                    relations: vec![NoteRelation {
                        kind: RelationKind::DerivedFrom,
                        target: target_uid,
                    }],
                    allow_new_tags: true,
                    client: "test/client",
                },
            )
            .unwrap();

        let bundle = crate::retrieval::context_documents(
            &conn,
            std::slice::from_ref(&source),
            crate::retrieval::RetrievalOptions::default(),
        )
        .unwrap();
        assert_eq!(
            bundle
                .documents
                .iter()
                .map(|doc| doc.id.as_str())
                .collect::<Vec<_>>(),
            [source.as_str(), target.as_str()]
        );
        assert!(vault.agent_removal_candidate(&conn, &target).is_err());
        assert!(
            vault
                .agent_delete_note(&conn, &target, "蒸留後の整理", "test/client")
                .is_err()
        );
        vault
            .agent_update_note(
                &conn,
                NoteUpdate {
                    id: &source,
                    title: None,
                    body: None,
                    description: None,
                    tags: None,
                    authority: None,
                    relations: Some(Vec::new()),
                    allow_new_tags: false,
                    client: "test/client",
                },
            )
            .unwrap();
        vault
            .agent_delete_note(&conn, &target, "蒸留後の整理", "test/client")
            .unwrap();
    }

    #[test]
    fn categories_count_descendant_notes_and_skip_deprecated() {
        let (_d, _v, conn) = setup();
        for (id, status) in [
            ("research/ai/検索", "stable"),
            ("research/概要", "stable"),
            ("入口", "stable"),
            ("research/旧版", "deprecated"),
        ] {
            conn.execute(
                "INSERT INTO notes(id,title,status,body,tags) VALUES (?1,?1,?2,'','')",
                rusqlite::params![id, status],
            )
            .unwrap();
        }
        let categories = super::note_categories(&conn).unwrap();
        let counts: std::collections::BTreeMap<_, _> = categories
            .into_iter()
            .map(|category| (category.path, category.count))
            .collect();
        assert_eq!(counts.get("research"), Some(&2));
        assert_eq!(counts.get("research/ai"), Some(&1));
        assert_eq!(counts.get(""), Some(&1));
    }

    #[test]
    fn category_list_is_cursor_paginated() {
        let (_d, _v, conn) = setup();
        for id in ["research/ai/検索", "research/概要"] {
            conn.execute(
                "INSERT INTO notes(id,title,status,body,tags) VALUES (?1,?1,'stable','','')",
                [id],
            )
            .unwrap();
        }
        let first = super::notes_in_category(&conn, "research", None, 1).unwrap();
        assert_eq!(first.total, 2);
        assert_eq!(first.notes.len(), 1);
        let second =
            super::notes_in_category(&conn, "research", first.next_cursor.as_deref(), 1).unwrap();
        assert_eq!(second.notes.len(), 1);
        assert!(second.next_cursor.is_none());
        assert_ne!(first.notes[0].id, second.notes[0].id);
    }

    #[test]
    fn category_list_includes_link_and_similar_presence() {
        let (_d, _v, conn) = setup();
        for id in ["signals/a", "signals/b", "signals/c", "signals/d"] {
            conn.execute(
                "INSERT INTO notes(id,title,status,body,tags) VALUES (?1,?1,'stable','','')",
                [id],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO links(src,dst) VALUES ('signals/a','signals/b')",
            [],
        )
        .unwrap();
        for (id, vector) in [
            ("signals/a", vec![1.0, 0.0]),
            ("signals/c", vec![1.0, 0.0]),
            ("signals/d", vec![0.0, 1.0]),
        ] {
            conn.execute(
                "INSERT INTO note_vecs(id,stamp,embedding) VALUES (?1,?2,?3)",
                rusqlite::params![
                    id,
                    crate::embed::embedding_stamp("fixture"),
                    crate::embed::to_blob(&vector)
                ],
            )
            .unwrap();
        }

        let page = super::notes_in_category(&conn, "signals", None, 10).unwrap();
        let by_id: std::collections::HashMap<_, _> = page
            .notes
            .into_iter()
            .map(|note| (note.id.clone(), note))
            .collect();
        assert_eq!(by_id["signals/a"].linked_count, 1);
        assert_eq!(by_id["signals/a"].has_similar, Some(true));
        assert_eq!(by_id["signals/b"].has_similar, None);
        assert_eq!(by_id["signals/d"].has_similar, Some(false));
    }

    /// 10k規模の目標が文章だけだったため、2026-08-16から専用release CIで退行を止める。
    #[test]
    #[ignore = "release buildの専用CIで10k fixtureを測る"]
    fn ten_thousand_note_performance_gate() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        write_performance_fixture(&vault);
        let (conn, rebuild) = timed(|| open_db(&vault).unwrap());
        assert_eq!(super::stats(&conn).unwrap().total, PERFORMANCE_NOTE_COUNT);
        seed_performance_vectors(&conn);

        let ((recent_notes, care, tags, home_stats), home_read) = timed(|| {
            repeat_last(20, || {
                (
                    super::recent(&conn, 500).unwrap(),
                    crate::care::list_open(&conn).unwrap(),
                    super::tag_counts(&conn, 30).unwrap(),
                    super::stats(&conn).unwrap(),
                )
            })
        });
        assert_eq!(recent_notes.len(), 500);
        assert!(care.is_empty());
        assert_eq!(tags.len(), 11);
        assert_eq!(home_stats.total, PERFORMANCE_NOTE_COUNT);

        let (categories, category_list) =
            timed(|| repeat_last(20, || super::note_categories(&conn).unwrap()));
        assert_eq!(categories.len(), PERFORMANCE_CATEGORY_COUNT + 1);

        let (page, note_list) = timed(|| {
            repeat_last(5, || {
                super::notes_in_category(&conn, "notes/topic-042", None, 50).unwrap()
            })
        });
        assert_eq!(
            page.total,
            PERFORMANCE_NOTE_COUNT / PERFORMANCE_CATEGORY_COUNT
        );
        assert_eq!(page.notes.len(), 50);
        assert!(page.next_cursor.is_some());

        let target_id = performance_note_id(PERFORMANCE_TARGET);
        let ((main, rescue), keyword_search) = timed(|| {
            repeat_last(100, || {
                (
                    super::main_search(&conn, "検索番兵オーロラ", 20, false).unwrap(),
                    super::rescue_search(&conn, "番兵オーロラ", 20).unwrap(),
                )
            })
        });
        assert!(main.iter().any(|hit| hit.id == target_id));
        assert!(rescue.iter().any(|hit| hit.id == target_id));

        let mut query = vec![0.0f32; PERFORMANCE_VECTOR_DIM];
        query[PERFORMANCE_TARGET % PERFORMANCE_VECTOR_DIM] = 1.0;
        let (neighbors, semantic_search) =
            timed(|| repeat_last(10, || crate::embed::knn(&conn, &query, 20).unwrap()));
        assert_eq!(neighbors.len(), 20);
        assert!(neighbors.iter().any(|(id, _)| id == &target_id));

        let (_, note_detail) = timed(|| {
            repeat_last(5, || {
                let note = crate::note_store::read(&conn, &target_id).unwrap();
                let related = super::related_of(&conn, Some(&target_id)).unwrap();
                let similar = super::similar_notes(&conn, &target_id, 6).unwrap();
                assert!(note.body.contains("検索番兵オーロラ"));
                assert!(!related.is_empty());
                assert_eq!(similar.len(), 6);
            })
        });

        let retrieval_hits = super::main_search(&conn, "検索番兵オーロラ", 5, true).unwrap();
        let retrieval_seed_ids = retrieval_hits
            .iter()
            .map(|hit| hit.id.clone())
            .collect::<Vec<_>>();
        let (retrieval, linked_context) = timed(|| {
            repeat_last(20, || {
                crate::retrieval::context_documents_for_query(
                    &conn,
                    &retrieval_seed_ids,
                    "検索番兵オーロラ",
                    crate::retrieval::RetrievalOptions::default(),
                )
                .unwrap()
            })
        });
        assert!(retrieval.documents.iter().any(|note| note.id == target_id));
        assert!(retrieval.stats.candidate_count >= 3);

        // ---------------------------------------- 派生索引registry追加計測(2026-08-28)
        // warm open: 構築済みDBの再open。open毎のhealth check(object存在・型 /
        // fts_main・fts_triのnote IDカバレッジ / governance validate)込みで5回測る。
        // healthyなDBでは修復・書込が一切走らないこと自体も検査する。
        drop(conn);
        let (warm_outcome, warm_open) = timed(|| {
            repeat_last(5, || {
                let outcome = crate::index::open_db_with_outcome(&vault).unwrap();
                assert!(
                    outcome.recovered.is_empty(),
                    "healthyなDBのwarm openで修復が走った"
                );
                assert!(outcome.write_blockers.is_empty());
                outcome
            })
        });
        let conn = warm_outcome.conn;

        // embed_pendingスキャン(レビューF2): 定常状態(全noteが現行prefixの
        // stamp行を持つ)ではSQL prefilterだけで候補0件を判定し、本文の
        // materialize・再hashを行わない。モデル未導入でもSQL+判定部は測れる。
        // 検索(MCP search)ごとに走る経路なので、全corpus走査の復活をここで止める。
        let (steady_pending, embed_pending_scan) =
            timed(|| repeat_last(100, || crate::embed::embed_pending(&conn, 0).unwrap()));
        assert_eq!(steady_pending, 0, "定常状態でpendingが残っている");

        // 単一note更新100回: 本番write経路(governance fail-closedゲート →
        // registry走査での派生索引維持 → 埋め込み無効化 → outbox積み → commit)を
        // 1件ずつ測り、median / p95 で判定する。Markdown export(git commit)は
        // registry変更の対象外かつファイルI/O支配のため計測に含めない。
        // 絶対値予算の根拠は docs/derived-registry.md(baseline実測+15%/+20%規則
        // にCIゆらぎの余裕を乗せた値)。
        let update_id = performance_note_id(PERFORMANCE_TARGET + 7);
        let mut update_note = crate::note_store::read(&conn, &update_id).unwrap();
        let mut update_samples = Vec::with_capacity(100);
        for round in 0..100usize {
            update_note.body =
                format!("単一更新回帰 {round:03}。派生索引の増分維持と埋め込み無効化を通す本文。");
            let ((), elapsed) = timed(|| {
                crate::note_store::put(&vault, &conn, &update_id, &update_note, "perf", "perf")
                    .unwrap()
            });
            update_samples.push(elapsed);
        }
        update_samples.sort();
        let update_median = update_samples[49];
        let update_p95 = update_samples[94];
        eprintln!(
            "performance_gate note_update_x100 us: median {} p95 {} max {}",
            update_median.as_micros(),
            update_p95.as_micros(),
            update_samples[99].as_micros()
        );

        // artifact rebuild: 自己修復と同じ force_rebuild(DROP→CREATE→再導出、
        // governanceはvalidate込み)を全artifactへ順に適用した合計時間。
        // NoteVecsはモデル未導入なのでCapabilityUnavailableで即返る。
        let (_, artifact_rebuild) = timed(|| {
            for artifact in crate::derived_index::DerivedArtifact::ALL {
                crate::derived_index::force_rebuild(&vault, &conn, artifact).unwrap();
            }
        });
        let post_rebuild = super::main_search(&conn, "検索番兵オーロラ", 20, false).unwrap();
        assert!(post_rebuild.iter().any(|hit| hit.id == target_id));

        let measurements = [
            ("index_rebuild", rebuild, Duration::from_secs(30)),
            ("home_db_read_x20", home_read, Duration::from_secs(2)),
            (
                "category_list_x20",
                category_list,
                Duration::from_millis(500),
            ),
            ("note_list_x5", note_list, Duration::from_secs(2)),
            (
                "keyword_search_x100",
                keyword_search,
                Duration::from_millis(500),
            ),
            (
                "semantic_search_x10",
                semantic_search,
                Duration::from_secs(1),
            ),
            ("note_detail_x5", note_detail, Duration::from_secs(2)),
            ("linked_context_x20", linked_context, Duration::from_secs(2)),
            ("warm_open_x5", warm_open, Duration::from_secs(2)),
            (
                "embed_pending_scan_x100",
                embed_pending_scan,
                Duration::from_millis(1500),
            ),
            (
                "note_update_median",
                update_median,
                Duration::from_millis(25),
            ),
            ("note_update_p95", update_p95, Duration::from_millis(50)),
            (
                "artifact_rebuild",
                artifact_rebuild,
                Duration::from_secs(10),
            ),
        ];
        for (name, elapsed, budget) in measurements {
            eprintln!(
                "performance_gate {name}: {} ms (budget {} ms)",
                elapsed.as_millis(),
                budget.as_millis()
            );
            assert!(
                elapsed <= budget,
                "{name} took {} ms; budget is {} ms",
                elapsed.as_millis(),
                budget.as_millis()
            );
        }
    }

    fn timed<T>(operation: impl FnOnce() -> T) -> (T, Duration) {
        let started = Instant::now();
        let value = operation();
        (value, started.elapsed())
    }

    fn repeat_last<T>(times: usize, mut operation: impl FnMut() -> T) -> T {
        let mut last = None;
        for _ in 0..times {
            last = Some(operation());
        }
        last.expect("性能fixtureは最低1回実行する")
    }

    fn performance_note_id(index: usize) -> String {
        format!(
            "notes/topic-{:03}/note-{index:05}",
            index % PERFORMANCE_CATEGORY_COUNT
        )
    }

    fn write_performance_fixture(vault: &Vault) {
        for index in 0..PERFORMANCE_NOTE_COUNT {
            let id = performance_note_id(index);
            let previous_link = if index > 0 {
                format!("\n\n[前のノート](/{}.md)", performance_note_id(index - 1))
            } else {
                String::new()
            };
            let marker = if index == PERFORMANCE_TARGET {
                "検索番兵オーロラ"
            } else {
                "標準知識"
            };
            let mut front = Frontmatter::new_note(&format!("性能fixture {index:05}"));
            front.description = Some(format!("10k回帰測定 category {}", index % 100));
            front.tags = vec!["performance".into(), format!("group-{}", index % 10)];
            front.origin = Some("agent".into());
            front.created = Some("2026-08-16T00:00:00Z".into());
            front.generated = Some(Generated {
                by: "test/performance-gate".into(),
                at: "2026-08-16T00:00:00Z".into(),
            });
            let note = Note {
                front,
                body: format!(
                    "{marker}。合成ナレッジ {index:05} の本文。性能回帰と検索品質を検査する。{previous_link}"
                ),
            };
            vault.write_note_fixture(&id, &note).unwrap();
        }
    }

    fn seed_performance_vectors(conn: &rusqlite::Connection) {
        let blobs: Vec<Vec<u8>> = (0..PERFORMANCE_VECTOR_DIM)
            .map(|axis| {
                let mut vector = vec![0.0f32; PERFORMANCE_VECTOR_DIM];
                vector[axis] = 1.0;
                crate::embed::to_blob(&vector)
            })
            .collect();
        let stamp = crate::embed::embedding_stamp("performance-fixture");
        let transaction = conn.unchecked_transaction().unwrap();
        {
            let mut insert = transaction
                .prepare("INSERT INTO note_vecs(id,stamp,embedding) VALUES (?1,?2,?3)")
                .unwrap();
            for index in 0..PERFORMANCE_NOTE_COUNT {
                insert
                    .execute(rusqlite::params![
                        performance_note_id(index),
                        stamp,
                        &blobs[index % PERFORMANCE_VECTOR_DIM]
                    ])
                    .unwrap();
            }
        }
        transaction.commit().unwrap();
    }
}
