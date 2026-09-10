//! アプリ更新前の永続形式の観測。署名検証・インストール許可・復元receiptは発行しない。
//!
//! 全登録先と明示KB_VAULTを検査するが、writerを停止するロックではない。
//! 前後のDB/WAL/registry変更は拒否するbest-effort観測であり、観測後の変更や
//! 旧新writerの共存、rollback成功を保証しない。DB本体と既存WALの永続内容の更新、
//! checkpoint・migrationは行わない。SQLiteの共有メモリ管理は許容する。
//! 事前観測後に別writerがWALを削除した場合、read-only openでも空sidecarを作り得る。
//! 観測できた差はSourceChangedで拒否するが、競合下でのsidecar作成ゼロは保証しない。

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, File, Metadata};
use std::io::{ErrorKind, Read};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use rusqlite::config::DbConfig;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const RUNTIME_STORE: &str = "db-v1";
const MAX_REGISTRY_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct PackageCompatibilityMetadata {
    pub runtime_store: String,
    pub database_schema: u32,
    pub persistent_compatibility_epoch: u32,
}

pub fn compiled_metadata() -> PackageCompatibilityMetadata {
    // 配布planと実行物が同じsourceを使う。epochは共存試験の合格票ではない。
    serde_json::from_str(include_str!("../update-compatibility.json"))
        .expect("同梱する更新互換性metadataは正しいJSON")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum CompatibilityFailureKind {
    PackageRuntimeStoreMismatch,
    PackageDatabaseSchemaMismatch,
    PackagePersistentEpochMismatch,
    RegistryLocationUnavailable,
    RegistryReadFailed,
    RegistryInvalid,
    RegistryChanged,
    ExplicitVaultInvalid,
    VaultPathInvalid,
    VaultUnavailable,
    DatabaseMissing,
    DatabaseReadFailed,
    DatabaseInvalid,
    DatabaseSchemaMissing,
    DatabaseSchemaInvalid,
    DatabaseSchemaMismatch,
    RuntimeStoreMissing,
    RuntimeStoreMismatch,
    DurableSchemaInvalid,
    DatabaseRequiresRecovery,
    SourceChanged,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct VaultCompatibilityObservation {
    pub registered_names: Vec<String>,
    pub explicitly_selected: bool,
    pub declared_schema: Option<u32>,
    pub runtime_store_matches: bool,
    pub blockers: Vec<CompatibilityFailureKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CompatibilityReport {
    pub format_version: u32,
    pub read_only: bool,
    pub compiled: PackageCompatibilityMetadata,
    pub package_metadata_matches: bool,
    pub registry_entries: Option<usize>,
    pub vaults: Vec<VaultCompatibilityObservation>,
    pub blockers: Vec<CompatibilityFailureKind>,
    /// この観測で互換性のblockerを検出しなかったことだけを表す。
    pub checks_passed: bool,
}

impl CompatibilityReport {
    fn new(metadata: &PackageCompatibilityMetadata) -> Self {
        let compiled = compiled_metadata();
        let mut blockers = Vec::new();
        if metadata.runtime_store != compiled.runtime_store {
            blockers.push(CompatibilityFailureKind::PackageRuntimeStoreMismatch);
        }
        if metadata.database_schema != compiled.database_schema {
            blockers.push(CompatibilityFailureKind::PackageDatabaseSchemaMismatch);
        }
        if metadata.persistent_compatibility_epoch != compiled.persistent_compatibility_epoch {
            blockers.push(CompatibilityFailureKind::PackagePersistentEpochMismatch);
        }
        Self {
            format_version: 1,
            read_only: true,
            compiled,
            package_metadata_matches: blockers.is_empty(),
            registry_entries: None,
            vaults: Vec::new(),
            blockers,
            checks_passed: false,
        }
    }
}

/// metadataは呼出側が署名済みpackageから取り出す。この関数自体は署名を検証しない。
/// 個人設定を使わない合成テストは、内部の明示source入口を利用する。
pub fn inspect(metadata: &PackageCompatibilityMetadata) -> CompatibilityReport {
    let path = match crate::registry::registry_path() {
        Ok(path) => path,
        Err(_) => {
            let mut report = CompatibilityReport::new(metadata);
            report
                .blockers
                .push(CompatibilityFailureKind::RegistryLocationUnavailable);
            return report;
        }
    };
    inspect_with_sources(&path, std::env::var_os("KB_VAULT").as_deref(), metadata)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictRegistry {
    vaults: Vec<StrictVaultEntry>,
    default: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictVaultEntry {
    name: String,
    path: PathBuf,
}

fn inspect_with_sources(
    registry_path: &Path,
    explicit_vault: Option<&OsStr>,
    metadata: &PackageCompatibilityMetadata,
) -> CompatibilityReport {
    let mut report = CompatibilityReport::new(metadata);
    if !report.package_metadata_matches {
        return report;
    }
    let bytes = match read_registry_bytes(registry_path) {
        Ok(bytes) => bytes,
        Err(kind) => {
            report.blockers.push(kind);
            return report;
        }
    };
    let registry = match bytes.as_deref() {
        Some(bytes) => match serde_json::from_slice::<StrictRegistry>(bytes) {
            Ok(registry) => registry,
            Err(_) => {
                report
                    .blockers
                    .push(CompatibilityFailureKind::RegistryInvalid);
                return report;
            }
        },
        None => StrictRegistry {
            vaults: Vec::new(),
            default: None,
        },
    };
    let mut names = BTreeSet::new();
    if registry
        .vaults
        .iter()
        .any(|entry| entry.name.trim().is_empty() || !names.insert(entry.name.clone()))
        || registry
            .default
            .as_ref()
            .is_some_and(|name| !names.contains(name))
    {
        report
            .blockers
            .push(CompatibilityFailureKind::RegistryInvalid);
        return report;
    }
    report.registry_entries = Some(registry.vaults.len());
    // symlinkは拒否し、同じcanonical pathの登録名だけをまとめる。別Vaultをdefaultで隠さない。
    let mut targets = BTreeMap::<PathBuf, VaultCompatibilityObservation>::new();
    for entry in registry.vaults {
        add_target(
            &mut targets,
            &mut report.vaults,
            entry.path,
            Some(entry.name),
        );
    }
    if let Some(explicit) = explicit_vault {
        if explicit.is_empty() {
            report
                .blockers
                .push(CompatibilityFailureKind::ExplicitVaultInvalid);
        } else {
            add_target(
                &mut targets,
                &mut report.vaults,
                PathBuf::from(explicit),
                None,
            );
        }
    }
    for (path, mut observation) in targets {
        inspect_vault(&path, &mut observation);
        report.vaults.push(observation);
    }
    if read_registry_bytes(registry_path).as_ref() != Ok(&bytes) {
        report
            .blockers
            .push(CompatibilityFailureKind::RegistryChanged);
    }
    report.checks_passed =
        report.blockers.is_empty() && report.vaults.iter().all(|vault| vault.blockers.is_empty());
    report
}

fn add_target(
    targets: &mut BTreeMap<PathBuf, VaultCompatibilityObservation>,
    invalid: &mut Vec<VaultCompatibilityObservation>,
    path: PathBuf,
    name: Option<String>,
) {
    let mut observation = VaultCompatibilityObservation {
        registered_names: name.iter().cloned().collect(),
        explicitly_selected: name.is_none(),
        declared_schema: None,
        runtime_store_matches: false,
        blockers: Vec::new(),
    };
    let canonical = match checked_path(&path) {
        Ok(Some(info)) if info.is_dir() => path.canonicalize().ok(),
        _ => None,
    };
    let Some(canonical) = canonical else {
        observation.blockers.push(if path.is_absolute() {
            CompatibilityFailureKind::VaultUnavailable
        } else {
            CompatibilityFailureKind::VaultPathInvalid
        });
        invalid.push(observation);
        return;
    };
    if let Some(existing) = targets.get_mut(&canonical) {
        existing
            .registered_names
            .extend(observation.registered_names);
        existing.explicitly_selected |= observation.explicitly_selected;
    } else {
        targets.insert(canonical, observation);
    }
}

// NotFound以外の失敗を空registryへ潰さない。親symlinkや非directoryも拒否する。
fn read_registry_bytes(path: &Path) -> Result<Option<Vec<u8>>, CompatibilityFailureKind> {
    let Some(info) =
        checked_path(path).map_err(|_| CompatibilityFailureKind::RegistryReadFailed)?
    else {
        return Ok(None);
    };
    if !info.is_file() || info.len() > MAX_REGISTRY_BYTES {
        return Err(CompatibilityFailureKind::RegistryReadFailed);
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(MAX_REGISTRY_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|_| CompatibilityFailureKind::RegistryReadFailed)?;
    if bytes.len() as u64 > MAX_REGISTRY_BYTES {
        return Err(CompatibilityFailureKind::RegistryReadFailed);
    }
    Ok(Some(bytes))
}

fn checked_path(path: &Path) -> std::io::Result<Option<Metadata>> {
    if !path.is_absolute() {
        return Err(std::io::Error::other("absolute path required"));
    }
    let mut current = PathBuf::new();
    let components: Vec<_> = path.components().collect();
    for (index, component) in components.iter().enumerate() {
        if matches!(component, Component::ParentDir | Component::CurDir) {
            return Err(std::io::Error::other("noncanonical path"));
        }
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(info) => {
                if info.file_type().is_symlink() || (index + 1 < components.len() && !info.is_dir())
                {
                    return Err(std::io::Error::other("nonregular path"));
                }
                if index + 1 == components.len() {
                    return Ok(Some(info));
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::other("empty path"))
}

#[derive(Debug, PartialEq, Eq)]
struct FileObservation {
    length: u64,
    modified: SystemTime,
    identity: (u64, u64),
    sha256: [u8; 32],
}

fn identity(info: &Metadata) -> (u64, u64) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (info.dev(), info.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = info;
        (0, 0)
    }
}

fn observe_file(path: &Path) -> Result<Option<FileObservation>, CompatibilityFailureKind> {
    let Some(before) =
        checked_path(path).map_err(|_| CompatibilityFailureKind::DatabaseReadFailed)?
    else {
        return Ok(None);
    };
    if !before.is_file() {
        return Err(CompatibilityFailureKind::DatabaseReadFailed);
    }
    let mut file = File::open(path).map_err(|_| CompatibilityFailureKind::DatabaseReadFailed)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| CompatibilityFailureKind::DatabaseReadFailed)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    let after = file
        .metadata()
        .map_err(|_| CompatibilityFailureKind::DatabaseReadFailed)?;
    if before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || identity(&before) != identity(&after)
    {
        return Err(CompatibilityFailureKind::SourceChanged);
    }
    Ok(Some(FileObservation {
        length: after.len(),
        modified: after
            .modified()
            .map_err(|_| CompatibilityFailureKind::DatabaseReadFailed)?,
        identity: identity(&after),
        sha256: hash.finalize().into(),
    }))
}

#[derive(Debug, PartialEq, Eq)]
struct DatabaseObservation {
    database: FileObservation,
    wal: Option<FileObservation>,
    journal: Option<FileObservation>,
}

fn observe_database(path: &Path) -> Result<DatabaseObservation, CompatibilityFailureKind> {
    let database = observe_file(path)?.ok_or(CompatibilityFailureKind::DatabaseMissing)?;
    let wal = observe_file(&path.with_file_name("index.db-wal"))?;
    let journal = observe_file(&path.with_file_name("index.db-journal"))?;
    if checked_path(&path.with_file_name("index.db-shm"))
        .map_err(|_| CompatibilityFailureKind::DatabaseReadFailed)?
        .is_some_and(|info| !info.is_file())
    {
        return Err(CompatibilityFailureKind::DatabaseReadFailed);
    }
    Ok(DatabaseObservation {
        database,
        wal,
        journal,
    })
}

fn inspect_vault(root: &Path, observation: &mut VaultCompatibilityObservation) {
    let path = root.join(".kb/index.db");
    let before = match observe_database(&path) {
        Ok(before) => before,
        Err(kind) => {
            observation.blockers.push(kind);
            return;
        }
    };
    // 復旧を要するjournalの解釈や削除を、この診断の責務へ広げない。
    if before.journal.is_some() {
        observation
            .blockers
            .push(CompatibilityFailureKind::DatabaseRequiresRecovery);
        return;
    }
    if let Err(kind) = inspect_database(&path, before.wal.is_none(), observation) {
        observation.blockers.push(kind);
    }
    // connectionのdrop後も確認し、最後のreaderによるWAL削除を見落とさない。
    if observe_database(&path).as_ref() != Ok(&before) {
        observation
            .blockers
            .push(CompatibilityFailureKind::SourceChanged);
    }
}

fn database_uri(path: &Path, immutable: bool) -> Result<String, CompatibilityFailureKind> {
    let text = path
        .to_str()
        .ok_or(CompatibilityFailureKind::VaultPathInvalid)?;
    let text = text.replace(std::path::MAIN_SEPARATOR, "/");
    let mut encoded = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~:".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    Ok(format!(
        "file:{encoded}?mode=ro{}",
        if immutable { "&immutable=1" } else { "" }
    ))
}

fn inspect_database(
    path: &Path,
    immutable: bool,
    observation: &mut VaultCompatibilityObservation,
) -> Result<(), CompatibilityFailureKind> {
    let mut connection = Connection::open_with_flags(
        database_uri(path, immutable)?,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| CompatibilityFailureKind::DatabaseReadFailed)?;
    // 2026-09-09: READ_ONLYでも最終connectionのcloseがcheckpoint済みWALを削除し得る。
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .map_err(|_| CompatibilityFailureKind::DatabaseReadFailed)?;
    connection
        .busy_timeout(Duration::from_secs(2))
        .and_then(|()| connection.execute_batch("PRAGMA query_only=ON;"))
        .map_err(|_| CompatibilityFailureKind::DatabaseReadFailed)?;
    let transaction = connection
        .transaction()
        .map_err(|_| CompatibilityFailureKind::DatabaseReadFailed)?;
    let integrity: Vec<String> = transaction
        .prepare("PRAGMA quick_check")
        .and_then(|mut statement| statement.query_map([], |row| row.get(0))?.collect())
        .map_err(|_| CompatibilityFailureKind::DatabaseInvalid)?;
    if integrity != ["ok"] {
        return Err(CompatibilityFailureKind::DatabaseInvalid);
    }
    let declaration = marker(
        &transaction,
        "schema",
        CompatibilityFailureKind::DatabaseSchemaMissing,
    )?;
    let schema: u32 = declaration
        .parse()
        .map_err(|_| CompatibilityFailureKind::DatabaseSchemaInvalid)?;
    if declaration != schema.to_string() {
        return Err(CompatibilityFailureKind::DatabaseSchemaInvalid);
    }
    observation.declared_schema = Some(schema);
    if schema != crate::index::CURRENT_SCHEMA {
        return Err(CompatibilityFailureKind::DatabaseSchemaMismatch);
    }
    let runtime = marker(
        &transaction,
        "runtime_store",
        CompatibilityFailureKind::RuntimeStoreMissing,
    )?;
    if runtime != RUNTIME_STORE {
        return Err(CompatibilityFailureKind::RuntimeStoreMismatch);
    }
    observation.runtime_store_matches = true;
    crate::index::verify_durable_tables(&transaction, schema)
        .map_err(|_| CompatibilityFailureKind::DurableSchemaInvalid)?;
    Ok(())
}

fn marker(
    connection: &Connection,
    key: &str,
    missing: CompatibilityFailureKind,
) -> Result<String, CompatibilityFailureKind> {
    let values: Vec<String> = connection
        .prepare("SELECT value FROM meta WHERE key=?1 LIMIT 2")
        .and_then(|mut statement| statement.query_map([key], |row| row.get(0))?.collect())
        .map_err(|_| CompatibilityFailureKind::DatabaseInvalid)?;
    match values.as_slice() {
        [value] => Ok(value.clone()),
        [] => Err(missing),
        _ => Err(CompatibilityFailureKind::DatabaseInvalid),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn release_metadata_matches_the_database_implementation() {
        let metadata = super::compiled_metadata();
        assert_eq!(metadata.database_schema, crate::index::CURRENT_SCHEMA);
        assert_eq!(metadata.runtime_store, super::RUNTIME_STORE);
        assert_eq!(metadata.persistent_compatibility_epoch, 1);
    }

    use super::*;
    use crate::vault::Vault;
    use tempfile::TempDir;

    fn tempdir() -> TempDir {
        // macOSの/var aliasをfixtureへ持ち込まず、製品と同じ通常path境界を通す。
        tempfile::Builder::new()
            .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
            .unwrap()
    }

    fn setup() -> (TempDir, PathBuf, Vault) {
        let directory = tempdir();
        let vault = Vault::create(directory.path().join("日本語 vault ?#%")).unwrap();
        drop(crate::index::open_db(&vault).unwrap());
        let registry = directory.path().join("registry.json");
        write_registry(&registry, &[("first", &vault.root)]);
        (directory, registry, vault)
    }

    fn write_registry(path: &Path, entries: &[(&str, &Path)]) {
        let entries: Vec<_> = entries
            .iter()
            .map(|(name, path)| serde_json::json!({"name":name,"path":path}))
            .collect();
        fs::write(
            path,
            serde_json::to_vec(&serde_json::json!({"vaults":entries})).unwrap(),
        )
        .unwrap();
    }

    fn inspect_fixture(registry: &Path) -> CompatibilityReport {
        inspect_with_sources(registry, None, &compiled_metadata())
    }

    fn mutate(vault: &Vault, sql: &str) {
        Connection::open(vault.index_db_path())
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }

    #[test]
    fn current_database_and_closed_wal_mode_are_observed_without_creating_sidecars() {
        let (_directory, registry, vault) = setup();
        let before = observe_database(&vault.index_db_path()).unwrap();
        assert!(before.wal.is_none());
        let report = inspect_fixture(&registry);
        assert!(report.checks_passed, "{report:?}");
        assert_eq!(
            report.vaults[0].declared_schema,
            Some(compiled_metadata().database_schema)
        );
        assert_eq!(observe_database(&vault.index_db_path()).unwrap(), before);
        assert!(
            !vault
                .index_db_path()
                .with_file_name("index.db-shm")
                .exists()
        );
    }

    /// 2026-09-09: 接続直後だけでなくdrop後も、checkpoint済みのWALを残す必要がある。
    #[test]
    fn checkpointed_persistent_wal_is_unchanged_after_last_reader_closes() {
        let (_directory, registry, vault) = setup();
        let writer = Connection::open(vault.index_db_path()).unwrap();
        writer
            .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
            .unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; INSERT INTO meta VALUES('synthetic','retained'); PRAGMA wal_checkpoint(FULL);").unwrap();
        drop(writer);
        let before = observe_database(&vault.index_db_path()).unwrap();
        assert!(before.wal.as_ref().unwrap().length > 0);
        let report = inspect_fixture(&registry);
        assert!(report.checks_passed, "{report:?}");
        assert_eq!(observe_database(&vault.index_db_path()).unwrap(), before);
    }

    #[test]
    fn committed_wal_schema_is_read_instead_of_stale_database_body() {
        let (_directory, registry, vault) = setup();
        let writer = Connection::open(vault.index_db_path()).unwrap();
        writer
            .execute_batch(
                "PRAGMA wal_autocheckpoint=0; UPDATE meta SET value='15' WHERE key='schema';",
            )
            .unwrap();
        let before = observe_database(&vault.index_db_path()).unwrap();
        assert!(before.wal.as_ref().unwrap().length > 0);
        let report = inspect_fixture(&registry);
        assert!(!report.checks_passed);
        assert_eq!(report.vaults[0].declared_schema, Some(15));
        assert_eq!(
            report.vaults[0].blockers,
            [CompatibilityFailureKind::DatabaseSchemaMismatch]
        );
        assert_eq!(observe_database(&vault.index_db_path()).unwrap(), before);
        drop(writer);
    }

    #[test]
    fn package_metadata_is_required_and_must_match_every_compiled_dimension() {
        let directory = tempdir();
        let missing_registry = directory.path().join("missing/registry.json");
        let mut cases = [
            compiled_metadata(),
            compiled_metadata(),
            compiled_metadata(),
        ];
        cases[0].runtime_store = "unknown".into();
        cases[1].database_schema += 1;
        cases[2].persistent_compatibility_epoch += 1;
        for metadata in cases {
            let report = inspect_with_sources(&missing_registry, None, &metadata);
            assert!(!report.checks_passed && !report.package_metadata_matches);
            assert_eq!(report.blockers.len(), 1);
        }
        for missing in [
            "runtime_store",
            "database_schema",
            "persistent_compatibility_epoch",
        ] {
            let mut value = serde_json::to_value(compiled_metadata()).unwrap();
            value.as_object_mut().unwrap().remove(missing);
            assert!(serde_json::from_value::<PackageCompatibilityMetadata>(value).is_err());
        }
        assert!(!missing_registry.parent().unwrap().exists());
    }

    #[test]
    fn old_future_missing_and_invalid_schema_are_blocked_without_migration() {
        for (sql, expected) in [
            (
                "UPDATE meta SET value='12' WHERE key='schema'",
                CompatibilityFailureKind::DatabaseSchemaMismatch,
            ),
            (
                "UPDATE meta SET value='15' WHERE key='schema'",
                CompatibilityFailureKind::DatabaseSchemaMismatch,
            ),
            (
                "DELETE FROM meta WHERE key='schema'",
                CompatibilityFailureKind::DatabaseSchemaMissing,
            ),
            (
                "UPDATE meta SET value='unrecognized' WHERE key='schema'",
                CompatibilityFailureKind::DatabaseSchemaInvalid,
            ),
            (
                "UPDATE meta SET value='013' WHERE key='schema'",
                CompatibilityFailureKind::DatabaseSchemaInvalid,
            ),
            (
                "UPDATE meta SET value='13 ' WHERE key='schema'",
                CompatibilityFailureKind::DatabaseSchemaInvalid,
            ),
            (
                "DELETE FROM meta WHERE key='runtime_store'",
                CompatibilityFailureKind::RuntimeStoreMissing,
            ),
            (
                "UPDATE meta SET value='future' WHERE key='runtime_store'",
                CompatibilityFailureKind::RuntimeStoreMismatch,
            ),
            (
                "DROP TABLE action_receipts",
                CompatibilityFailureKind::DurableSchemaInvalid,
            ),
        ] {
            let (_directory, registry, vault) = setup();
            mutate(&vault, sql);
            let before = observe_database(&vault.index_db_path()).unwrap();
            let report = inspect_fixture(&registry);
            assert!(!report.checks_passed);
            assert_eq!(report.vaults[0].blockers, [expected], "{report:?}");
            assert_eq!(observe_database(&vault.index_db_path()).unwrap(), before);
        }
    }

    #[test]
    fn missing_and_corrupt_database_are_never_recreated() {
        let (_directory, registry, vault) = setup();
        fs::remove_file(vault.index_db_path()).unwrap();
        let report = inspect_fixture(&registry);
        assert_eq!(
            report.vaults[0].blockers,
            [CompatibilityFailureKind::DatabaseMissing]
        );
        assert!(!vault.index_db_path().exists());
        fs::write(vault.index_db_path(), b"synthetic not sqlite").unwrap();
        let before = observe_database(&vault.index_db_path()).unwrap();
        let report = inspect_fixture(&registry);
        assert_eq!(
            report.vaults[0].blockers,
            [CompatibilityFailureKind::DatabaseInvalid]
        );
        assert_eq!(observe_database(&vault.index_db_path()).unwrap(), before);
    }

    #[test]
    fn registry_missing_is_empty_but_invalid_and_unreadable_are_blocked() {
        let directory = tempdir();
        let registry = directory.path().join("registry.json");
        assert!(inspect_fixture(&registry).checks_passed);
        assert!(!registry.exists());
        for bytes in [b"{".as_slice(), b"{}", br#"{"vaultz":[]}"#] {
            fs::write(&registry, bytes).unwrap();
            assert_eq!(
                inspect_fixture(&registry).blockers,
                [CompatibilityFailureKind::RegistryInvalid]
            );
            assert_eq!(fs::read(&registry).unwrap(), bytes);
        }
        assert_eq!(
            inspect_fixture(&registry.join("unreadable.json")).blockers,
            [CompatibilityFailureKind::RegistryReadFailed]
        );
        assert_eq!(
            inspect_fixture(directory.path()).blockers,
            [CompatibilityFailureKind::RegistryReadFailed]
        );
    }

    #[test]
    fn every_registration_and_explicit_vault_is_checked_with_alias_deduplication() {
        let (directory, registry, first) = setup();
        let second = Vault::create(directory.path().join("second")).unwrap();
        drop(crate::index::open_db(&second).unwrap());
        mutate(&second, "UPDATE meta SET value='15' WHERE key='schema'");
        write_registry(
            &registry,
            &[
                ("first", &first.root),
                ("alias", &first.root),
                ("second", &second.root),
            ],
        );
        let report = inspect_with_sources(
            &registry,
            Some(first.root.as_os_str()),
            &compiled_metadata(),
        );
        assert!(!report.checks_passed);
        assert_eq!(report.registry_entries, Some(3));
        assert_eq!(report.vaults.len(), 2);
        assert!(
            report
                .vaults
                .iter()
                .any(|vault| vault.registered_names.len() == 2
                    && vault.explicitly_selected
                    && vault.blockers.is_empty())
        );
        write_registry(&registry, &[("first", &first.root)]);
        let report = inspect_with_sources(
            &registry,
            Some(second.root.as_os_str()),
            &compiled_metadata(),
        );
        assert!(!report.checks_passed && report.vaults.len() == 2);
        let report = inspect_with_sources(&registry, Some(OsStr::new("")), &compiled_metadata());
        assert_eq!(
            report.blockers,
            [CompatibilityFailureKind::ExplicitVaultInvalid]
        );
        let report = inspect_with_sources(
            &registry,
            Some(OsStr::new("relative")),
            &compiled_metadata(),
        );
        assert!(
            report
                .vaults
                .iter()
                .any(|vault| vault.blockers == [CompatibilityFailureKind::VaultPathInvalid])
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_registry_and_database_are_not_followed() {
        use std::os::unix::fs::symlink;
        let (directory, registry, vault) = setup();
        let alias = directory.path().join("registry-alias.json");
        symlink(&registry, &alias).unwrap();
        assert_eq!(
            inspect_fixture(&alias).blockers,
            [CompatibilityFailureKind::RegistryReadFailed]
        );
        let original = vault.root.join("original.db");
        fs::rename(vault.index_db_path(), &original).unwrap();
        let before = fs::read(&original).unwrap();
        symlink(&original, vault.index_db_path()).unwrap();
        assert_eq!(
            inspect_fixture(&registry).vaults[0].blockers,
            [CompatibilityFailureKind::DatabaseReadFailed]
        );
        assert_eq!(fs::read(original).unwrap(), before);
    }
}
