//! 固定snapshotから全文と候補を組み立て、探索の予算と参照範囲を守る。

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use super::{
    MAX_CATALOG, MAX_CONTEXT_BYTES, MAX_DOCUMENTS, MAX_EXPLORATIONS, PermanentReviewError,
    ReviewInterruption, require_reviewable,
};
use crate::authority::{Authority, NoteRelation, NoteUid};
use crate::distillation::DistillationPlan;
use crate::distillation_jobs::{self, JobLease};
use crate::frontmatter::Note;

#[derive(Debug, Clone, Serialize)]
pub struct ReviewDocument {
    pub note: String,
    pub input_hash: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub body: String,
    pub tags: Vec<String>,
    pub authority: Option<Authority>,
    pub note_uid: Option<NoteUid>,
    pub relations: Vec<NoteRelation>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewCatalogEntry {
    pub note: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub authority: Option<Authority>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewContext {
    // 単件APIの互換用先頭ID。AIへのバッチ契約ではsources全件を判定する。
    pub source: String,
    pub sources: Vec<String>,
    pub snapshot_digest: String,
    pub documents: Vec<ReviewDocument>,
    pub catalog: Vec<ReviewCatalogEntry>,
    pub catalog_complete: bool,
    pub search_history: Vec<ReviewSearch>,
    pub remaining_explorations: usize,
    pub final_review_reason: Option<FinalReviewReason>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewSearch {
    pub query: String,
    pub matched_notes: Vec<String>,
    pub supplemental_notes: Vec<String>,
    // 検索APIには総hit数がない。0件/80件未満でも全KBの不存在証明ではない。
    pub coverage: SearchCoverage,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchCoverage {
    RankedCandidatesOnly,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FinalReviewReason {
    ExplorationBudgetUsed,
    NoNewEvidence,
}

pub(super) fn prepare_many(
    conn: &Connection,
    leases: &[JobLease],
    now: i64,
) -> Result<ReviewContext> {
    let first = leases.first().context("蒸留対象が空")?;
    let tx = conn.unchecked_transaction()?;
    for lease in leases {
        distillation_jobs::verify_lease_in_tx(&tx, lease, now)?;
    }
    let plan = crate::distillation::plan_in_transaction(&tx)?;
    let mut ids = leases
        .iter()
        .map(|lease| lease.note.clone())
        .collect::<Vec<_>>();
    if ids.iter().collect::<std::collections::BTreeSet<_>>().len() != ids.len() {
        bail!("蒸留対象が重複している");
    }
    let mut sources = Vec::new();
    for lease in leases {
        let source = crate::note_store::read(&tx, &lease.note)?;
        require_reviewable(&source)?;
        sources.push(source);
    }
    for entry in &plan.entries {
        if ids.len() >= leases.len() + 4 || ids.len() >= MAX_DOCUMENTS {
            break;
        }
        if ids.contains(&entry.note) || !normal_reference(&tx, &entry.note)? {
            continue;
        }
        let related = sources.iter().any(|source| {
            let source_scope = source.front.authority.as_ref().map(|a| a.scope.as_str());
            let same_scope = source_scope.is_some()
                && entry.authority.as_ref().map(|a| a.scope.as_str()) == source_scope;
            source
                .front
                .relations
                .iter()
                .any(|r| Some(r.target.as_str()) == entry.note_uid.as_deref())
                || same_scope
        });
        if related
            && entry
                .authority
                .as_ref()
                .is_some_and(|a| a.is_active_canonical())
        {
            ids.push(entry.note.clone());
        }
    }
    let query = sources
        .iter()
        .map(|source| {
            format!(
                "{} {}",
                source.front.title.as_deref().unwrap_or(""),
                source.front.description.as_deref().unwrap_or("")
            )
            .chars()
            .take(512 / leases.len().max(1))
            .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ");
    let candidates = catalog(&tx, &plan, &first.note, &query)?;
    let mut context = ReviewContext {
        source: first.note.clone(),
        sources: leases.iter().map(|lease| lease.note.clone()).collect(),
        snapshot_digest: plan.snapshot.digest,
        documents: Vec::new(),
        catalog: candidates.entries,
        catalog_complete: candidates.complete,
        search_history: candidates.search.into_iter().collect(),
        remaining_explorations: MAX_EXPLORATIONS,
        final_review_reason: None,
    };
    for id in ids {
        context.documents.push(read_document(&tx, &id)?);
    }
    check_context_size(&context)?;
    tx.rollback()?;
    Ok(context)
}

pub(super) fn extend_context_many(
    conn: &Connection,
    leases: &[JobLease],
    context: &mut ReviewContext,
    notes: &[String],
    search_query: Option<&str>,
    now: i64,
) -> Result<bool> {
    let lease = leases.first().context("蒸留対象が空")?;
    if context.remaining_explorations == 0 {
        return Err(PermanentReviewError::RoundLimit.into());
    }
    let tx = conn.unchecked_transaction()?;
    for member in leases {
        distillation_jobs::verify_lease_in_tx(&tx, member, now)?;
    }
    require_snapshot(&tx, &context.snapshot_digest)?;
    let mut next = context.clone();
    let unread: std::collections::BTreeSet<_> = notes
        .iter()
        .filter(|id| !next.documents.iter().any(|document| &document.note == *id))
        .collect();
    if next.documents.len() + unread.len() > MAX_DOCUMENTS {
        return Err(PermanentReviewError::ContextSizeLimit.into());
    }
    if let Some(query) = search_query {
        if query.trim().is_empty() || query.chars().count() > 512 {
            bail!("追加検索は1〜512文字で指定する");
        }
        let query = normalize_query(query);
        if !next.catalog_complete && !next.search_history.iter().any(|past| past.query == query) {
            let plan = crate::distillation::plan_in_transaction(&tx)?;
            let candidates = catalog(&tx, &plan, &lease.note, &query)?;
            next.catalog = candidates.entries;
            next.catalog_complete = candidates.complete;
            next.search_history.extend(candidates.search);
        }
    }
    check_context_size(&next)?;
    for id in notes {
        if !context
            .catalog
            .iter()
            .chain(next.catalog.iter())
            .any(|e| &e.note == id)
            && !next.search_history.iter().any(|search| {
                search
                    .matched_notes
                    .iter()
                    .chain(&search.supplemental_notes)
                    .any(|known| known == id)
            })
            && !context.documents.iter().any(|d| &d.note == id)
        {
            bail!("確認範囲の外のノートは取得できない");
        }
        if !next.documents.iter().any(|d| &d.note == id) {
            next.documents.push(read_document(&tx, id)?);
            check_context_size(&next)?;
        }
    }
    let progressed = next.search_history.len() > context.search_history.len()
        || next.documents.len() > context.documents.len();
    if progressed {
        next.remaining_explorations -= 1;
        if next.remaining_explorations == 0 {
            next.final_review_reason = Some(FinalReviewReason::ExplorationBudgetUsed);
        }
    }
    check_context_size(&next)?;
    tx.rollback()?;
    *context = next;
    Ok(progressed)
}

struct CatalogResult {
    entries: Vec<ReviewCatalogEntry>,
    complete: bool,
    search: Option<ReviewSearch>,
}

fn normalize_query(query: &str) -> String {
    query.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn catalog(
    conn: &Connection,
    plan: &DistillationPlan,
    source: &str,
    query: &str,
) -> Result<CatalogResult> {
    let visible = plan
        .entries
        .iter()
        .filter_map(|e| match normal_reference(conn, &e.note) {
            Ok(true) => Some(Ok(e)),
            Ok(false) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>>>()?;
    let complete = visible.len() <= MAX_CATALOG;
    let mut search = None;
    let ids = if complete {
        visible.iter().map(|e| e.note.clone()).collect::<Vec<_>>()
    } else {
        let query = normalize_query(&query.chars().take(512).collect::<String>());
        let found = crate::search::search_for_note(conn, &query, MAX_CATALOG, source);
        if !found.degraded.is_empty() {
            bail!("蒸留の候補検索が劣化しているため再試行する");
        }
        let mut ids = found.hits.into_iter().map(|h| h.id).collect::<Vec<_>>();
        let matched_notes = ids.clone();
        let mut supplemental_notes = Vec::new();
        // 検索0件でも同scopeの正本を見失わない。残りは次のsearch_queryで取得できる。
        let scope = visible
            .iter()
            .find(|e| e.note == source)
            .and_then(|e| e.authority.as_ref())
            .map(|a| a.scope.as_str());
        for entry in &visible {
            if ids.len() >= MAX_CATALOG {
                break;
            }
            if entry
                .authority
                .as_ref()
                .is_some_and(|a| Some(a.scope.as_str()) == scope)
                && !ids.contains(&entry.note)
            {
                ids.push(entry.note.clone());
                supplemental_notes.push(entry.note.clone());
            }
        }
        search = Some(ReviewSearch {
            query,
            matched_notes,
            supplemental_notes,
            coverage: SearchCoverage::RankedCandidatesOnly,
        });
        ids
    };
    let mut result = Vec::new();
    for id in ids {
        let entry = visible
            .iter()
            .find(|e| e.note == id)
            .context("候補ノートがsnapshotにない")?;
        let description: Option<String> =
            conn.query_row("SELECT description FROM notes WHERE id=?1", [&id], |r| {
                r.get(0)
            })?;
        // 候補は手掛かりなので上限を持つ。判断に使う本文は必ず全文取得する。
        result.push(ReviewCatalogEntry {
            note: id,
            title: entry.title.as_ref().map(|s| s.chars().take(160).collect()),
            description: description.map(|s| s.chars().take(320).collect()),
            authority: entry.authority.clone(),
        });
    }
    Ok(CatalogResult {
        entries: result,
        complete,
        search,
    })
}

pub(super) fn read_document(conn: &Connection, id: &str) -> Result<ReviewDocument> {
    if !normal_reference(conn, id)? {
        bail!("通常参照できないノートは蒸留対象にできない");
    }
    let document: String =
        conn.query_row("SELECT document FROM notes WHERE id=?1", [id], |r| r.get(0))?;
    let note = Note::parse(&document)?;
    Ok(ReviewDocument {
        note: id.into(),
        input_hash: crate::distillation::sha256(document.as_bytes()),
        title: note.front.title,
        description: note.front.description,
        body: note.body,
        tags: note.front.tags,
        authority: note.front.authority,
        note_uid: note.front.note_uid,
        relations: note.front.relations,
    })
}

fn normal_reference(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT normal_reference_allowed=1 FROM notes WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

pub(super) fn check_context_size(context: &ReviewContext) -> Result<()> {
    if context.documents.len() > MAX_DOCUMENTS
        || serde_json::to_vec(context)?.len() > MAX_CONTEXT_BYTES
    {
        return Err(PermanentReviewError::ContextSizeLimit.into());
    }
    Ok(())
}

pub(super) fn require_snapshot(conn: &Connection, digest: &str) -> Result<()> {
    if crate::distillation::plan_in_transaction(conn)?
        .snapshot
        .digest
        != digest
    {
        return Err(ReviewInterruption::SnapshotChanged.into());
    }
    Ok(())
}
