//! 現存Markdownの限定復旧。バックアップを検証するまで元DBの行・schemaを変更しない。

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Result, ensure};
use rusqlite::{
    Connection, OpenFlags, TransactionBehavior,
    backup::{Backup, StepResult},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::derived_index::{DerivedArtifact, ObjectKind};
use crate::note_id::NoteId;
use crate::vault::Vault;

use super::{RecoveryJobState, RuntimeRecoveryPlan};

// ロック・I/O異常で専用復旧画面を無期限に占有しない。
const BACKUP_TIMEOUT: Duration = Duration::from_secs(60);
const BACKUP_DIRECTORY: &str = "runtime-recovery-backups";
const PRESERVED_TABLES: [&str; 6] = [
    "note_exports",
    "distillation_runs",
    "action_receipts",
    "action_capability_uses",
    "distillation_jobs",
    "distillation_job_runs",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(deny_unknown_fields)]
pub struct RuntimeRecoveryRequest {
    pub expected_plan_digest: String,
    pub acknowledge_unproven: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RuntimeRecoveryReceipt {
    pub backup_id: String,
    pub plan_digest: String,
    pub restored_notes: u64,
    pub verified_review_notes: u64,
    pub unproven_notes: u64,
    pub backup_verified: bool,
    pub preserved_ledgers: Vec<RuntimeRecoveryLedgerReceipt>,
    pub automatic_processing_resumed: bool,
    pub restart_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RuntimeRecoveryLedgerReceipt {
    pub table: String,
    pub rows: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum RuntimeRecoveryFailureKind {
    InvalidRequest,
    PlanChanged,
    UnsupportedState,
    UnprovenNotAcknowledged,
    DatabaseBusy,
    BackupFailed,
    SourceChanged,
    RestoreFailed,
    VerificationFailed,
    CommitFailed,
}

#[derive(Debug)]
pub struct RuntimeRecoveryFailure {
    pub kind: RuntimeRecoveryFailureKind,
}

impl std::fmt::Display for RuntimeRecoveryFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "限定復旧を完了できない: {:?}", self.kind)
    }
}
impl std::error::Error for RuntimeRecoveryFailure {}

pub fn failure_kind(error: &anyhow::Error) -> Option<RuntimeRecoveryFailureKind> {
    error.chain().find_map(|cause| {
        cause
            .downcast_ref::<RuntimeRecoveryFailure>()
            .map(|failure| failure.kind)
    })
}

fn failure(kind: RuntimeRecoveryFailureKind) -> anyhow::Error {
    RuntimeRecoveryFailure { kind }.into()
}

fn stage<T>(kind: RuntimeRecoveryFailureKind, result: Result<T>) -> Result<T> {
    // GUI境界へSQL/pathを含む下位errorを運ばず、固定分類だけを渡す。
    result.map_err(|_| failure(kind))
}

#[derive(Serialize, Deserialize)]
struct BackupManifest {
    format_version: u32,
    plan_digest: String,
    database_logical_sha256: String,
    notes: Vec<BackupNoteManifest>,
    preserved_ledgers: Vec<RuntimeRecoveryLedgerReceipt>,
}

#[derive(Serialize, Deserialize)]
struct BackupNoteManifest {
    note: String,
    bytes: u64,
    sha256: String,
}

struct RawNote {
    id: String,
    document: String,
    mtime: i64,
}

struct VerifiedBackup {
    id: String,
    directory: PathBuf,
    notes: Vec<RawNote>,
}

/// 他writerを停止した専用起動面からのみ使う。worker起動・export・Git同期は呼ばない。
pub fn apply(vault: &Vault, request: &RuntimeRecoveryRequest) -> Result<RuntimeRecoveryReceipt> {
    apply_with_hook(vault, request, |_| Ok(()))
}

// 故障注入は同じ処理経路を使い、backup前・反映途中・commit直前のrollbackを検証する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyPoint {
    BeforeBackup,
    BackupVerified,
    RowsRestored,
    BeforeCommit,
}

