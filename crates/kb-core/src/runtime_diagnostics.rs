//! 正常なopen/migrationに入れない保存状態も、原状を変えず製品の診断口から観測する。
//! DBは単一read transaction、Markdownは走査中の変更を検出するbest-effort観測とする。
//! 件数の不明と0を区別し、本文・絶対パス・SQL・復旧可否の断定を公開しない。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use anyhow::Result;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::frontmatter::Note;
use crate::note_id::NoteId;
use crate::vault::Vault;

// 数万ノートの異常時も診断JSONが無制限に膨らまない。総件数は別に保持する。
const DETAIL_LIMIT: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RuntimeDiagnosticsReport {
    pub format_version: u32,
    pub read_only: bool,
    pub recovery_performed: bool,
    pub database_snapshot_complete: bool,
    pub declared_schema: Option<String>,
    pub runtime_store: Option<String>,
    pub schema_fingerprint: Option<String>,
    pub notes_columns: Option<Vec<String>>,
    pub unrecognized_notes_columns: Option<u64>,
    pub durable_tables: Vec<RuntimeTableObservation>,
    pub exports: RuntimeExportObservation,
    pub jobs: RuntimeJobObservation,
    pub markdown: RuntimeMarkdownObservation,
    pub findings: Vec<String>,
    pub issues: Vec<RuntimeDiagnosticIssue>,
    pub issue_count: u64,
    pub issues_truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RuntimeTableObservation {
    pub table: String,
    pub present: Option<bool>,
    pub rows: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RuntimeExportObservation {
    pub upserts: Option<u64>,
    pub deletes: Option<u64>,
    pub unknown_operations: Option<u64>,
    pub invalid_upsert_documents: Option<u64>,
    pub latest_upserts_matching_db: Option<u64>,
    pub latest_upserts_differing_from_db: Option<u64>,
    pub latest_upserts_missing_from_db: Option<u64>,
    pub latest_deletes_still_in_db: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RuntimeJobObservation {
    pub notes: Option<u64>,
    pub missing_from_db: Option<u64>,
    pub missing_from_db_with_valid_markdown: Option<u64>,
    pub missing_from_db_with_valid_pending_upsert: Option<u64>,
    pub missing_from_db_without_valid_markdown: Option<u64>,
    pub missing_from_db_without_valid_markdown_or_pending_upsert: Option<u64>,
    pub missing_from_db_without_valid_markdown_ids: Vec<String>,
    pub missing_ids_truncated: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RuntimeMarkdownObservation {
    /// SQLiteとファイル群をまたぐ原子的snapshotではないことを常に明示する。
    pub atomic_snapshot: bool,
    pub scan_complete: bool,
    pub files: Option<u64>,
    pub parsed: Option<u64>,
    pub invalid: Option<u64>,
    pub unreadable: Option<u64>,
    pub database_only: Option<u64>,
    pub markdown_only: Option<u64>,
    pub matching_documents: Option<u64>,
    pub differing_documents: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RuntimeDiagnosticIssue {
    pub code: String,
    pub table: Option<String>,
    pub note: Option<String>,
}

impl RuntimeDiagnosticsReport {
    fn new() -> Self {
        Self {
            format_version: 1,
            read_only: true,
            recovery_performed: false,
            database_snapshot_complete: false,
            declared_schema: None,
            runtime_store: None,
            schema_fingerprint: None,
            notes_columns: None,
            unrecognized_notes_columns: None,
            durable_tables: crate::derived_index::DURABLE_STATE_TABLES
                .iter()
                .map(|table| RuntimeTableObservation {
                    table: (*table).into(),
                    present: None,
                    rows: None,
                })
                .collect(),
            exports: RuntimeExportObservation::default(),
            jobs: RuntimeJobObservation::default(),
            markdown: RuntimeMarkdownObservation::default(),
            findings: Vec::new(),
            issues: Vec::new(),
            issue_count: 0,
            issues_truncated: false,
        }
    }

    fn issue(&mut self, code: &str, table: Option<&str>, note: Option<&str>) {
        self.issue_count += 1;
        if self.issues.len() < DETAIL_LIMIT {
            self.issues.push(RuntimeDiagnosticIssue {
                code: code.into(),
                table: table.map(str::to_owned),
                // 破損したDBのIDを絶対パスなどとして外へ漏らさない。
                note: note
                    .filter(|id| NoteId::parse(id).is_ok())
                    .map(str::to_owned),
            });
        } else {
            self.issues_truncated = true;
        }
    }

    fn observed<T>(&mut self, result: rusqlite::Result<T>, code: &str, table: &str) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(_) => {
                self.issue(code, Some(table), None);
                None
            }
        }
    }

    fn table(&self, name: &str) -> Option<&RuntimeTableObservation> {
        self.durable_tables.iter().find(|table| table.table == name)
    }
}

#[derive(Clone)]
struct DocumentObservation {
    hash: [u8; 32],
    valid: bool,
}

impl DocumentObservation {
    fn new(document: &str) -> Self {
        Self {
            hash: Sha256::digest(document.as_bytes()).into(),
            valid: Note::parse(document).is_ok(),
        }
    }
}

type Documents = BTreeMap<String, DocumentObservation>;
type SchemaObject = (String, String, String, Option<String>);

enum PendingExport {
    Upsert(DocumentObservation),
    Delete,
    Unknown,
}

/// 障害中でも入口を開けるよう、通常のVault/DB openやmigrationを一切経由しない。
/// 読取不能は診断結果に残し、未知の状態を空の正常ストアと取り違えない。
pub fn inspect(vault: &Vault) -> Result<RuntimeDiagnosticsReport> {
    let mut report = RuntimeDiagnosticsReport::new();
    let mut connection = match Connection::open_with_flags(
        vault.index_db_path(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(connection) => connection,
        Err(_) => {
            report.issue("database_open_failed", None, None);
            inspect_markdown(vault, &mut report);
            return Ok(report);
        }
    };
    if connection
        .busy_timeout(std::time::Duration::from_secs(2))
        .and_then(|()| connection.execute_batch("PRAGMA query_only=ON;"))
        .is_err()
    {
        report.issue("read_only_configuration_failed", None, None);
        inspect_markdown(vault, &mut report);
        return Ok(report);
    }
    let transaction = match connection.transaction() {
        Ok(transaction) => transaction,
        Err(_) => {
            report.issue("read_transaction_failed", None, None);
            inspect_markdown(vault, &mut report);
            return Ok(report);
        }
    };
    // 最初のschema読取でsnapshotを固定し、そのtransactionの中で全DB照合を行う。
    let tables = inspect_schema(&transaction, &mut report);
    let (documents, exports, jobs) = if let Some(tables) = tables {
        inspect_tables(&transaction, &tables, &mut report);
        let documents = inspect_documents(&transaction, &mut report);
        let exports = inspect_exports(&transaction, &mut report);
        let jobs = inspect_jobs(&transaction, &mut report);
        (documents, exports, jobs)
    } else {
        (None, None, None)
    };
    let markdown = inspect_markdown(vault, &mut report);
    compare_sources(
        documents.as_ref(),
        exports.as_ref(),
        jobs.as_ref(),
        markdown.as_ref(),
        &mut report,
    );
    detect_findings(&mut report);
    // COMMITも復旧操作と混同しないよう、read transactionは明示的にrollbackで閉じる。
    if transaction.rollback().is_err() {
        report.issue("read_transaction_close_failed", None, None);
    }
    report.database_snapshot_complete = report.schema_fingerprint.is_some()
        && report.durable_tables.iter().all(|table| {
            table.present == Some(false) || (table.present == Some(true) && table.rows.is_some())
        })
        && documents.is_some()
        && exports.is_some()
        && jobs.is_some();
    Ok(report)
}

fn inspect_schema(
    conn: &Connection,
    report: &mut RuntimeDiagnosticsReport,
) -> Option<BTreeSet<String>> {
    let schema = (|| -> rusqlite::Result<Vec<SchemaObject>> {
        let mut statement = conn.prepare(
            "SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name, tbl_name",
        )?;
        statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect()
    })();
    let schema = report.observed(schema, "schema_read_failed", "sqlite_schema")?;
    let mut hash = Sha256::new();
    let mut tables = BTreeSet::new();
    for (kind, name, table, sql) in schema {
        if kind == "table" {
            tables.insert(name.clone());
        }
        for field in [kind, name, table, sql.unwrap_or_default()] {
            hash.update((field.len() as u64).to_le_bytes());
            hash.update(field.as_bytes());
        }
    }
    report.schema_fingerprint = Some(format!("sha256:{:x}", hash.finalize()));
    if tables.contains("meta") {
        report.declared_schema = inspect_marker(conn, "schema", report);
        report.runtime_store = inspect_marker(conn, "runtime_store", report);
    } else {
        report.issue("meta_table_missing", Some("meta"), None);
    }
    Some(tables)
}

fn inspect_marker(
    conn: &Connection,
    key: &str,
    report: &mut RuntimeDiagnosticsReport,
) -> Option<String> {
    let value: Option<Option<String>> = report.observed(
        conn.query_row("SELECT value FROM meta WHERE key=?1", [key], |row| {
            row.get(0)
        })
        .optional(),
        "metadata_read_failed",
        "meta",
    );
    match value {
        Some(Some(value)) => match key {
            "schema" => match value.trim().parse::<u32>() {
                Ok(version) => Some(version.to_string()),
                Err(_) => {
                    report.issue("schema_declaration_invalid", Some("meta"), None);
                    None
                }
            },
            "runtime_store" if value == "db-v1" => Some(value),
            _ => {
                report.issue("runtime_marker_unrecognized", Some("meta"), None);
                None
            }
        },
        Some(None) => {
            report.issue(
                if key == "schema" {
                    "schema_declaration_missing"
                } else {
                    "runtime_marker_missing"
                },
                Some("meta"),
                None,
            );
            None
        }
        None => None,
    }
}

fn inspect_tables(
    conn: &Connection,
    tables: &BTreeSet<String>,
    report: &mut RuntimeDiagnosticsReport,
) {
    let vocabulary_absent_before_v11 = report
        .declared_schema
        .as_deref()
        .and_then(|version| version.parse::<u32>().ok())
        .is_some_and(|version| version < 11)
        && crate::tag_vocabulary_source::TABLES
            .iter()
            .all(|table| !tables.contains(*table));
    let tag_history_absent_before_v12 = report
        .declared_schema
        .as_deref()
        .and_then(|version| version.parse::<u32>().ok())
        .is_some_and(|version| version < 12)
        && crate::tag_vocabulary_history::TABLES
            .iter()
            .all(|table| !tables.contains(*table));
    let tag_rollback_absent_before_v13 = report
        .declared_schema
        .as_deref()
        .and_then(|version| version.parse::<u32>().ok())
        .is_some_and(|version| version < 13)
        && !tables.contains("tag_vocabulary_rollbacks");
    for index in 0..report.durable_tables.len() {
        let name = report.durable_tables[index].table.clone();
        let present = tables.contains(&name);
        report.durable_tables[index].present = Some(present);
        if present {
            // 名前は呼出し引数ではなくDURABLE_STATE_TABLESの固定allowlist由来。
            let rows = report.observed(
                conn.query_row(&format!("SELECT count(*) FROM {name}"), [], |row| {
                    row.get::<_, i64>(0).map(|count| count as u64)
                }),
                "table_count_failed",
                &name,
            );
            report.durable_tables[index].rows = rows;
        } else if !((vocabulary_absent_before_v11
            && crate::tag_vocabulary_source::TABLES.contains(&name.as_str()))
            || (tag_history_absent_before_v12
                && crate::tag_vocabulary_history::TABLES.contains(&name.as_str()))
            || (tag_rollback_absent_before_v13
                && crate::tag_vocabulary_history::ROLLBACK_TABLES.contains(&name.as_str())))
        {
            report.issue("durable_table_missing", Some(&name), None);
        }
    }
    if crate::tag_vocabulary_source::TABLES
        .iter()
        .all(|table| tables.contains(*table))
        && crate::tag_vocabulary_source::verify_schema(conn).is_err()
    {
        report.issue("tag_vocabulary_tables_invalid", None, None);
    }
    if crate::tag_vocabulary_history::TABLES
        .iter()
        .all(|table| tables.contains(*table))
        && crate::tag_vocabulary_history::verify_integrity(conn).is_err()
    {
        report.issue("tag_vocabulary_history_invalid", None, None);
    }
    if tables.contains("tag_vocabulary_rollbacks")
        && crate::tag_vocabulary_history::verify_rollback_integrity(conn).is_err()
    {
        report.issue("tag_vocabulary_rollback_history_invalid", None, None);
    }
    if tables.contains("notes") {
        let columns = (|| -> rusqlite::Result<Vec<String>> {
            let mut statement = conn.prepare("PRAGMA table_info(notes)")?;
            statement.query_map([], |row| row.get(1))?.collect()
        })();
        if let Some(columns) = report.observed(columns, "notes_columns_read_failed", "notes") {
            // 破損したsqlite_schemaには本文やpathを列名として埋め込めるため、
            // 公開する名前は既知versionの列に限定し、未知名は件数とfingerprintで区別する。
            let known: Vec<_> = columns
                .iter()
                .filter(|column| {
                    matches!(
                        column.as_str(),
                        "id" | "title"
                            | "description"
                            | "status"
                            | "origin"
                            | "generated_by"
                            | "generated_at"
                            | "mtime"
                            | "body"
                            | "tags"
                            | "created"
                            | "document"
                            | "note_uid"
                            | "namespace"
                            | "authority_role"
                            | "authority_status"
                            | "authority_scope"
                            | "normal_reference_allowed"
                            | "distillation_allowed"
                    )
                })
                .cloned()
                .collect();
            let unknown = columns.len() - known.len();
            report.unrecognized_notes_columns = Some(unknown as u64);
            report.notes_columns = Some(known);
            if unknown > 0 {
                report.issue("notes_columns_unrecognized", Some("notes"), None);
            }
        }
    }
}

fn inspect_documents(
    conn: &Connection,
    report: &mut RuntimeDiagnosticsReport,
) -> Option<Documents> {
    if report.table("notes")?.present != Some(true) {
        return None;
    }
    let result = (|| -> rusqlite::Result<Documents> {
        let mut statement = conn.prepare("SELECT id, document FROM notes ORDER BY id")?;
        let mut rows = statement.query([])?;
        let mut documents = BTreeMap::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let document: String = row.get(1)?;
            if NoteId::parse(&id).is_err() {
                report.issue("database_note_id_invalid", Some("notes"), None);
            }
            let observation = DocumentObservation::new(&document);
            if !observation.valid {
                report.issue("database_document_invalid", Some("notes"), Some(&id));
            }
            documents.insert(id, observation);
        }
        Ok(documents)
    })();
    report.observed(result, "documents_read_failed", "notes")
}

fn inspect_exports(
    conn: &Connection,
    report: &mut RuntimeDiagnosticsReport,
) -> Option<BTreeMap<String, PendingExport>> {
    if report.table("note_exports")?.present != Some(true) {
        return None;
    }
    let result = (|| -> rusqlite::Result<_> {
        let mut statement =
            conn.prepare("SELECT note_id, operation, document FROM note_exports ORDER BY seq")?;
        let mut rows = statement.query([])?;
        let (mut upserts, mut deletes, mut unknown, mut invalid) = (0, 0, 0, 0);
        let mut latest = BTreeMap::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let operation: String = row.get(1)?;
            let document: Option<String> = row.get(2)?;
            let pending = match operation.as_str() {
                "upsert" => {
                    upserts += 1;
                    let observation =
                        DocumentObservation::new(document.as_deref().unwrap_or_default());
                    if !observation.valid {
                        invalid += 1;
                        report.issue(
                            "pending_upsert_document_invalid",
                            Some("note_exports"),
                            Some(&id),
                        );
                    }
                    PendingExport::Upsert(observation)
                }
                "delete" => {
                    deletes += 1;
                    PendingExport::Delete
                }
                _ => {
                    unknown += 1;
                    report.issue(
                        "pending_export_operation_unknown",
                        Some("note_exports"),
                        Some(&id),
                    );
                    PendingExport::Unknown
                }
            };
            latest.insert(id, pending);
        }
        Ok((latest, upserts, deletes, unknown, invalid))
    })();
    let (latest, upserts, deletes, unknown, invalid) =
        report.observed(result, "exports_read_failed", "note_exports")?;
    report.exports.upserts = Some(upserts);
    report.exports.deletes = Some(deletes);
    report.exports.unknown_operations = Some(unknown);
    report.exports.invalid_upsert_documents = Some(invalid);
    Some(latest)
}

fn inspect_jobs(
    conn: &Connection,
    report: &mut RuntimeDiagnosticsReport,
) -> Option<BTreeSet<String>> {
    if report.table("distillation_jobs")?.present != Some(true) {
        return None;
    }
    let result = (|| -> rusqlite::Result<BTreeSet<String>> {
        let mut statement = conn.prepare("SELECT note FROM distillation_jobs ORDER BY note")?;
        statement.query_map([], |row| row.get(0))?.collect()
    })();
    let jobs = report.observed(result, "jobs_read_failed", "distillation_jobs")?;
    report.jobs.notes = Some(jobs.len() as u64);
    for id in &jobs {
        if NoteId::parse(id).is_err() {
            report.issue("job_note_id_invalid", Some("distillation_jobs"), None);
        }
    }
    Some(jobs)
}

fn inspect_markdown(vault: &Vault, report: &mut RuntimeDiagnosticsReport) -> Option<Documents> {
    let files = match vault.list_note_files() {
        Ok(files) => files,
        Err(_) => {
            report.issue("markdown_scan_failed", None, None);
            return None;
        }
    };
    report.markdown.files = Some(files.len() as u64);
    let (mut parsed, mut invalid, mut unreadable) = (0, 0, 0);
    let mut changed = false;
    let mut documents = BTreeMap::new();
    for (id, path) in &files {
        let read = (|| -> std::io::Result<_> {
            let before = fs::metadata(path)?;
            let document = fs::read_to_string(path)?;
            let after = fs::metadata(path)?;
            let stable = before.len() == after.len() && before.modified()? == after.modified()?;
            Ok((document, stable))
        })();
        let (document, stable) = match read {
            Ok(read) => read,
            Err(_) => {
                unreadable += 1;
                report.issue("markdown_read_failed", None, Some(id));
                continue;
            }
        };
        if !stable {
            changed = true;
            report.issue("markdown_changed_during_scan", None, Some(id));
        }
        let observation = DocumentObservation::new(&document);
        if observation.valid {
            parsed += 1;
        } else {
            invalid += 1;
            report.issue("markdown_document_invalid", None, Some(id));
        }
        documents.insert(id.clone(), observation);
    }
    match vault.list_note_files() {
        Ok(after) if after == files => {}
        Ok(_) => {
            changed = true;
            report.issue("markdown_inventory_changed", None, None);
        }
        Err(_) => {
            changed = true;
            report.issue("markdown_rescan_failed", None, None);
        }
    }
    report.markdown.parsed = Some(parsed);
    report.markdown.invalid = Some(invalid);
    report.markdown.unreadable = Some(unreadable);
    report.markdown.scan_complete = unreadable == 0 && !changed;
    report.markdown.scan_complete.then_some(documents)
}

fn compare_sources(
    db: Option<&Documents>,
    exports: Option<&BTreeMap<String, PendingExport>>,
    jobs: Option<&BTreeSet<String>>,
    markdown: Option<&Documents>,
    report: &mut RuntimeDiagnosticsReport,
) {
    let Some(db) = db else { return };
    if let Some(markdown) = markdown {
        report.markdown.database_only =
            Some(db.keys().filter(|id| !markdown.contains_key(*id)).count() as u64);
        report.markdown.markdown_only =
            Some(markdown.keys().filter(|id| !db.contains_key(*id)).count() as u64);
        let (mut matching, mut differing) = (0, 0);
        for (id, document) in db {
            if let Some(exported) = markdown.get(id) {
                if document.hash == exported.hash {
                    matching += 1;
                } else {
                    differing += 1;
                }
            }
        }
        report.markdown.matching_documents = Some(matching);
        report.markdown.differing_documents = Some(differing);
    }
    if let Some(exports) = exports {
        let (mut matching, mut differing, mut missing, mut deleted) = (0, 0, 0, 0);
        for (id, pending) in exports {
            match (pending, db.get(id)) {
                (PendingExport::Upsert(document), Some(stored)) if document.hash == stored.hash => {
                    matching += 1
                }
                (PendingExport::Upsert(_), Some(_)) => differing += 1,
                (PendingExport::Upsert(_), None) => missing += 1,
                (PendingExport::Delete, Some(_)) => deleted += 1,
                _ => {}
            }
        }
        report.exports.latest_upserts_matching_db = Some(matching);
        report.exports.latest_upserts_differing_from_db = Some(differing);
        report.exports.latest_upserts_missing_from_db = Some(missing);
        report.exports.latest_deletes_still_in_db = Some(deleted);
    }
    let Some(jobs) = jobs else { return };
    let missing: Vec<_> = jobs.iter().filter(|id| !db.contains_key(*id)).collect();
    report.jobs.missing_from_db = Some(missing.len() as u64);
    let pending_valid = |id: &str| matches!(exports.and_then(|exports| exports.get(id)), Some(PendingExport::Upsert(document)) if document.valid);
    if exports.is_some() {
        report.jobs.missing_from_db_with_valid_pending_upsert =
            Some(missing.iter().filter(|id| pending_valid(id)).count() as u64);
    }
    if let Some(markdown) = markdown {
        let unavailable: Vec<_> = missing
            .iter()
            .filter(|id| {
                !markdown
                    .get(id.as_str())
                    .is_some_and(|document| document.valid)
            })
            .collect();
        report.jobs.missing_from_db_with_valid_markdown =
            Some((missing.len() - unavailable.len()) as u64);
        report.jobs.missing_from_db_without_valid_markdown = Some(unavailable.len() as u64);
        report.jobs.missing_from_db_without_valid_markdown_ids = unavailable
            .iter()
            .filter(|id| NoteId::parse(id).is_ok())
            .take(DETAIL_LIMIT)
            .map(|id| id.to_string())
            .collect();
        report.jobs.missing_ids_truncated =
            unavailable.len() > report.jobs.missing_from_db_without_valid_markdown_ids.len();
        if exports.is_some() {
            report
                .jobs
                .missing_from_db_without_valid_markdown_or_pending_upsert =
                Some(unavailable.iter().filter(|id| !pending_valid(id)).count() as u64);
        }
    }
}

fn detect_findings(report: &mut RuntimeDiagnosticsReport) {
    if report
        .declared_schema
        .as_deref()
        .and_then(|version| version.parse::<u32>().ok())
        .is_some_and(|version| version < 13)
        && report
            .table("tag_vocabulary_rollbacks")
            .is_some_and(|table| table.present == Some(true))
    {
        report
            .findings
            .push("schema_declaration_conflicts_with_v13_tables".into());
    }
    if report
        .declared_schema
        .as_deref()
        .and_then(|version| version.parse::<u32>().ok())
        .is_some_and(|version| version < 12)
        && crate::tag_vocabulary_history::TABLES.iter().any(|name| {
            report
                .table(name)
                .is_some_and(|table| table.present == Some(true))
        })
    {
        report
            .findings
            .push("schema_declaration_conflicts_with_v12_tables".into());
    }
    if report
        .declared_schema
        .as_deref()
        .and_then(|version| version.parse::<u32>().ok())
        .is_some_and(|version| version < 11)
        && crate::tag_vocabulary_source::TABLES.iter().any(|name| {
            report
                .table(name)
                .is_some_and(|table| table.present == Some(true))
        })
    {
        report
            .findings
            .push("schema_declaration_conflicts_with_v11_tables".into());
    }
    let v10_jobs = ["distillation_jobs", "distillation_job_runs"]
        .iter()
        .all(|name| {
            report
                .table(name)
                .is_some_and(|table| table.present == Some(true))
        });
    let declared_v9 = report.declared_schema.as_deref() == Some("9");
    if declared_v9 && v10_jobs {
        report
            .findings
            .push("schema_declaration_conflicts_with_v10_tables".into());
    }
    let missing_marker = report
        .issues
        .iter()
        .any(|issue| issue.code == "runtime_marker_missing");
    if declared_v9
        && v10_jobs
        && missing_marker
        && report
            .table("notes")
            .is_some_and(|table| table.rows == Some(0))
        && report.notes_columns.as_ref().is_some_and(|columns| {
            columns.iter().any(|column| column == "document")
                && !columns.iter().any(|column| {
                    column == "normal_reference_allowed" || column == "distillation_allowed"
                })
        })
    {
        // 複数の独立した痕跡の一致。実行者や発生時刻、復旧可能性を断定しない。
        report.findings.push("legacy_v9_reset_pattern".into());
    }
    if report.jobs.missing_from_db.is_some_and(|count| count > 0) {
        report.findings.push("current_jobs_missing_notes".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontmatter::Frontmatter;
    use rusqlite::params;

    fn setup() -> (tempfile::TempDir, Vault) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        (dir, vault)
    }

    fn document(body: &str) -> String {
        Note {
            front: Frontmatter::new_note("診断fixture"),
            body: body.into(),
        }
        .to_file_string()
        .unwrap()
    }

    fn write_markdown(vault: &Vault, name: &str, document: &str) {
        fs::write(vault.root.join(format!("notes/{name}.md")), document).unwrap();
    }

    fn synthetic_mixed_schema(vault: &Vault) -> Connection {
        fs::create_dir_all(vault.index_db_path().parent().unwrap()).unwrap();
        let conn = Connection::open(vault.index_db_path()).unwrap();
        conn.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO meta VALUES('schema','9');
             CREATE TABLE notes(id TEXT PRIMARY KEY, document TEXT NOT NULL);
             CREATE TABLE note_exports(seq INTEGER PRIMARY KEY, note_id TEXT, operation TEXT, document TEXT);
             CREATE TABLE distillation_jobs(note TEXT PRIMARY KEY);
             CREATE TABLE distillation_job_runs(run_id TEXT PRIMARY KEY, before_documents TEXT);
             INSERT INTO distillation_job_runs VALUES('run:preserve', '{\"before\":\"private audit content\"}');
             CREATE TABLE distillation_runs(execution_id TEXT PRIMARY KEY, before_documents TEXT);
             INSERT INTO distillation_runs VALUES('distill:preserve', 'private manual audit content');
             CREATE TABLE action_receipts(receipt_id TEXT);
             CREATE TABLE action_capability_uses(capability_id TEXT);"
        ).unwrap();
        conn
    }

    /// 2026-09-07: schema宣言だけ戻りnotesを失ったDBを、診断時のmigrationで更に変えない。
    #[test]
    fn mixed_schema_reports_missing_sources_without_changing_database_or_markdown() {
        let (_dir, vault) = setup();
        let conn = synthetic_mixed_schema(&vault);
        let exported = document("private exported body");
        let pending = document("private pending body newer than markdown");
        write_markdown(&vault, "exported", &exported);
        write_markdown(&vault, "invalid", "broken frontmatter private contents");
        conn.execute_batch(
            "INSERT INTO distillation_jobs VALUES('notes/exported'),('notes/pending'),('notes/invalid'),('notes/missing');"
        ).unwrap();
        conn.execute(
            "INSERT INTO note_exports VALUES(1,'notes/pending','upsert',?1)",
            [&pending],
        )
        .unwrap();
        drop(conn);
        let path = vault.index_db_path();
        let before = fs::read(&path).unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let markdown_before = fs::read(vault.root.join("notes/exported.md")).unwrap();

        let report = inspect(&vault).unwrap();

        assert!(report.read_only);
        assert!(!report.recovery_performed);
        assert!(report.database_snapshot_complete);
        assert_eq!(report.declared_schema.as_deref(), Some("9"));
        assert_eq!(report.runtime_store, None);
        for table in crate::tag_vocabulary_source::TABLES {
            assert_eq!(report.table(table).unwrap().present, Some(false));
            assert_eq!(report.table(table).unwrap().rows, None);
            assert!(!report.issues.iter().any(|issue| {
                issue.code == "durable_table_missing" && issue.table.as_deref() == Some(table)
            }));
        }
        assert!(
            report
                .findings
                .iter()
                .any(|code| code == "legacy_v9_reset_pattern")
        );
        assert_eq!(report.table("distillation_job_runs").unwrap().rows, Some(1));
        assert_eq!(report.table("distillation_runs").unwrap().rows, Some(1));
        assert_eq!(report.markdown.files, Some(2));
        assert_eq!(report.markdown.parsed, Some(1));
        assert_eq!(report.markdown.invalid, Some(1));
        assert_eq!(report.markdown.markdown_only, Some(2));
        assert_eq!(report.jobs.missing_from_db, Some(4));
        assert_eq!(report.jobs.missing_from_db_with_valid_markdown, Some(1));
        assert_eq!(
            report.jobs.missing_from_db_with_valid_pending_upsert,
            Some(1)
        );
        assert_eq!(report.jobs.missing_from_db_without_valid_markdown, Some(3));
        assert_eq!(
            report
                .jobs
                .missing_from_db_without_valid_markdown_or_pending_upsert,
            Some(2)
        );
        assert_eq!(
            report.jobs.missing_from_db_without_valid_markdown_ids,
            ["notes/invalid", "notes/missing", "notes/pending"]
        );
        assert_eq!(report.exports.upserts, Some(1));
        assert_eq!(report.exports.latest_upserts_missing_from_db, Some(1));
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
        assert_eq!(
            fs::read(vault.root.join("notes/exported.md")).unwrap(),
            markdown_before
        );
        assert!(!path.with_extension("db-wal").exists());
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("private"));
        assert!(!json.contains(&vault.root.display().to_string()));
        assert!(!json.contains("CREATE TABLE"));
        assert!(!json.contains("can_apply"));
    }

    #[test]
    fn absent_and_corrupt_database_remain_absent_or_byte_identical() {
        let (_dir, vault) = setup();
        write_markdown(&vault, "valid", &document("source"));
        let missing = inspect(&vault).unwrap();
        assert!(!vault.index_db_path().exists());
        assert!(!missing.database_snapshot_complete);
        assert_eq!(missing.table("notes").unwrap().present, None);
        assert_eq!(missing.table("notes").unwrap().rows, None);
        assert_eq!(missing.markdown.files, Some(1));
        assert_eq!(missing.markdown.database_only, None);
        assert!(
            missing
                .issues
                .iter()
                .any(|issue| issue.code == "database_open_failed")
        );

        fs::create_dir_all(vault.index_db_path().parent().unwrap()).unwrap();
        fs::write(vault.index_db_path(), b"not a database private content").unwrap();
        let before = fs::read(vault.index_db_path()).unwrap();
        let corrupted = inspect(&vault).unwrap();
        assert_eq!(corrupted.table("notes").unwrap().rows, None);
        assert!(!corrupted.database_snapshot_complete);
        assert!(corrupted.issue_count > 0);
        assert_eq!(fs::read(vault.index_db_path()).unwrap(), before);
    }

    #[test]
    fn read_errors_are_unknown_not_zero_and_missing_tables_are_explicit() {
        let (_dir, vault) = setup();
        let conn = synthetic_mixed_schema(&vault);
        conn.execute_batch(
            "ALTER TABLE note_exports RENAME COLUMN document TO incompatible;
                            DROP TABLE action_receipts;",
        )
        .unwrap();
        drop(conn);
        fs::write(vault.root.join("notes/not-utf8.md"), [0xff, 0xfe]).unwrap();
        let report = inspect(&vault).unwrap();
        assert_eq!(report.exports.upserts, None);
        assert_eq!(
            report.table("action_receipts").unwrap().present,
            Some(false)
        );
        assert_eq!(report.table("action_receipts").unwrap().rows, None);
        assert_eq!(report.markdown.unreadable, Some(1));
        assert!(!report.markdown.scan_complete);
        assert_eq!(report.markdown.markdown_only, None);
        assert_eq!(report.jobs.missing_from_db_without_valid_markdown, None);
        assert!(!report.database_snapshot_complete);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "exports_read_failed")
        );
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "markdown_read_failed")
        );
    }

    /// 2026-09-08: v11導入前の両表不在と、片欠損・現行schemaの欠損を区別する。
    #[test]
    fn vocabulary_table_absence_is_versioned_without_hiding_partial_loss() {
        for (schema, partial, expected_missing) in [(9, false, 0), (9, true, 1), (11, false, 2)] {
            let (_dir, vault) = setup();
            let conn = crate::index::open_db(&vault).unwrap();
            conn.execute_batch("DROP TABLE tag_vocabulary_source_exports")
                .unwrap();
            if !partial {
                conn.execute_batch("DROP TABLE tag_vocabulary_sources")
                    .unwrap();
            }
            conn.execute(
                "UPDATE meta SET value=?1 WHERE key='schema'",
                [schema.to_string()],
            )
            .unwrap();
            conn.execute_batch("PRAGMA journal_mode=DELETE").unwrap();
            drop(conn);
            let before = fs::read(vault.index_db_path()).unwrap();
            let report = inspect(&vault).unwrap();
            let missing = report
                .issues
                .iter()
                .filter(|issue| {
                    issue.code == "durable_table_missing"
                        && issue.table.as_deref().is_some_and(|table| {
                            crate::tag_vocabulary_source::TABLES.contains(&table)
                        })
                })
                .count();
            assert_eq!(missing, expected_missing);
            assert_eq!(
                report
                    .findings
                    .iter()
                    .any(|finding| { finding == "schema_declaration_conflicts_with_v11_tables" }),
                partial
            );
            assert_eq!(fs::read(vault.index_db_path()).unwrap(), before);
        }
    }

    #[test]
    fn vocabulary_history_absence_is_versioned_and_invalid_originals_are_not_exposed() {
        for (schema, partial, expected_missing) in [(11, false, 0), (11, true, 1), (12, false, 2)] {
            let (_dir, vault) = setup();
            let conn = crate::index::open_db(&vault).unwrap();
            conn.execute_batch("DROP TABLE tag_vocabulary_run_notes")
                .unwrap();
            if !partial {
                conn.execute_batch("DROP TABLE tag_vocabulary_runs")
                    .unwrap();
            }
            conn.execute(
                "UPDATE meta SET value=?1 WHERE key='schema'",
                [schema.to_string()],
            )
            .unwrap();
            conn.execute_batch("PRAGMA journal_mode=DELETE").unwrap();
            drop(conn);
            let before = fs::read(vault.index_db_path()).unwrap();
            let report = inspect(&vault).unwrap();
            assert_eq!(
                report
                    .issues
                    .iter()
                    .filter(|issue| issue.code == "durable_table_missing"
                        && issue.table.as_deref().is_some_and(|table| {
                            crate::tag_vocabulary_history::TABLES.contains(&table)
                        }))
                    .count(),
                expected_missing
            );
            assert_eq!(
                report
                    .findings
                    .iter()
                    .any(|finding| finding == "schema_declaration_conflicts_with_v12_tables"),
                partial
            );
            assert_eq!(fs::read(vault.index_db_path()).unwrap(), before);
        }
        let (_dir, vault) = setup();
        let conn = crate::index::open_db(&vault).unwrap();
        crate::tag_vocabulary_history::record_for_test(&conn);
        conn.execute_batch(
            "UPDATE tag_vocabulary_run_notes SET before_document='private broken original'",
        )
        .unwrap();
        drop(conn);
        let report = inspect(&vault).unwrap();
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "tag_vocabulary_history_invalid")
        );
        assert!(!serde_json::to_string(&report).unwrap().contains("private"));
    }

    #[test]
    fn vocabulary_rollback_absence_and_downgrade_are_versioned_without_mutating_database() {
        for (schema, keep_table) in [(12, false), (13, false), (12, true)] {
            let (_dir, vault) = setup();
            let conn = crate::index::open_db(&vault).unwrap();
            if !keep_table {
                conn.execute_batch("DROP TABLE tag_vocabulary_rollbacks")
                    .unwrap();
            }
            conn.execute(
                "UPDATE meta SET value=?1 WHERE key='schema'",
                [schema.to_string()],
            )
            .unwrap();
            conn.execute_batch("PRAGMA journal_mode=DELETE").unwrap();
            drop(conn);
            let before = fs::read(vault.index_db_path()).unwrap();
            let report = inspect(&vault).unwrap();
            assert_eq!(
                report
                    .issues
                    .iter()
                    .any(|issue| issue.code == "durable_table_missing"
                        && issue.table.as_deref() == Some("tag_vocabulary_rollbacks")),
                schema == 13
            );
            assert_eq!(
                report
                    .findings
                    .iter()
                    .any(|finding| finding == "schema_declaration_conflicts_with_v13_tables"),
                keep_table
            );
            assert_eq!(before, fs::read(vault.index_db_path()).unwrap());
        }
        let (_dir, vault) = setup();
        let conn = crate::index::open_db(&vault).unwrap();
        let execution_id = crate::tag_vocabulary_history::record_for_test(&conn);
        crate::tag_vocabulary_history::record_rollback_for_test(&conn, &execution_id);
        conn.execute_batch("UPDATE tag_vocabulary_rollbacks SET restored_at='private broken time'")
            .unwrap();
        drop(conn);
        let report = inspect(&vault).unwrap();
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "tag_vocabulary_rollback_history_invalid")
        );
        assert!(!serde_json::to_string(&report).unwrap().contains("private"));
    }

    /// 2026-09-08: 指定JSONやoutbox破損を空の正本扱いにせず、内容を診断JSONへ漏らさない。
    #[test]
    fn invalid_vocabulary_documents_are_reported_without_mutation_or_content() {
        for alteration in [
            "INSERT INTO tag_vocabulary_sources VALUES(1,'private broken document')",
            "INSERT INTO tag_vocabulary_source_exports VALUES(1,'op',NULL,'private broken document','log','commit')",
        ] {
            let (_dir, vault) = setup();
            let conn = crate::index::open_db(&vault).unwrap();
            conn.execute_batch(alteration).unwrap();
            conn.execute_batch("PRAGMA journal_mode=DELETE").unwrap();
            drop(conn);
            let before = fs::read(vault.index_db_path()).unwrap();
            let report = inspect(&vault).unwrap();
            assert!(
                report
                    .issues
                    .iter()
                    .any(|issue| { issue.code == "tag_vocabulary_tables_invalid" })
            );
            assert_eq!(fs::read(vault.index_db_path()).unwrap(), before);
            assert!(!serde_json::to_string(&report).unwrap().contains("private"));
        }
    }

    #[test]
    fn latest_pending_operation_is_observed_without_resurrecting_deleted_notes() {
        let (_dir, vault) = setup();
        let conn = synthetic_mixed_schema(&vault);
        let old = document("old private body");
        let new = document("new private body");
        conn.execute("INSERT INTO notes VALUES('notes/current',?1)", [&new])
            .unwrap();
        conn.execute("INSERT INTO notes VALUES('notes/deleted',?1)", [&old])
            .unwrap();
        conn.execute(
            "INSERT INTO note_exports VALUES(1,'notes/current','upsert',?1)",
            [&old],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO note_exports VALUES(2,'notes/current','upsert',?1)",
            [&new],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO note_exports VALUES(3,'notes/deleted','upsert',?1)",
            [&old],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO note_exports VALUES(4,'notes/deleted','delete',NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO note_exports VALUES(5,'notes/unknown','unknown',NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO note_exports VALUES(6,'notes/invalid','upsert',NULL)",
            [],
        )
        .unwrap();
        conn.execute_batch("INSERT INTO distillation_jobs VALUES('notes/deleted');")
            .unwrap();
        write_markdown(&vault, "current", &old);
        write_markdown(&vault, "extra", &old);
        drop(conn);

        let report = inspect(&vault).unwrap();
        assert_eq!(report.exports.upserts, Some(4));
        assert_eq!(report.exports.deletes, Some(1));
        assert_eq!(report.exports.unknown_operations, Some(1));
        assert_eq!(report.exports.invalid_upsert_documents, Some(1));
        assert_eq!(report.exports.latest_upserts_matching_db, Some(1));
        assert_eq!(report.exports.latest_upserts_differing_from_db, Some(0));
        assert_eq!(report.exports.latest_upserts_missing_from_db, Some(1));
        assert_eq!(report.exports.latest_deletes_still_in_db, Some(1));
        assert_eq!(report.markdown.database_only, Some(1));
        assert_eq!(report.markdown.markdown_only, Some(1));
        assert_eq!(report.markdown.differing_documents, Some(1));
        assert_eq!(report.jobs.missing_from_db, Some(0));
        assert!(
            !report
                .findings
                .iter()
                .any(|code| code == "legacy_v9_reset_pattern")
        );
    }

    #[test]
    fn issue_details_are_bounded_and_invalid_ids_never_escape_as_paths() {
        let (_dir, vault) = setup();
        let conn = synthetic_mixed_schema(&vault);
        for i in 0..(DETAIL_LIMIT + 2) {
            conn.execute(
                "INSERT INTO notes VALUES(?1,'invalid')",
                [format!("notes/{i}")],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO distillation_jobs VALUES('/private/path/secret')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO notes VALUES(?1,?2)",
            params!["/private/other/secret", "invalid"],
        )
        .unwrap();
        drop(conn);
        let report = inspect(&vault).unwrap();
        assert!(report.issues_truncated);
        assert_eq!(report.issues.len(), DETAIL_LIMIT);
        assert!(report.issue_count > DETAIL_LIMIT as u64);
        assert_eq!(report.jobs.missing_from_db_without_valid_markdown, Some(1));
        assert!(
            report
                .jobs
                .missing_from_db_without_valid_markdown_ids
                .is_empty()
        );
        assert!(report.jobs.missing_ids_truncated);
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("/private/")
        );
    }

    #[test]
    fn wal_observation_uses_committed_state_and_does_not_repair_schema() {
        let (_dir, vault) = setup();
        let conn = synthetic_mixed_schema(&vault);
        conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        conn.execute(
            "INSERT INTO notes VALUES('notes/committed',?1)",
            [document("committed")],
        )
        .unwrap();
        conn.execute_batch("BEGIN IMMEDIATE;").unwrap();
        conn.execute(
            "INSERT INTO notes VALUES('notes/uncommitted',?1)",
            [document("uncommitted")],
        )
        .unwrap();

        let report = inspect(&vault).unwrap();
        assert_eq!(report.table("notes").unwrap().rows, Some(1));
        assert_eq!(report.declared_schema.as_deref(), Some("9"));
        assert_eq!(report.notes_columns.as_ref().unwrap(), &["id", "document"]);
        conn.execute_batch("ROLLBACK;").unwrap();
    }

    /// 2026-09-07: 異常DBのmetadata/列名を診断口からそのまま外へ返さない。
    #[test]
    fn unexpected_metadata_and_column_names_never_expose_arbitrary_text() {
        let (_dir, vault) = setup();
        let conn = synthetic_mixed_schema(&vault);
        conn.execute_batch(
            "UPDATE meta SET value='/private/secret' WHERE key='schema';
             INSERT INTO meta VALUES('runtime_store','private body');
             ALTER TABLE notes ADD COLUMN \"SELECT private body FROM /private/secret\" TEXT;",
        )
        .unwrap();
        drop(conn);
        let before = fs::read(vault.index_db_path()).unwrap();
        let report = inspect(&vault).unwrap();
        assert_eq!(report.declared_schema, None);
        assert_eq!(report.runtime_store, None);
        assert_eq!(report.notes_columns.as_ref().unwrap(), &["id", "document"]);
        assert_eq!(report.unrecognized_notes_columns, Some(1));
        assert!(report.schema_fingerprint.is_some());
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("private"));
        assert!(!json.contains("SELECT"));
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "schema_declaration_invalid")
        );
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "runtime_marker_unrecognized")
        );
        assert_eq!(fs::read(vault.index_db_path()).unwrap(), before);
    }
}
