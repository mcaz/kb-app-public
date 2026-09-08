//! 壊れたruntime storeを変更する前に、現存候補と最新性の根拠を固定する。
//! 存在する版を読み戻せることと、事故前の最新版が揃っていることを区別し、適用は行わない。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use anyhow::{Result, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension, types::ValueRef};
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use crate::frontmatter::Note;
use crate::note_id::NoteId;
use crate::vault::Vault;

mod apply;
pub use apply::{
    RuntimeRecoveryFailure, RuntimeRecoveryFailureKind, RuntimeRecoveryLedgerReceipt,
    RuntimeRecoveryReceipt, RuntimeRecoveryRequest, apply, failure_kind,
};

// 数万件での異常も集計を保持し、JSON内の個別情報だけを制限する。
const DETAIL_LIMIT: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RuntimeRecoveryPlan {
    pub format_version: u32,
    pub read_only: bool,
    pub recovery_performed: bool,
    /// DBとMarkdown群をまたぐ原子的snapshotは取得していない。
    pub atomic_snapshot: bool,
    pub declared_schema: Option<u32>,
    pub supported_reset_shape: bool,
    /// 固定対象を完読・検証できたこと。観測終了後の変更までは保証しない。
    pub snapshot_complete: bool,
    pub plan_digest: Option<String>,
    /// 現存Markdown全件を読め、全current jobを含み、構造・参照を検証できたこと。
    /// reset対象への該当やblockerとは独立した被覆の検査であり、適用許可ではない。
    pub existing_markdown_coverage_complete: bool,
    /// 観測した全ノートがcurrent completed hashに一致した場合だけtrue。
    /// 観測中・観測後の全保存場所の最新版保証や適用許可ではない。
    pub latest_state_proven: bool,
    pub markdown_notes: Option<u64>,
    pub job_notes: Option<u64>,
    pub history_runs: Option<u64>,
    pub summary: Option<RecoveryEvidenceSummary>,
    pub notes: Vec<RecoveryNoteEvidence>,
    pub note_count: u64,
    pub notes_truncated: bool,
    pub issues: Vec<RecoveryPlanIssue>,
    pub issue_count: u64,
    pub blocking_issue_count: u64,
    pub issues_truncated: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RecoveryEvidenceSummary {
    pub markdown_with_job: u64,
    pub markdown_without_job: u64,
    pub eligible_markdown: u64,
    pub eligible_markdown_without_job: u64,
    pub jobs_without_markdown: u64,
    pub completed_review_matches: u64,
    pub completed_review_conflicts: u64,
    pub previous_review_matches: u64,
    pub history_after_matches: u64,
    pub history_before_matches: u64,
    pub no_matching_history: u64,
    pub latest_state_unproven: u64,
    pub current_review_available_in_history: u64,
    /// 履歴だけに残るIDは、削除済みかもしれないので復元対象と認定しない。
    pub historical_only_notes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RecoveryNoteEvidence {
    pub note: String,
    pub markdown_present: bool,
    pub eligible: Option<bool>,
    pub job_present: bool,
    pub job_state: Option<RecoveryJobState>,
    pub reviewed_hash_matches_markdown: Option<bool>,
    pub history_after_matches_markdown: bool,
    pub history_before_matches_markdown: bool,
    pub history_after_versions: u64,
    pub history_before_versions: u64,
    pub current_review_source: RecoveryCurrentSource,
    pub freshness: RecoveryFreshness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum RecoveryJobState {
    Pending,
    Running,
    RetryWait,
    Completed,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum RecoveryCurrentSource {
    Markdown,
    HistoryAfter,
    HistoryBefore,
    Unavailable,
    Unproven,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum RecoveryFreshness {
    CurrentCompletedReview,
    ConflictsWithCompletedReview,
    MatchesPreviousReview,
    ObservedInAfterHistory,
    ObservedOnlyInBeforeHistory,
    NoLatestVersionEvidence,
    EligibleWithoutJob,
    MissingMarkdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RecoveryPlanIssue {
    pub code: String,
    pub blocking: bool,
    pub note: Option<String>,
}

impl RuntimeRecoveryPlan {
    fn new() -> Self {
        Self {
            format_version: 1,
            read_only: true,
            recovery_performed: false,
            atomic_snapshot: false,
            declared_schema: None,
            supported_reset_shape: false,
            snapshot_complete: false,
            plan_digest: None,
            existing_markdown_coverage_complete: false,
            latest_state_proven: false,
            markdown_notes: None,
            job_notes: None,
            history_runs: None,
            summary: None,
            notes: Vec::new(),
            note_count: 0,
            notes_truncated: false,
            issues: Vec::new(),
            issue_count: 0,
            blocking_issue_count: 0,
            issues_truncated: false,
        }
    }

    fn issue(&mut self, code: &str, blocking: bool, note: Option<&str>) {
        self.issue_count += 1;
        self.blocking_issue_count += u64::from(blocking);
        if self.issues.len() < DETAIL_LIMIT {
            self.issues.push(RecoveryPlanIssue {
                code: code.into(),
                blocking,
                note: note
                    .filter(|id| NoteId::parse(id).is_ok())
                    .map(str::to_owned),
            });
        } else {
            self.issues_truncated = true;
        }
    }
}

struct MarkdownSource {
    document_hash: String,
    parsed: Note,
    eligible: bool,
}

struct CurrentJob {
    state: RecoveryJobState,
    reviewed_hash: Option<String>,
}

#[derive(Default)]
struct HistoricalVersions {
    before: BTreeSet<String>,
    after: BTreeSet<String>,
}

struct DatabaseSource {
    digest: Vec<u8>,
    jobs: BTreeMap<String, CurrentJob>,
    history: BTreeMap<String, HistoricalVersions>,
}

/// 専用の診断入口であり、通常open・migration・Markdown importは呼ばない。
pub fn plan(vault: &Vault) -> Result<RuntimeRecoveryPlan> {
    let mut report = RuntimeRecoveryPlan::new();
    let mut conn = match Connection::open_with_flags(
        vault.index_db_path(),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(conn) => conn,
        Err(_) => {
            report.issue("database_open_failed", true, None);
            return Ok(report);
        }
    };
    if conn
        .busy_timeout(std::time::Duration::from_secs(2))
        .and_then(|()| conn.execute_batch("PRAGMA query_only=ON;"))
        .is_err()
    {
        report.issue("read_only_configuration_failed", true, None);
        return Ok(report);
    }
    let tx = match conn.transaction() {
        Ok(tx) => tx,
        Err(_) => {
            report.issue("read_transaction_failed", true, None);
            return Ok(report);
        }
    };
    let source = read_database(&tx, &mut report);
    let markdown = read_markdown(vault, &mut report);
    if let (Ok(source), Some((markdown, markdown_digest))) = (source, markdown) {
        report.plan_digest = Some(snapshot_digest(&source.digest, &markdown_digest));
        report.snapshot_complete = true;
        assess(&source, &markdown, &mut report);
    } else {
        report.issue("source_snapshot_incomplete", true, None);
    }
    if tx.rollback().is_err() {
        report.snapshot_complete = false;
        report.latest_state_proven = false;
        report.plan_digest = None;
        report.issue("read_transaction_close_failed", true, None);
    }
    Ok(report)
}

fn snapshot_digest(database: &[u8], markdown: &[u8]) -> String {
    let mut digest = Sha256::new();
    hash_field(&mut digest, b"kb-app.runtime-recovery-plan/v1");
    hash_field(&mut digest, database);
    hash_field(&mut digest, markdown);
    format!("sha256:{:x}", digest.finalize())
}

fn read_database(conn: &Connection, report: &mut RuntimeRecoveryPlan) -> Result<DatabaseSource> {
    let result = read_database_inner(conn, report);
    if result.is_err() {
        report.issue("database_snapshot_read_failed", true, None);
    }
    result
}

fn read_database_inner(
    conn: &Connection,
    report: &mut RuntimeRecoveryPlan,
) -> Result<DatabaseSource> {
    let mut digest = Sha256::new();
    // schemaを最初に読むことで、後続の台帳検査も同じSQLite snapshotへ固定する。
    hash_query(
        conn,
        "SELECT type, name, tbl_name, sql FROM sqlite_schema",
        &mut digest,
    )?;
    for table in crate::derived_index::DURABLE_STATE_TABLES {
        hash_field(&mut digest, table.as_bytes());
        hash_query(conn, &format!("SELECT * FROM {table}"), &mut digest)?;
    }
    let declared: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key='schema'", [], |row| {
            row.get(0)
        })
        .optional()?;
    report.declared_schema = declared
        .as_deref()
        .and_then(|value| value.trim().parse::<u32>().ok());
    let runtime_marker_rows: i64 = conn.query_row(
        "SELECT count(*) FROM meta WHERE key='runtime_store'",
        [],
        |row| row.get(0),
    )?;
    let notes: i64 = conn.query_row("SELECT count(*) FROM notes", [], |row| row.get(0))?;
    let exports: i64 = conn.query_row("SELECT count(*) FROM note_exports", [], |row| row.get(0))?;
    let notes_columns = columns(conn, "notes")?;
    let expected_notes: BTreeSet<_> = [
        "id",
        "title",
        "description",
        "status",
        "origin",
        "generated_by",
        "generated_at",
        "mtime",
        "body",
        "tags",
        "created",
        "document",
        "note_uid",
        "namespace",
        "authority_role",
        "authority_status",
        "authority_scope",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let job_columns: BTreeSet<_> = [
        "note",
        "generation",
        "state",
        "reason",
        "queued_at",
        "available_at",
        "attempt",
        "lease_token",
        "lease_expires_at",
        "last_reviewed_at",
        "reviewed_hash",
        "outcome",
        "last_error",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let run_columns: BTreeSet<_> = [
        "run_id",
        "note",
        "generation",
        "reviewed_at",
        "outcome",
        "reason",
        "snapshot_digest",
        "before_documents",
        "after_documents",
        "client",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    report.supported_reset_shape = matches!(report.declared_schema, Some(7 | 9))
        && runtime_marker_rows == 0
        && notes == 0
        && exports == 0
        && notes_columns == expected_notes
        && columns(conn, "distillation_jobs")? == job_columns
        && columns(conn, "distillation_job_runs")? == run_columns;
    if !report.supported_reset_shape {
        report.issue("unsupported_reset_shape", true, None);
    }
    let jobs = read_jobs(conn, report)?;
    let history = read_history(conn, report)?;
    Ok(DatabaseSource {
        digest: digest.finalize().to_vec(),
        jobs,
        history,
    })
}

fn columns(conn: &Connection, table: &str) -> rusqlite::Result<BTreeSet<String>> {
    // 呼出し箇所の固定名だけを受け、外部APIからSQL/pathは受けない。
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    statement.query_map([], |row| row.get(1))?.collect()
}

fn hash_field(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

fn hash_query(conn: &Connection, sql: &str, digest: &mut Sha256) -> rusqlite::Result<()> {
    let mut statement = conn.prepare(sql)?;
    let count = statement.column_count();
    let mut rows = statement.query([])?;
    let mut row_hashes = Vec::new();
    while let Some(row) = rows.next()? {
        let mut hash = Sha256::new();
        for i in 0..count {
            match row.get_ref(i)? {
                ValueRef::Null => hash.update([0]),
                ValueRef::Integer(value) => {
                    hash.update([1]);
                    hash.update(value.to_le_bytes());
                }
                ValueRef::Real(value) => {
                    hash.update([2]);
                    hash.update(value.to_bits().to_le_bytes());
                }
                ValueRef::Text(value) => {
                    hash.update([3]);
                    hash_field(&mut hash, value);
                }
                ValueRef::Blob(value) => {
                    hash.update([4]);
                    hash_field(&mut hash, value);
                }
            }
        }
        row_hashes.push(hash.finalize().to_vec());
    }
    row_hashes.sort();
    digest.update((row_hashes.len() as u64).to_le_bytes());
    for hash in row_hashes {
        hash_field(digest, &hash);
    }
    Ok(())
}

fn read_jobs(
    conn: &Connection,
    report: &mut RuntimeRecoveryPlan,
) -> Result<BTreeMap<String, CurrentJob>> {
    let mut statement = conn.prepare(
        "SELECT note,state,generation,reviewed_hash FROM distillation_jobs ORDER BY note",
    )?;
    let mut rows = statement.query([])?;
    let mut jobs = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let state: String = row.get(1)?;
        let generation: i64 = row.get(2)?;
        let reviewed_hash: Option<String> = row.get(3)?;
        let state = match state.as_str() {
            "pending" => RecoveryJobState::Pending,
            "running" => RecoveryJobState::Running,
            "retry_wait" => RecoveryJobState::RetryWait,
            "completed" => RecoveryJobState::Completed,
            "blocked" => RecoveryJobState::Blocked,
            _ => {
                report.issue("job_state_invalid", true, Some(&id));
                bail!("蒸留jobの状態が不正");
            }
        };
        if NoteId::parse(&id).is_err()
            || generation <= 0
            || reviewed_hash
                .as_deref()
                .is_some_and(|hash| !valid_hash(hash))
        {
            report.issue("job_record_invalid", true, Some(&id));
            bail!("蒸留jobの版情報が不正");
        }
        if state == RecoveryJobState::Completed && reviewed_hash.is_none() {
            report.issue("completed_job_hash_missing", true, Some(&id));
        }
        if jobs
            .insert(
                id,
                CurrentJob {
                    state,
                    reviewed_hash,
                },
            )
            .is_some()
        {
            bail!("蒸留jobのIDが重複");
        }
    }
    report.job_notes = Some(jobs.len() as u64);
    Ok(jobs)
}

fn valid_hash(hash: &str) -> bool {
    hash.strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn read_history(
    conn: &Connection,
    report: &mut RuntimeRecoveryPlan,
) -> Result<BTreeMap<String, HistoricalVersions>> {
    let mut statement = conn.prepare(
        "SELECT note,before_documents,after_documents,generation,reviewed_at,outcome FROM distillation_job_runs ORDER BY run_id",
    )?;
    let mut rows = statement.query([])?;
    let mut history = BTreeMap::<String, HistoricalVersions>::new();
    let mut count = 0;
    while let Some(row) = rows.next()? {
        count += 1;
        let source: String = row.get(0)?;
        let generation: i64 = row.get(3)?;
        let reviewed_at: i64 = row.get(4)?;
        let outcome: String = row.get(5)?;
        if NoteId::parse(&source).is_err()
            || generation <= 0
            || reviewed_at < 0
            || !matches!(outcome.as_str(), "applied" | "no_change")
        {
            report.issue("history_record_invalid", true, Some(&source));
            bail!("蒸留履歴の記録が不正");
        }
        for (column, after) in [(1, false), (2, true)] {
            let raw: String = row.get(column)?;
            let documents = match parse_history_map(&raw) {
                Ok(documents) => documents,
                Err(_) => {
                    report.issue("history_document_map_invalid", true, Some(&source));
                    bail!("蒸留履歴のJSONが不正");
                }
            };
            for (id, document) in documents {
                if NoteId::parse(&id).is_err() || Note::parse(&document).is_err() {
                    report.issue("history_document_invalid", true, Some(&id));
                    bail!("蒸留履歴のノートが不正");
                }
                let versions = history.entry(id).or_default();
                let hashes = if after {
                    &mut versions.after
                } else {
                    &mut versions.before
                };
                hashes.insert(crate::distillation::sha256(document.as_bytes()));
            }
        }
    }
    report.history_runs = Some(count);
    Ok(history)
}

fn parse_history_map(raw: &str) -> Result<BTreeMap<String, String>> {
    struct UniqueDocuments;
    impl<'de> Visitor<'de> for UniqueDocuments {
        type Value = BTreeMap<String, String>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("一意なIDとdocumentのmap")
        }
        fn visit_map<M: MapAccess<'de>>(
            self,
            mut map: M,
        ) -> std::result::Result<Self::Value, M::Error> {
            let mut documents = BTreeMap::new();
            while let Some((id, document)) = map.next_entry::<String, String>()? {
                if documents.insert(id, document).is_some() {
                    return Err(serde::de::Error::custom("重複した履歴ID"));
                }
            }
            Ok(documents)
        }
    }
    let mut deserializer = serde_json::Deserializer::from_str(raw);
    let parsed = deserializer.deserialize_map(UniqueDocuments)?;
    deserializer.end()?;
    Ok(parsed)
}

fn read_markdown(
    vault: &Vault,
    report: &mut RuntimeRecoveryPlan,
) -> Option<(BTreeMap<String, MarkdownSource>, Vec<u8>)> {
    let files = match vault.list_note_files() {
        Ok(files) => files,
        Err(_) => {
            report.issue("markdown_scan_failed", true, None);
            return None;
        }
    };
    report.markdown_notes = Some(files.len() as u64);
    let mut sources = BTreeMap::new();
    let mut digest = Sha256::new();
    let mut complete = true;
    let mut stamps = Vec::new();
    for (id, path) in &files {
        let read = (|| -> Result<_> {
            NoteId::parse(id)?;
            let before = fs::metadata(path)?;
            let raw = fs::read_to_string(path)?;
            let after = fs::metadata(path)?;
            if before.len() != after.len() || before.modified()? != after.modified()? {
                bail!("走査中にノートが変わった");
            }
            Ok((raw, after.len(), after.modified()?))
        })();
        let (raw, length, modified) = match read {
            Ok(read) => read,
            Err(_) => {
                report.issue("markdown_read_unstable_or_failed", true, Some(id));
                complete = false;
                continue;
            }
        };
        stamps.push((id, path, length, modified));
        hash_field(&mut digest, id.as_bytes());
        hash_field(&mut digest, raw.as_bytes());
        match Note::parse(&raw) {
            Ok(parsed) => {
                let eligible = crate::distillation_jobs::derive_allowed(&parsed);
                sources.insert(
                    id.clone(),
                    MarkdownSource {
                        document_hash: crate::distillation::sha256(raw.as_bytes()),
                        parsed,
                        eligible,
                    },
                );
            }
            Err(_) => {
                report.issue("markdown_document_invalid", true, Some(id));
                complete = false;
            }
        }
    }
    let after = vault.list_note_files();
    if !after.is_ok_and(|after| after == files) {
        report.issue("markdown_inventory_unstable", true, None);
        complete = false;
    }
    for (id, path, length, modified) in stamps {
        let stable = fs::metadata(path).is_ok_and(|metadata| {
            metadata.len() == length && metadata.modified().is_ok_and(|after| after == modified)
        });
        if !stable {
            report.issue("markdown_changed_during_scan", true, Some(id));
            complete = false;
        }
    }
    if !complete {
        return None;
    }
    let notes: Vec<_> = sources
        .iter()
        .map(|(id, source)| crate::storage_contract::SnapshotNote {
            id: id.clone(),
            frontmatter: source.parsed.front.clone(),
            body: source.parsed.body.clone(),
        })
        .collect();
    if crate::storage_contract::validate_note_authority(&notes).is_err() {
        report.issue("markdown_authority_or_relations_invalid", true, None);
        return None;
    }
    Some((sources, digest.finalize().to_vec()))
}

fn assess(
    source: &DatabaseSource,
    markdown: &BTreeMap<String, MarkdownSource>,
    report: &mut RuntimeRecoveryPlan,
) {
    let mut summary = RecoveryEvidenceSummary::default();
    let ids: BTreeSet<_> = markdown.keys().chain(source.jobs.keys()).cloned().collect();
    for id in &ids {
        let md = markdown.get(id);
        let job = source.jobs.get(id);
        let history = source.history.get(id);
        let matches_review = md.zip(job).and_then(|(md, job)| {
            job.reviewed_hash
                .as_ref()
                .map(|hash| hash == &md.document_hash)
        });
        let after_matches = md
            .zip(history)
            .is_some_and(|(md, history)| history.after.contains(&md.document_hash));
        let before_matches = md
            .zip(history)
            .is_some_and(|(md, history)| history.before.contains(&md.document_hash));
        let current_source = match job {
            Some(CurrentJob {
                state: RecoveryJobState::Completed,
                reviewed_hash: Some(hash),
            }) => {
                if md.is_some_and(|md| &md.document_hash == hash) {
                    RecoveryCurrentSource::Markdown
                } else if history.is_some_and(|history| history.after.contains(hash)) {
                    RecoveryCurrentSource::HistoryAfter
                } else if history.is_some_and(|history| history.before.contains(hash)) {
                    RecoveryCurrentSource::HistoryBefore
                } else {
                    RecoveryCurrentSource::Unavailable
                }
            }
            _ => RecoveryCurrentSource::Unproven,
        };
        let freshness = if md.is_none() {
            report.issue("current_job_markdown_missing", true, Some(id));
            RecoveryFreshness::MissingMarkdown
        } else if md.is_some_and(|md| md.eligible) && job.is_none() {
            report.issue(
                "eligible_markdown_without_job_may_be_pending_deletion",
                true,
                Some(id),
            );
            RecoveryFreshness::EligibleWithoutJob
        } else if current_source == RecoveryCurrentSource::Markdown {
            RecoveryFreshness::CurrentCompletedReview
        } else if job.is_some_and(|job| job.state == RecoveryJobState::Completed) {
            report.issue("markdown_differs_from_completed_review", true, Some(id));
            RecoveryFreshness::ConflictsWithCompletedReview
        } else if matches_review == Some(true) {
            RecoveryFreshness::MatchesPreviousReview
        } else if after_matches {
            RecoveryFreshness::ObservedInAfterHistory
        } else if before_matches {
            RecoveryFreshness::ObservedOnlyInBeforeHistory
        } else {
            RecoveryFreshness::NoLatestVersionEvidence
        };
        if job.is_some() && md.is_some_and(|md| !md.eligible) {
            report.issue("current_job_markdown_ineligible", true, Some(id));
        }
        summary.markdown_with_job += u64::from(md.is_some() && job.is_some());
        summary.markdown_without_job += u64::from(md.is_some() && job.is_none());
        summary.eligible_markdown += u64::from(md.is_some_and(|md| md.eligible));
        summary.eligible_markdown_without_job +=
            u64::from(md.is_some_and(|md| md.eligible) && job.is_none());
        summary.jobs_without_markdown += u64::from(md.is_none() && job.is_some());
        summary.completed_review_matches +=
            u64::from(freshness == RecoveryFreshness::CurrentCompletedReview);
        summary.completed_review_conflicts +=
            u64::from(freshness == RecoveryFreshness::ConflictsWithCompletedReview);
        summary.previous_review_matches +=
            u64::from(freshness == RecoveryFreshness::MatchesPreviousReview);
        summary.history_after_matches += u64::from(after_matches);
        summary.history_before_matches += u64::from(before_matches);
        summary.no_matching_history += u64::from(md.is_some() && !after_matches && !before_matches);
        summary.latest_state_unproven +=
            u64::from(freshness != RecoveryFreshness::CurrentCompletedReview);
        summary.current_review_available_in_history += u64::from(matches!(
            current_source,
            RecoveryCurrentSource::HistoryAfter | RecoveryCurrentSource::HistoryBefore
        ));
        report.note_count += 1;
        if report.notes.len() < DETAIL_LIMIT {
            report.notes.push(RecoveryNoteEvidence {
                note: id.clone(),
                markdown_present: md.is_some(),
                eligible: md.map(|md| md.eligible),
                job_present: job.is_some(),
                job_state: job.map(|job| job.state),
                reviewed_hash_matches_markdown: matches_review,
                history_after_matches_markdown: after_matches,
                history_before_matches_markdown: before_matches,
                history_after_versions: history.map_or(0, |history| history.after.len() as u64),
                history_before_versions: history.map_or(0, |history| history.before.len() as u64),
                current_review_source: current_source,
                freshness,
            });
        } else {
            report.notes_truncated = true;
        }
    }
    summary.historical_only_notes = source
        .history
        .keys()
        .filter(|id| !ids.contains(*id))
        .count() as u64;
    report.existing_markdown_coverage_complete = summary.jobs_without_markdown == 0
        && summary.eligible_markdown_without_job == 0
        && source
            .jobs
            .keys()
            .all(|id| markdown.get(id).is_some_and(|md| md.eligible))
        && !markdown.is_empty();
    report.latest_state_proven = summary.latest_state_unproven == 0
        && report.supported_reset_shape
        && report.snapshot_complete
        && report.blocking_issue_count == 0
        && !ids.is_empty();
    if summary.latest_state_unproven > 0 {
        report.issue(
            "latest_version_not_proven_does_not_mean_data_lost",
            false,
            None,
        );
    }
    report.summary = Some(summary);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontmatter::Frontmatter;
    use rusqlite::params;

    fn setup(schema: u32) -> (tempfile::TempDir, Vault, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        conn.execute_batch(
            "DROP TRIGGER distillation_jobs_insert;
             DROP TRIGGER distillation_jobs_update;
             DROP TRIGGER distillation_jobs_hide;
             DROP TRIGGER distillation_jobs_delete;
             ALTER TABLE notes DROP COLUMN normal_reference_allowed;
             ALTER TABLE notes DROP COLUMN distillation_allowed;
             DELETE FROM meta WHERE key='runtime_store';",
        )
        .unwrap();
        conn.execute(
            "UPDATE meta SET value=?1 WHERE key='schema'",
            [schema.to_string()],
        )
        .unwrap();
        conn.execute_batch("PRAGMA journal_mode=DELETE;").unwrap();
        (dir, vault, conn)
    }

    fn document(body: &str, eligible: bool) -> String {
        let mut front = Frontmatter::new_note("復旧候補fixture");
        front.tags = vec!["test".into()];
        front.origin = Some(if eligible { "agent" } else { "human" }.into());
        Note {
            front,
            body: body.into(),
        }
        .to_file_string()
        .unwrap()
    }

    fn md(vault: &Vault, id: &str, raw: &str) {
        fs::write(vault.root.join(format!("{id}.md")), raw).unwrap();
    }

    fn job(conn: &Connection, id: &str, state: &str, reviewed: Option<&str>) {
        conn.execute(
            "INSERT INTO distillation_jobs(note,generation,state,reason,queued_at,available_at,reviewed_hash)
             VALUES(?1,3,?2,'fixture',100,0,?3)",
            params![id,state,reviewed.map(|raw| crate::distillation::sha256(raw.as_bytes()))],
        ).unwrap();
    }

    fn history(
        conn: &Connection,
        run: &str,
        source: &str,
        before: &[(&str, &str)],
        after: &[(&str, &str)],
    ) {
        let before: BTreeMap<_, _> = before.iter().copied().collect();
        let after: BTreeMap<_, _> = after.iter().copied().collect();
        conn.execute(
            "INSERT INTO distillation_job_runs VALUES(?1,?2,2,90,'applied','fixture','sha256:fixture',?3,?4,'test')",
            params![run,source,serde_json::to_string(&before).unwrap(),serde_json::to_string(&after).unwrap()],
        ).unwrap();
    }

    /// 2026-09-07: completedの現行hashとpendingに残った前回hashを混同しない。
    #[test]
    fn distinguishes_current_review_previous_review_and_history_without_mutation() {
        let (_dir, vault, conn) = setup(7);
        let current = document("private current document", true);
        let previous = document("private previous document", true);
        let observed = document("private historical document", true);
        let human = document("private human document", false);
        for (id, raw) in [
            ("notes/current", &current),
            ("notes/pending", &previous),
            ("notes/historical", &observed),
            ("notes/human", &human),
        ] {
            md(&vault, id, raw);
        }
        job(&conn, "notes/current", "completed", Some(&current));
        job(&conn, "notes/pending", "running", Some(&previous));
        job(&conn, "notes/historical", "pending", None);
        history(
            &conn,
            "run1",
            "notes/current",
            &[("notes/pending", &previous)],
            &[("notes/current", &current), ("notes/historical", &observed)],
        );
        // 一括処理の各sourceに複製されたJSONは別versionと数えない。
        history(
            &conn,
            "run2",
            "notes/historical",
            &[("notes/pending", &previous)],
            &[("notes/current", &current), ("notes/historical", &observed)],
        );
        drop(conn);
        let before = fs::read(vault.index_db_path()).unwrap();
        let modified = fs::metadata(vault.index_db_path())
            .unwrap()
            .modified()
            .unwrap();
        let report = plan(&vault).unwrap();
        assert!(report.read_only && !report.recovery_performed && !report.atomic_snapshot);
        assert!(report.supported_reset_shape && report.snapshot_complete);
        assert!(report.existing_markdown_coverage_complete);
        assert!(!report.latest_state_proven);
        assert_eq!(report.markdown_notes, Some(4));
        assert_eq!(report.job_notes, Some(3));
        assert_eq!(report.history_runs, Some(2));
        let summary = report.summary.as_ref().unwrap();
        assert_eq!(summary.completed_review_matches, 1);
        assert_eq!(summary.previous_review_matches, 1);
        assert_eq!(summary.latest_state_unproven, 3);
        assert_eq!(summary.history_after_matches, 2);
        let current = report
            .notes
            .iter()
            .find(|note| note.note == "notes/current")
            .unwrap();
        assert_eq!(current.freshness, RecoveryFreshness::CurrentCompletedReview);
        assert_eq!(current.history_after_versions, 1);
        let pending = report
            .notes
            .iter()
            .find(|note| note.note == "notes/pending")
            .unwrap();
        assert_eq!(
            pending.current_review_source,
            RecoveryCurrentSource::Unproven
        );
        assert_eq!(pending.freshness, RecoveryFreshness::MatchesPreviousReview);
        assert_eq!(fs::read(vault.index_db_path()).unwrap(), before);
        assert_eq!(
            fs::metadata(vault.index_db_path())
                .unwrap()
                .modified()
                .unwrap(),
            modified
        );
        assert_eq!(
            fs::read_to_string(vault.root.join("notes/human.md")).unwrap(),
            human
        );
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("private"));
        assert!(!json.contains(&vault.root.display().to_string()));
        assert!(!json.contains("CREATE TABLE"));
        assert_eq!(report.plan_digest, plan(&vault).unwrap().plan_digest);
    }

    #[test]
    fn completed_conflict_can_identify_a_historical_current_version_without_applying_it() {
        let (_dir, vault, conn) = setup(9);
        let old = document("old", true);
        let current = document("current", true);
        md(&vault, "notes/stale", &old);
        job(&conn, "notes/stale", "completed", Some(&current));
        history(
            &conn,
            "run",
            "notes/stale",
            &[("notes/stale", &old)],
            &[("notes/stale", &current)],
        );
        drop(conn);
        let report = plan(&vault).unwrap();
        assert!(report.existing_markdown_coverage_complete);
        assert!(!report.latest_state_proven);
        assert_eq!(
            report.notes[0].current_review_source,
            RecoveryCurrentSource::HistoryAfter
        );
        assert_eq!(
            report.notes[0].freshness,
            RecoveryFreshness::ConflictsWithCompletedReview
        );
        assert_eq!(
            report
                .summary
                .as_ref()
                .unwrap()
                .current_review_available_in_history,
            1
        );
        assert!(report.blocking_issue_count > 0);
        assert_eq!(
            fs::read_to_string(vault.root.join("notes/stale.md")).unwrap(),
            old
        );
    }

    /// 2026-09-07: jobが無いeligible MDを、削除待ちの残骸でないと決めつけない。
    #[test]
    fn distinguishes_untracked_eligible_notes_missing_jobs_and_historical_only_notes() {
        let (_dir, vault, conn) = setup(7);
        let raw = document("record", true);
        md(&vault, "notes/untracked", &raw);
        job(&conn, "notes/missing", "completed", Some(&raw));
        history(
            &conn,
            "run",
            "notes/missing",
            &[("notes/missing", &raw), ("notes/old-deleted", &raw)],
            &[],
        );
        drop(conn);
        let report = plan(&vault).unwrap();
        let summary = report.summary.as_ref().unwrap();
        assert_eq!(summary.eligible_markdown_without_job, 1);
        assert_eq!(summary.jobs_without_markdown, 1);
        assert_eq!(summary.historical_only_notes, 1);
        assert!(!report.existing_markdown_coverage_complete);
        assert!(
            !report
                .notes
                .iter()
                .any(|note| note.note == "notes/old-deleted")
        );
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "eligible_markdown_without_job_may_be_pending_deletion")
        );
        let missing = report
            .notes
            .iter()
            .find(|note| note.note == "notes/missing")
            .unwrap();
        assert_eq!(
            missing.current_review_source,
            RecoveryCurrentSource::HistoryBefore
        );
    }

    #[test]
    fn plan_digest_binds_full_durable_rows_schema_and_raw_markdown() {
        let (_dir, vault, conn) = setup(7);
        let raw = document("raw", true);
        md(&vault, "notes/a", &raw);
        job(&conn, "notes/a", "completed", Some(&raw));
        let first = plan(&vault).unwrap();
        assert!(first.latest_state_proven);
        conn.execute("UPDATE distillation_jobs SET reason='changed evidence'", [])
            .unwrap();
        let changed_row = plan(&vault).unwrap();
        assert_ne!(first.plan_digest, changed_row.plan_digest);
        conn.execute("CREATE TABLE unknown_future_data(value TEXT)", [])
            .unwrap();
        let changed_schema = plan(&vault).unwrap();
        assert_ne!(changed_row.plan_digest, changed_schema.plan_digest);
        let altered = format!("{raw}\n");
        md(&vault, "notes/a", &altered);
        let changed_md = plan(&vault).unwrap();
        assert_ne!(changed_schema.plan_digest, changed_md.plan_digest);
        assert!(!changed_md.latest_state_proven);
        // 読取だけなので、完読後にも既存SQLite writer connectionはそのまま使える。
        assert_eq!(
            conn.query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn malformed_or_duplicate_history_keys_block_without_assuming_zero_history() {
        for malformed in ["not json", r#"{"notes/a":"x","notes/a":"y"}"#] {
            let (_dir, vault, conn) = setup(7);
            let raw = document("record", true);
            md(&vault, "notes/a", &raw);
            job(&conn, "notes/a", "completed", Some(&raw));
            conn.execute("INSERT INTO distillation_job_runs VALUES('run','notes/a',1,90,'no_change','fixture','digest',?1,'{}','test')",[malformed]).unwrap();
            drop(conn);
            let report = plan(&vault).unwrap();
            assert!(!report.snapshot_complete);
            assert!(report.plan_digest.is_none());
            assert!(report.summary.is_none());
            assert!(report.history_runs.is_none());
            assert!(
                report
                    .issues
                    .iter()
                    .any(|issue| issue.code == "history_document_map_invalid" && issue.blocking)
            );
        }
    }

    #[test]
    fn nonempty_notes_exports_or_runtime_marker_are_outside_the_limited_reset_shape() {
        for alteration in [
            "INSERT INTO notes(id,document) VALUES('notes/kept','kept private document')",
            "INSERT INTO note_exports(op_id,note_id,operation,document,log_entry,commit_message) VALUES('pending','notes/pending','upsert','private document','log','commit')",
            "INSERT INTO meta VALUES('runtime_store','db-v1')",
            "UPDATE meta SET value='10' WHERE key='schema'",
        ] {
            let (_dir, vault, conn) = setup(7);
            let raw = document("current observed version", true);
            md(&vault, "notes/a", &raw);
            job(&conn, "notes/a", "completed", Some(&raw));
            conn.execute(alteration, []).unwrap();
            drop(conn);
            let before = fs::read(vault.index_db_path()).unwrap();
            let report = plan(&vault).unwrap();
            assert!(!report.supported_reset_shape);
            assert_eq!(report.summary.as_ref().unwrap().completed_review_matches, 1);
            assert!(!report.latest_state_proven);
            assert!(
                report
                    .issues
                    .iter()
                    .any(|issue| issue.code == "unsupported_reset_shape")
            );
            assert_eq!(fs::read(vault.index_db_path()).unwrap(), before);
        }
    }

    #[test]
    fn invalid_authority_or_markdown_blocks_complete_candidate_set() {
        let (_dir, vault, conn) = setup(7);
        md(&vault, "notes/broken", "invalid private data");
        drop(conn);
        let report = plan(&vault).unwrap();
        assert_eq!(report.markdown_notes, Some(1));
        assert!(!report.snapshot_complete);
        assert!(!report.existing_markdown_coverage_complete);
        assert!(report.summary.is_none());
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "markdown_document_invalid")
        );
        assert!(!serde_json::to_string(&report).unwrap().contains("private"));
    }
}