fn apply_with_hook(
    vault: &Vault,
    request: &RuntimeRecoveryRequest,
    mut hook: impl FnMut(ApplyPoint) -> Result<()>,
) -> Result<RuntimeRecoveryReceipt> {
    if !super::valid_hash(&request.expected_plan_digest) {
        return Err(failure(RuntimeRecoveryFailureKind::InvalidRequest));
    }
    let identity = stage(
        RuntimeRecoveryFailureKind::UnsupportedState,
        runtime_identity(vault),
    )?;
    let initial = stage(
        RuntimeRecoveryFailureKind::UnsupportedState,
        super::plan(vault),
    )?;
    validate_plan(&initial, request)?;
    let mut conn = stage(
        RuntimeRecoveryFailureKind::DatabaseBusy,
        (|| -> Result<_> {
            let conn = Connection::open_with_flags(
                vault.index_db_path(),
                OpenFlags::SQLITE_OPEN_READ_WRITE,
            )?;
            conn.busy_timeout(Duration::from_secs(2))?;
            Ok(conn)
        })(),
    )?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| failure(RuntimeRecoveryFailureKind::DatabaseBusy))?;
    stage(
        RuntimeRecoveryFailureKind::SourceChanged,
        verify_runtime_identity(vault, &identity),
    )?;
    // BEGIN IMMEDIATEだけでは行を書かない。以後backupと復元が終わるまで他writerを入れない。
    let locked = stage(
        RuntimeRecoveryFailureKind::SourceChanged,
        super::plan(vault),
    )?;
    validate_plan(&locked, request)?;
    let preserved = stage(
        RuntimeRecoveryFailureKind::VerificationFailed,
        ledger_snapshot(&tx),
    )?;
    let meta_before = stage(
        RuntimeRecoveryFailureKind::VerificationFailed,
        untouched_meta_digest(&tx),
    )?;
    stage(
        RuntimeRecoveryFailureKind::UnsupportedState,
        validate_triggers(&tx),
    )?;
    stage(
        RuntimeRecoveryFailureKind::BackupFailed,
        hook(ApplyPoint::BeforeBackup),
    )?;
    stage(
        RuntimeRecoveryFailureKind::SourceChanged,
        verify_runtime_identity(vault, &identity),
    )?;
    let backup = stage(
        RuntimeRecoveryFailureKind::BackupFailed,
        create_verified_backup(vault, &tx, request, &preserved),
    )?;
    stage(
        RuntimeRecoveryFailureKind::VerificationFailed,
        hook(ApplyPoint::BackupVerified),
    )?;
    // 退避中のファイル更新も反映前に止める。
    stage(
        RuntimeRecoveryFailureKind::SourceChanged,
        verify_markdown(vault, &backup.notes),
    )?;
    stage(
        RuntimeRecoveryFailureKind::VerificationFailed,
        verify_backup_files(&backup.directory, request, &preserved),
    )?;
    stage(
        RuntimeRecoveryFailureKind::SourceChanged,
        verify_runtime_identity(vault, &identity),
    )?;
    stage(
        RuntimeRecoveryFailureKind::RestoreFailed,
        restore_rows_and_indexes(vault, &tx, &backup.notes),
    )?;
    stage(
        RuntimeRecoveryFailureKind::VerificationFailed,
        hook(ApplyPoint::RowsRestored),
    )?;
    stage(
        RuntimeRecoveryFailureKind::SourceChanged,
        verify_runtime_identity(vault, &identity),
    )?;
    stage(
        RuntimeRecoveryFailureKind::VerificationFailed,
        (|| -> Result<()> {
            verify_restored(&tx, vault, &backup.notes)?;
            ensure!(
                ledger_snapshot(&tx)? == preserved,
                "復旧で既存台帳が変わった"
            );
            ensure!(
                untouched_meta_digest(&tx)? == meta_before,
                "復旧対象外metadataが変わった"
            );
            verify_backup_files(&backup.directory, request, &preserved)?;
            verify_markdown(vault, &backup.notes)?;
            Ok(())
        })(),
    )?;
    // 元のnote/schemaへの変更が全て同じtransactionに揃ってから宣言を切り替える。
    stage(
        RuntimeRecoveryFailureKind::VerificationFailed,
        crate::index::finish_runtime_recovery(&tx),
    )?;
    stage(
        RuntimeRecoveryFailureKind::VerificationFailed,
        hook(ApplyPoint::BeforeCommit),
    )?;
    stage(
        RuntimeRecoveryFailureKind::SourceChanged,
        verify_markdown(vault, &backup.notes),
    )?;
    stage(
        RuntimeRecoveryFailureKind::SourceChanged,
        verify_runtime_identity(vault, &identity),
    )?;
    tx.commit()
        .map_err(|_| failure(RuntimeRecoveryFailureKind::CommitFailed))?;
    let summary = locked.summary.expect("validate_planでsummaryを検証済み");
    Ok(RuntimeRecoveryReceipt {
        backup_id: backup.id,
        plan_digest: request.expected_plan_digest.clone(),
        restored_notes: backup.notes.len() as u64,
        verified_review_notes: summary.completed_review_matches,
        unproven_notes: summary.latest_state_unproven,
        backup_verified: true,
        preserved_ledgers: preserved,
        automatic_processing_resumed: false,
        restart_required: true,
    })
}

fn validate_plan(plan: &RuntimeRecoveryPlan, request: &RuntimeRecoveryRequest) -> Result<()> {
    if plan.plan_digest.as_deref() != Some(request.expected_plan_digest.as_str()) {
        return Err(failure(RuntimeRecoveryFailureKind::PlanChanged));
    }
    if !plan.supported_reset_shape
        || !plan.snapshot_complete
        || !plan.existing_markdown_coverage_complete
        || plan.blocking_issue_count != 0
        || plan.summary.is_none()
        || plan.notes.iter().any(|note| {
            note.job_state.is_some_and(|state| {
                !matches!(
                    state,
                    RecoveryJobState::Completed | RecoveryJobState::Blocked
                )
            })
        })
    {
        return Err(failure(RuntimeRecoveryFailureKind::UnsupportedState));
    }
    if plan
        .summary
        .as_ref()
        .is_some_and(|summary| summary.latest_state_unproven > 0)
        && !request.acknowledge_unproven
    {
        return Err(failure(RuntimeRecoveryFailureKind::UnprovenNotAcknowledged));
    }
    Ok(())
}

fn ledger_snapshot(conn: &Connection) -> Result<Vec<RuntimeRecoveryLedgerReceipt>> {
    PRESERVED_TABLES
        .iter()
        .map(|table| {
            let mut digest = Sha256::new();
            super::hash_query(conn, &format!("SELECT * FROM {table}"), &mut digest)?;
            let rows: i64 =
                conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })?;
            Ok(RuntimeRecoveryLedgerReceipt {
                table: (*table).into(),
                rows: rows.try_into()?,
                sha256: format!("sha256:{:x}", digest.finalize()),
            })
        })
        .collect()
}

fn untouched_meta_digest(conn: &Connection) -> Result<Vec<u8>> {
    let mut digest = Sha256::new();
    super::hash_query(
        conn,
        "SELECT * FROM meta WHERE key NOT IN ('schema','runtime_store')",
        &mut digest,
    )?;
    Ok(digest.finalize().to_vec())
}

fn database_logical_hash(conn: &Connection) -> Result<String> {
    let mut digest = Sha256::new();
    super::hash_query(
        conn,
        "SELECT type,name,tbl_name,sql FROM sqlite_schema",
        &mut digest,
    )?;
    let mut statement =
        conn.prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")?;
    let tables = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // 計画のdurable一覧以外も退避する。schema由来の名前を識別子として引用し、
    // ページ配置に依存しない全行の型付きhashを退避元・退避先で比べる。
    for table in tables {
        super::hash_field(&mut digest, table.as_bytes());
        super::hash_query(
            conn,
            &format!("SELECT * FROM \"{}\"", table.replace('"', "\"\"")),
            &mut digest,
        )?;
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn trigger_definitions() -> Result<BTreeMap<&'static str, &'static str>> {
    let mut definitions: BTreeMap<_, _> = crate::distillation_jobs::trigger_definitions()?
        .into_iter()
        .collect();
    for artifact in DerivedArtifact::ALL {
        for object in artifact.spec().objects {
            if matches!(object.kind, ObjectKind::Trigger) {
                definitions.insert(object.name, object.create_sql);
            }
        }
    }
    Ok(definitions)
}

fn normalize_sql(sql: &str) -> String {
    sql.trim()
        .trim_end_matches(';')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn validate_triggers(conn: &Connection) -> Result<()> {
    let definitions = trigger_definitions()?;
    let mut statement = conn.prepare("SELECT name,sql FROM sqlite_schema WHERE type='trigger'")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(0)?;
        let sql: String = row.get(1)?;
        ensure!(
            definitions
                .get(name.as_str())
                .is_some_and(|expected| normalize_sql(expected) == normalize_sql(&sql)),
            "未知または改変されたtriggerを復旧できない"
        );
    }
    // 公開reportの個別一覧は上限付きなので、実行中jobの制限は全行にも強制する。
    let unsafe_jobs: i64 = conn.query_row("SELECT count(*) FROM distillation_jobs WHERE state NOT IN ('completed','blocked') OR lease_token IS NOT NULL OR lease_expires_at IS NOT NULL", [], |row| row.get(0))?;
    ensure!(unsafe_jobs == 0, "復旧中に進行し得るjobがある");
    Ok(())
}

fn restore_rows_and_indexes(vault: &Vault, conn: &Connection, notes: &[RawNote]) -> Result<()> {
    for name in trigger_definitions()?.keys() {
        conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {name}"))?;
    }
    crate::index::prepare_empty_runtime_recovery(conn)?;
    for note in notes {
        crate::index::restore_document_row(conn, &note.id, note.mtime, &note.document)?;
    }
    for artifact in DerivedArtifact::ALL {
        crate::derived_index::force_rebuild_in_transaction(vault, conn, artifact)?;
    }
    for (_, definition) in crate::distillation_jobs::trigger_definitions()? {
        conn.execute_batch(definition)?;
    }
    Ok(())
}

fn verify_restored(conn: &Connection, vault: &Vault, notes: &[RawNote]) -> Result<()> {
    let count: i64 = conn.query_row("SELECT count(*) FROM notes", [], |row| row.get(0))?;
    ensure!(count == notes.len() as i64, "復元件数が一致しない");
    for note in notes {
        let raw: String = conn.query_row(
            "SELECT document FROM notes WHERE id=?1",
            [&note.id],
            |row| row.get(0),
        )?;
        ensure!(raw == note.document, "復元した原文が一致しない");
    }
    for artifact in DerivedArtifact::ALL {
        ensure!(
            matches!(
                (artifact.spec().health)(vault, conn)?,
                crate::derived_index::ArtifactHealth::Ready
            ),
            "派生索引の検証に失敗"
        );
    }
    crate::index::validate_authority_index(conn)?;
    crate::distillation_jobs::verify_schema(conn)?;
    let missing: i64 = conn.query_row("SELECT count(*) FROM distillation_jobs j LEFT JOIN notes n ON n.id=j.note WHERE n.id IS NULL OR n.distillation_allowed<>1 OR n.normal_reference_allowed<>1", [], |row| row.get(0))?;
    ensure!(missing == 0, "current jobと復元ノートが一致しない");
    let mut check = conn.prepare("PRAGMA integrity_check")?;
    let messages = check
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(messages == ["ok"], "SQLite integrity checkに失敗");
    Ok(())
}

fn capture_markdown(vault: &Vault) -> Result<(Vec<RawNote>, Vec<u8>)> {
    let files = vault.list_note_files()?;
    let mut notes = Vec::new();
    let mut digest = Sha256::new();
    for (id, path) in files {
        NoteId::parse(&id)?;
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(metadata.file_type().is_file(), "原文が通常ファイルではない");
        let document = fs::read_to_string(&path)?;
        let mtime = metadata
            .modified()?
            .duration_since(UNIX_EPOCH)?
            .as_nanos()
            .try_into()?;
        super::hash_field(&mut digest, id.as_bytes());
        super::hash_field(&mut digest, document.as_bytes());
        notes.push(RawNote {
            id,
            document,
            mtime,
        });
    }
    Ok((notes, digest.finalize().to_vec()))
}

fn verify_markdown(vault: &Vault, notes: &[RawNote]) -> Result<()> {
    let mut diagnostic = RuntimeRecoveryPlan::new();
    let (observed, _) = super::read_markdown(vault, &mut diagnostic)
        .ok_or_else(|| failure(RuntimeRecoveryFailureKind::SourceChanged))?;
    ensure!(
        diagnostic.blocking_issue_count == 0 && observed.len() == notes.len(),
        "原文の走査結果が変わった"
    );
    for note in notes {
        ensure!(
            observed
                .get(&note.id)
                .is_some_and(|source| source.document_hash
                    == crate::distillation::sha256(note.document.as_bytes())),
            "原文が計画後に変わった"
        );
    }
    Ok(())
}

fn create_verified_backup(
    vault: &Vault,
    write_conn: &Connection,
    request: &RuntimeRecoveryRequest,
    preserved: &[RuntimeRecoveryLedgerReceipt],
) -> Result<VerifiedBackup> {
    ensure!(!write_conn.is_autocommit(), "退避前のwrite排他がない");
    verify_runtime_paths(vault)?;
    let mut source =
        Connection::open_with_flags(vault.index_db_path(), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    source.busy_timeout(Duration::from_secs(2))?;
    source.execute_batch("PRAGMA query_only=ON;")?;
    let source_tx = source.transaction()?;
    let mut report = RuntimeRecoveryPlan::new();
    let database = super::read_database_inner(&source_tx, &mut report)?;
    let logical_hash = database_logical_hash(&source_tx)?;
    let (notes, markdown_digest) = capture_markdown(vault)?;
    ensure!(
        super::snapshot_digest(&database.digest, &markdown_digest) == request.expected_plan_digest,
        "退避する版が計画と異なる"
    );
    let root = vault.root.join(".kb").join(BACKUP_DIRECTORY);
    ensure_private_directory(&root)?;
    let id = format!("recovery-{:032x}", rand::random::<u128>());
    let directory = root.join(&id);
    create_private_directory(&directory)?;
    let db_path = directory.join("runtime.db");
    create_private_file(&db_path)?.sync_all()?;
    let mut destination = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    destination.busy_timeout(Duration::from_millis(100))?;
    {
        let backup = Backup::new(&source_tx, &mut destination)?;
        let deadline = Instant::now() + BACKUP_TIMEOUT;
        loop {
            ensure!(Instant::now() < deadline, "SQLite退避の制限時間を超えた");
            match backup.step(128)? {
                StepResult::Done => break,
                StepResult::Busy | StepResult::Locked => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                StepResult::More => {}
                _ => anyhow::bail!("SQLite退避の状態が不明"),
            }
        }
    }
    destination.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;")?;
    let mut backup_report = RuntimeRecoveryPlan::new();
    let backup_database = super::read_database_inner(&destination, &mut backup_report)?;
    ensure!(
        database.digest == backup_database.digest
            && logical_hash == database_logical_hash(&destination)?,
        "DB退避の論理内容が一致しない"
    );
    ensure!(
        ledger_snapshot(&destination)? == preserved,
        "DB退避の台帳が一致しない"
    );
    drop(destination);
    File::open(&db_path)?.sync_all()?;
    source_tx.rollback()?;
    let markdown_root = directory.join("markdown");
    create_private_directory(&markdown_root)?;
    let mut entries = Vec::new();
    for note in &notes {
        let relative = NoteId::parse(&note.id)?.markdown_relative_path();
        let target = markdown_root.join(relative);
        create_private_parents(&markdown_root, target.parent().expect("退避先parent"))?;
        let mut file = create_private_file(&target)?;
        file.write_all(note.document.as_bytes())?;
        file.sync_all()?;
        entries.push(BackupNoteManifest {
            note: note.id.clone(),
            bytes: note.document.len() as u64,
            sha256: crate::distillation::sha256(note.document.as_bytes()),
        });
    }
    let manifest = BackupManifest {
        format_version: 1,
        plan_digest: request.expected_plan_digest.clone(),
        database_logical_sha256: logical_hash,
        notes: entries,
        preserved_ledgers: preserved.to_vec(),
    };
    let mut file = create_private_file(&directory.join("manifest.json"))?;
    file.write_all(&serde_json::to_vec(&manifest)?)?;
    file.sync_all()?;
    verify_backup_files(&directory, request, preserved)?;
    sync_directories(&directory)?;
    File::open(&root)?.sync_all()?;
    verify_markdown(vault, &notes)?;
    Ok(VerifiedBackup {
        id,
        directory,
        notes,
    })
}

fn verify_backup_files(
    directory: &Path,
    request: &RuntimeRecoveryRequest,
    preserved: &[RuntimeRecoveryLedgerReceipt],
) -> Result<()> {
    // 最終要素だけでは祖先directoryのsymlink置換を見逃す。
    let backup_root = directory
        .parent()
        .ok_or_else(|| anyhow::anyhow!("退避rootがない"))?;
    let runtime_root = backup_root
        .parent()
        .ok_or_else(|| anyhow::anyhow!("runtime rootがない"))?;
    let vault_root = runtime_root
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Vault rootがない"))?;
    ensure!(
        backup_root
            .file_name()
            .is_some_and(|name| name == BACKUP_DIRECTORY)
            && runtime_root.file_name().is_some_and(|name| name == ".kb"),
        "退避領域が製品固定位置ではない"
    );
    for ancestor in [vault_root, runtime_root, backup_root, directory] {
        ensure_regular_directory(ancestor)?;
    }
    ensure_regular_file(&directory.join("manifest.json"))?;
    ensure_regular_file(&directory.join("runtime.db"))?;
    let manifest: BackupManifest =
        serde_json::from_slice(&fs::read(directory.join("manifest.json"))?)?;
    ensure!(
        manifest.format_version == 1
            && manifest.plan_digest == request.expected_plan_digest
            && manifest.preserved_ledgers == preserved,
        "退避manifestが一致しない"
    );
    let mut backup = Connection::open_with_flags(
        directory.join("runtime.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    backup.execute_batch("PRAGMA query_only=ON;")?;
    let backup = backup.transaction()?;
    let mut report = RuntimeRecoveryPlan::new();
    let source = super::read_database_inner(&backup, &mut report)?;
    ensure!(
        manifest.database_logical_sha256 == database_logical_hash(&backup)?,
        "退避DBが変わった"
    );
    ensure!(ledger_snapshot(&backup)? == preserved, "退避台帳が変わった");
    verify_backup_inventory(&directory.join("markdown"), &manifest.notes)?;
    let mut digest = Sha256::new();
    let mut previous: Option<&str> = None;
    for note in &manifest.notes {
        ensure!(
            previous.is_none_or(|id| id < note.note.as_str()),
            "退避ノートが重複または不整列"
        );
        let id = NoteId::parse(&note.note)?;
        let bytes = fs::read(directory.join("markdown").join(id.markdown_relative_path()))?;
        ensure!(
            bytes.len() as u64 == note.bytes && crate::distillation::sha256(&bytes) == note.sha256,
            "退避原文が一致しない"
        );
        super::hash_field(&mut digest, note.note.as_bytes());
        super::hash_field(&mut digest, &bytes);
        previous = Some(&note.note);
    }
    ensure!(
        super::snapshot_digest(&source.digest, &digest.finalize()) == request.expected_plan_digest,
        "退避全体が計画と一致しない"
    );
    backup.rollback()?;
    Ok(())
}

fn ensure_regular_file(path: &Path) -> Result<()> {
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_file(),
        "対象が通常ファイルではない"
    );
    Ok(())
}

fn ensure_regular_directory(path: &Path) -> Result<()> {
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_dir(),
        "対象が通常directoryではない"
    );
    Ok(())
}

fn verify_runtime_paths(vault: &Vault) -> Result<()> {
    ensure_regular_directory(&vault.root)?;
    let root = vault.root.join(".kb");
    ensure_regular_directory(&root)?;
    ensure_regular_file(&vault.index_db_path())?;
    for sidecar in ["index.db-wal", "index.db-shm", "index.db-journal"] {
        match fs::symlink_metadata(root.join(sidecar)) {
            Ok(metadata) => ensure!(
                metadata.file_type().is_file(),
                "SQLite付随物が通常ファイルではない"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

// SQLiteの排他はinodeに属する。最初のwrite後に同内容の別ファイルへ置き換わると
// DBMOVED保護でもcommit成功になり得るため、復旧対象のdirectoryとDBの同一性も固定する。
fn runtime_identity(vault: &Vault) -> Result<[[u64; 2]; 3]> {
    verify_runtime_paths(vault)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let paths = [
            vault.root.clone(),
            vault.root.join(".kb"),
            vault.index_db_path(),
        ];
        let mut identities = [[0; 2]; 3];
        for (index, path) in paths.iter().enumerate() {
            let metadata = fs::symlink_metadata(path)?;
            ensure!(
                !metadata.file_type().is_symlink(),
                "復旧対象がsymlinkへ変わった"
            );
            identities[index] = [metadata.dev(), metadata.ino()];
        }
        Ok(identities)
    }
    #[cfg(not(unix))]
    anyhow::bail!("このOSでは復旧対象のfile identityを検証できない")
}

fn verify_runtime_identity(vault: &Vault, expected: &[[u64; 2]; 3]) -> Result<()> {
    ensure!(
        runtime_identity(vault)? == *expected,
        "復旧対象のfile identityが変わった"
    );
    Ok(())
}

fn verify_backup_inventory(root: &Path, notes: &[BackupNoteManifest]) -> Result<()> {
    ensure!(
        fs::symlink_metadata(root)?.file_type().is_dir(),
        "退避原文領域が通常directoryではない"
    );
    let mut expected = notes
        .iter()
        .map(|note| Ok(NoteId::parse(&note.note)?.markdown_relative_path()))
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(expected.len() == notes.len(), "退避原文が重複している");
    for entry in walkdir::WalkDir::new(root).min_depth(1) {
        let entry = entry?;
        if entry.file_type().is_dir() {
            continue;
        }
        ensure!(
            entry.file_type().is_file() && expected.remove(entry.path().strip_prefix(root)?),
            "退避原文一覧がmanifestと一致しない"
        );
    }
    ensure!(expected.is_empty(), "退避原文に不足がある");
    Ok(())
}

fn create_private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}
fn create_private_directory(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}
fn ensure_private_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => ensure!(
            metadata.file_type().is_dir(),
            "退避領域が通常directoryではない"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_private_directory(path)?
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
fn create_private_parents(root: &Path, parent: &Path) -> Result<()> {
    let mut current = root.to_path_buf();
    for component in parent.strip_prefix(root)?.components() {
        current.push(component);
        ensure_private_directory(&current)?;
    }
    Ok(())
}
fn sync_directories(root: &Path) -> Result<()> {
    for entry in walkdir::WalkDir::new(root).contents_first(true) {
        let entry = entry?;
        if entry.file_type().is_dir() {
            File::open(entry.path())?.sync_all()?;
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::frontmatter::{Frontmatter, Note};
    use rusqlite::params;

    fn setup(wal: bool) -> (tempfile::TempDir, Vault, RuntimeRecoveryRequest) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        conn.execute_batch("DROP TRIGGER distillation_jobs_insert; DROP TRIGGER distillation_jobs_update;
            DROP TRIGGER distillation_jobs_hide; DROP TRIGGER distillation_jobs_delete;
            ALTER TABLE notes DROP COLUMN normal_reference_allowed;
            ALTER TABLE notes DROP COLUMN distillation_allowed;
            UPDATE meta SET value='7' WHERE key='schema'; DELETE FROM meta WHERE key='runtime_store';
            INSERT INTO meta VALUES('preserved_fixture','private metadata');").unwrap();
        for (id, origin, state) in [
            ("notes/reviewed", "agent", Some("completed")),
            ("notes/blocked", "agent", Some("blocked")),
            ("notes/human", "human", None),
        ] {
            let mut front = Frontmatter::new_note("復旧fixture");
            front.origin = Some(origin.into());
            front.tags = vec!["test".into()];
            let raw = format!(
                "{}\n\n",
                Note {
                    front,
                    body: format!("private contents {id}")
                }
                .to_file_string()
                .unwrap()
            );
            fs::write(vault.root.join(format!("{id}.md")), &raw).unwrap();
            if let Some(state) = state {
                conn.execute("INSERT INTO distillation_jobs(note,generation,state,reason,queued_at,available_at,reviewed_hash)
                    VALUES(?1,3,?2,'preserve exact',100,0,?3)",params![id,state,(state=="completed").then(||crate::distillation::sha256(raw.as_bytes()))]).unwrap();
            }
            if state == Some("completed") {
                let docs = serde_json::to_string(&BTreeMap::from([(id, &raw)])).unwrap();
                conn.execute("INSERT INTO distillation_job_runs VALUES('run:preserve',?1,2,90,'applied','reason','digest',?2,?2,'client')",params![id,docs]).unwrap();
            }
        }
        conn.execute_batch("INSERT INTO distillation_runs VALUES('execution','plan','before','after','{}','{}','{}','client','time','applied',NULL,NULL);
            INSERT INTO action_receipts(receipt_id,workspace,request_hash,idempotency_key,request_json,decision_json,status,reserved_at)
                VALUES('receipt','workspace','request','idempotent','{}','{}','pending',100);
            INSERT INTO action_capability_uses VALUES('capability','issuer','receipt');").unwrap();
        if !wal {
            conn.execute_batch("PRAGMA journal_mode=DELETE;").unwrap();
        }
        drop(conn);
        let report = super::super::plan(&vault).unwrap();
        assert!(
            report.supported_reset_shape
                && report.snapshot_complete
                && report.blocking_issue_count == 0
        );
        let request = RuntimeRecoveryRequest {
            expected_plan_digest: report.plan_digest.unwrap(),
            acknowledge_unproven: true,
        };
        (dir, vault, request)
    }

    fn original(vault: &Vault) -> (Vec<u8>, std::time::SystemTime) {
        (
            fs::read(vault.index_db_path()).unwrap(),
            fs::metadata(vault.index_db_path())
                .unwrap()
                .modified()
                .unwrap(),
        )
    }

    #[test]
    fn recovery_request_rejects_unknown_operation_or_path_fields() {
        assert!(
            serde_json::from_value::<RuntimeRecoveryRequest>(serde_json::json!({
                "expected_plan_digest": "sha256:fixture",
                "acknowledge_unproven": true,
                "backup_path": "/private/unrequested"
            }))
            .is_err()
        );
    }

    /// 2026-09-07: 復旧時にjob triggerを発火させず、原文と全台帳をそのまま戻す。
    #[test]
    fn restores_raw_markdown_and_preserves_all_observed_ledgers_with_verified_backup() {
        let (_dir, vault, request) = setup(false);
        let conn = Connection::open(vault.index_db_path()).unwrap();
        let before = ledger_snapshot(&conn).unwrap();
        drop(conn);
        let raw = fs::read_to_string(vault.root.join("notes/reviewed.md")).unwrap();
        assert_ne!(Note::parse(&raw).unwrap().to_file_string().unwrap(), raw);
        let receipt = apply(&vault, &request).unwrap();
        assert_eq!(receipt.restored_notes, 3);
        assert_eq!(receipt.verified_review_notes, 1);
        assert_eq!(receipt.unproven_notes, 2);
        assert!(
            receipt.backup_verified
                && !receipt.automatic_processing_resumed
                && receipt.restart_required
        );
        assert_eq!(receipt.preserved_ledgers, before);
        let conn = Connection::open(vault.index_db_path()).unwrap();
        assert_eq!(ledger_snapshot(&conn).unwrap(), before);
        assert_eq!(
            conn.query_row(
                "SELECT document FROM notes WHERE id='notes/reviewed'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            raw
        );
        assert_eq!(
            conn.query_row("SELECT value FROM meta WHERE key='schema'", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
            "10"
        );
        assert_eq!(
            conn.query_row(
                "SELECT value FROM meta WHERE key='runtime_store'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "db-v1"
        );
        assert_eq!(
            fs::read_to_string(vault.root.join("notes/reviewed.md")).unwrap(),
            raw
        );
        crate::distillation_jobs::verify_schema(&conn).unwrap();
        let backup = vault
            .root
            .join(".kb")
            .join(BACKUP_DIRECTORY)
            .join(&receipt.backup_id);
        verify_backup_files(&backup, &request, &before).unwrap();
        let json = serde_json::to_string(&receipt).unwrap();
        assert!(!json.contains("private") && !json.contains(&vault.root.display().to_string()));
        drop(conn);
        // 正常化済みへの再適用は古い計画として拒否し、履歴を増やさない。
        assert_eq!(
            failure_kind(&apply(&vault, &request).unwrap_err()),
            Some(RuntimeRecoveryFailureKind::PlanChanged)
        );
        let opened = crate::index::open_db(&vault).unwrap();
        assert_eq!(ledger_snapshot(&opened).unwrap(), before);
        assert_eq!(
            fs::read_to_string(vault.root.join("notes/reviewed.md")).unwrap(),
            raw
        );
    }

    #[test]
    fn acknowledgement_stale_plan_running_jobs_and_unknown_triggers_fail_before_backup() {
        for case in ["ack", "stale", "running", "trigger"] {
            let (_dir, vault, mut request) = setup(false);
            if case == "ack" {
                request.acknowledge_unproven = false;
            }
            if case == "stale" {
                fs::write(vault.root.join("notes/blocked.md"), "changed").unwrap();
            }
            if case == "running" || case == "trigger" {
                let conn = Connection::open(vault.index_db_path()).unwrap();
                if case == "running" {
                    conn.execute(
                        "UPDATE distillation_jobs SET state='running' WHERE note='notes/blocked'",
                        [],
                    )
                    .unwrap();
                } else {
                    conn.execute_batch("CREATE TRIGGER unknown_write AFTER INSERT ON notes BEGIN DELETE FROM distillation_job_runs; END").unwrap();
                }
                drop(conn);
                request.expected_plan_digest =
                    super::super::plan(&vault).unwrap().plan_digest.unwrap();
            }
            let before = original(&vault);
            let error = apply(&vault, &request).unwrap_err();
            assert!(failure_kind(&error).is_some());
            assert_eq!(original(&vault), before, "{case}");
            assert!(!vault.root.join(".kb").join(BACKUP_DIRECTORY).exists());
        }
    }

    /// 2026-09-07: 退避失敗・反映途中・commit直前の失敗は全て元DBへrollbackする。
    #[test]
    fn backup_and_mid_restore_failures_leave_original_database_and_markdown_unchanged() {
        for point in [
            ApplyPoint::BeforeBackup,
            ApplyPoint::BackupVerified,
            ApplyPoint::RowsRestored,
            ApplyPoint::BeforeCommit,
        ] {
            let (_dir, vault, request) = setup(false);
            let before = original(&vault);
            let markdown = fs::read(vault.root.join("notes/reviewed.md")).unwrap();
            let result = apply_with_hook(&vault, &request, |at| {
                ensure!(at != point, "fault injection");
                Ok(())
            });
            assert!(result.is_err(), "{point:?}");
            assert_eq!(
                fs::read(vault.index_db_path()).unwrap(),
                before.0,
                "{point:?}"
            );
            if matches!(point, ApplyPoint::BeforeBackup | ApplyPoint::BackupVerified) {
                assert_eq!(original(&vault).1, before.1);
            }
            assert_eq!(
                fs::read(vault.root.join("notes/reviewed.md")).unwrap(),
                markdown
            );
            assert!(super::super::plan(&vault).unwrap().supported_reset_shape);
        }
    }

    #[test]
    fn backup_creation_or_manifest_verification_failure_prevents_original_mutation() {
        let (_dir, vault, request) = setup(false);
        let before = original(&vault);
        fs::write(
            vault.root.join(".kb").join(BACKUP_DIRECTORY),
            "not a directory",
        )
        .unwrap();
        assert_eq!(
            failure_kind(&apply(&vault, &request).unwrap_err()),
            Some(RuntimeRecoveryFailureKind::BackupFailed)
        );
        assert_eq!(original(&vault), before);

        let (_dir, vault, request) = setup(false);
        let before = original(&vault);
        let result = apply_with_hook(&vault, &request, |point| {
            if point == ApplyPoint::BackupVerified {
                let directory = fs::read_dir(vault.root.join(".kb").join(BACKUP_DIRECTORY))?
                    .next()
                    .unwrap()?
                    .path();
                fs::write(
                    directory.join("markdown/notes/reviewed.md"),
                    "damaged backup",
                )?;
            }
            Ok(())
        });
        assert_eq!(
            failure_kind(&result.unwrap_err()),
            Some(RuntimeRecoveryFailureKind::VerificationFailed)
        );
        assert_eq!(original(&vault), before);
    }

    #[test]
    fn full_backup_table_hash_and_inventory_reject_tampering_before_original_mutation() {
        for case in ["table", "extra_note", "symlink"] {
            let (_dir, vault, mut request) = setup(false);
            let conn = Connection::open(vault.index_db_path()).unwrap();
            conn.execute_batch(
                "CREATE TABLE \"extra\"\"table\"(blob BLOB,number INTEGER,decimal REAL,absent TEXT);
                 INSERT INTO \"extra\"\"table\" VALUES(x'616263',41,1.5,NULL);",
            )
            .unwrap();
            drop(conn);
            request.expected_plan_digest = super::super::plan(&vault).unwrap().plan_digest.unwrap();
            let before = original(&vault);
            let result = apply_with_hook(&vault, &request, |point| {
                if point == ApplyPoint::BackupVerified {
                    let directory = fs::read_dir(vault.root.join(".kb").join(BACKUP_DIRECTORY))?
                        .next()
                        .unwrap()?
                        .path();
                    match case {
                        "table" => {
                            let backup = Connection::open(directory.join("runtime.db"))?;
                            backup.execute("UPDATE \"extra\"\"table\" SET number=42", [])?;
                        }
                        "extra_note" => {
                            fs::write(directory.join("markdown/unlisted.md"), "extra")?;
                        }
                        _ => {
                            #[cfg(unix)]
                            {
                                let target = directory.join("markdown/notes/reviewed.md");
                                fs::remove_file(&target)?;
                                std::os::unix::fs::symlink(
                                    vault.root.join("notes/reviewed.md"),
                                    target,
                                )?;
                            }
                            #[cfg(not(unix))]
                            anyhow::bail!("symlink fault injection");
                        }
                    }
                }
                Ok(())
            });
            assert_eq!(
                failure_kind(&result.unwrap_err()),
                Some(RuntimeRecoveryFailureKind::VerificationFailed),
                "{case}"
            );
            assert_eq!(original(&vault), before, "{case}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn storage_or_backup_symlinks_are_rejected_without_following_them_for_writes() {
        for internal_root in [true, false] {
            let (dir, vault, request) = setup(false);
            let before = original(&vault);
            let outside = dir.path().join("outside");
            if internal_root {
                fs::rename(vault.root.join(".kb"), &outside).unwrap();
                std::os::unix::fs::symlink(&outside, vault.root.join(".kb")).unwrap();
            } else {
                fs::create_dir(&outside).unwrap();
                std::os::unix::fs::symlink(&outside, vault.root.join(".kb").join(BACKUP_DIRECTORY))
                    .unwrap();
            }
            assert!(apply(&vault, &request).is_err());
            assert_eq!(original(&vault), before);
            if internal_root {
                assert!(!outside.join(BACKUP_DIRECTORY).exists());
            } else {
                assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
            }
        }
    }

    #[test]
    fn markdown_change_after_backup_aborts_instead_of_overwriting_the_new_source() {
        let (_dir, vault, request) = setup(false);
        let before = original(&vault);
        let changed = "user changed the source after plan";
        let result = apply_with_hook(&vault, &request, |point| {
            if point == ApplyPoint::BackupVerified {
                fs::write(vault.root.join("notes/blocked.md"), changed)?;
            }
            Ok(())
        });
        assert_eq!(
            failure_kind(&result.unwrap_err()),
            Some(RuntimeRecoveryFailureKind::SourceChanged)
        );
        assert_eq!(original(&vault), before);
        assert_eq!(
            fs::read_to_string(vault.root.join("notes/blocked.md")).unwrap(),
            changed
        );
    }

    /// 2026-09-07: COMMIT直前の同内容ファイル置換を、復旧成功として受け入れない。
    #[test]
    fn database_path_replacement_before_commit_does_not_report_success() {
        for phase in [
            ApplyPoint::BeforeBackup,
            ApplyPoint::BackupVerified,
            ApplyPoint::BeforeCommit,
        ] {
            let (dir, vault, request) = setup(false);
            let initial = fs::read(vault.index_db_path()).unwrap();
            let displaced = dir.path().join("displaced.db");
            let result = apply_with_hook(&vault, &request, |point| {
                if point == phase {
                    fs::rename(vault.index_db_path(), &displaced)?;
                    fs::copy(&displaced, vault.index_db_path())?;
                }
                Ok(())
            });
            assert_eq!(
                failure_kind(&result.unwrap_err()),
                Some(RuntimeRecoveryFailureKind::SourceChanged),
                "{phase:?}"
            );
            assert_eq!(fs::read(vault.index_db_path()).unwrap(), initial);
            assert_eq!(fs::read(displaced).unwrap(), initial);
        }
    }

    #[test]
    fn runtime_directory_identity_is_checked_before_backup_writes() {
        for replace_vault_root in [true, false] {
            let (dir, vault, request) = setup(false);
            let initial = fs::read(vault.index_db_path()).unwrap();
            let displaced = dir.path().join("displaced-directory");
            let result = apply_with_hook(&vault, &request, |point| {
                if point == ApplyPoint::BeforeBackup {
                    let target = if replace_vault_root {
                        vault.root.clone()
                    } else {
                        vault.root.join(".kb")
                    };
                    fs::rename(target, &displaced)?;
                    fs::create_dir_all(vault.root.join(".kb"))?;
                    let saved_db = if replace_vault_root {
                        displaced.join(".kb/index.db")
                    } else {
                        displaced.join("index.db")
                    };
                    fs::copy(saved_db, vault.index_db_path())?;
                }
                Ok(())
            });
            assert_eq!(
                failure_kind(&result.unwrap_err()),
                Some(RuntimeRecoveryFailureKind::SourceChanged)
            );
            assert_eq!(fs::read(vault.index_db_path()).unwrap(), initial);
            assert!(!vault.root.join(".kb").join(BACKUP_DIRECTORY).exists());
        }
    }

    #[test]
    fn backup_includes_committed_wal_and_write_exclusion_covers_backup_and_restore() {
        let (_dir, vault, mut request) = setup(true);
        let held = Connection::open(vault.index_db_path()).unwrap();
        held.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        held.execute(
            "UPDATE distillation_jobs SET reason='committed only in wal'",
            [],
        )
        .unwrap();
        request.expected_plan_digest = super::super::plan(&vault).unwrap().plan_digest.unwrap();
        let before = ledger_snapshot(&held).unwrap();
        let receipt = apply_with_hook(&vault, &request, |point| {
            if matches!(point, ApplyPoint::BackupVerified | ApplyPoint::RowsRestored) {
                let other = Connection::open(vault.index_db_path())?;
                other.busy_timeout(Duration::ZERO)?;
                assert!(
                    other
                        .execute(
                            "UPDATE distillation_jobs SET reason='concurrent writer'",
                            []
                        )
                        .is_err()
                );
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(receipt.preserved_ledgers, before);
        assert_eq!(ledger_snapshot(&held).unwrap(), before);
        let backup = vault
            .root
            .join(".kb")
            .join(BACKUP_DIRECTORY)
            .join(receipt.backup_id);
        verify_backup_files(&backup, &request, &before).unwrap();
    }
}
