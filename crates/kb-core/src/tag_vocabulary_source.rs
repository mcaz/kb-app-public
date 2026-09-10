//! 語彙正本のworkspace/UID固定と、その復元用JSONのdurable outbox。
//!
//! 題名の変更で正本を切り替えない。DBへの指定と出力待ちを同時に確定し、
//! exportが失敗しても指定を失わず、バックアップが参照中の旧ノートも保護する。

use std::collections::BTreeSet;
use std::fs;
use std::io::{ErrorKind, Write as _};
use std::str::FromStr;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::authority::NoteUid;
use crate::frontmatter::Note;
use crate::vault::Vault;
use crate::write_rejection::WriteRejection;

pub const SOURCE_FILE: &str = ".kb-tag-vocabulary.json";
pub const SOURCE_SCHEMA: &str = "kb-app.tag-vocabulary-source/v1";
pub(crate) const TABLES: [&str; 2] = ["tag_vocabulary_sources", "tag_vocabulary_source_exports"];
pub(crate) const SCHEMA_SQL: &str = "
CREATE TABLE tag_vocabulary_sources(
    singleton INTEGER PRIMARY KEY CHECK(singleton=1), document TEXT NOT NULL
);
CREATE TABLE tag_vocabulary_source_exports(
    singleton INTEGER PRIMARY KEY CHECK(singleton=1), op_id TEXT NOT NULL UNIQUE,
    base_document TEXT, document TEXT NOT NULL, log_entry TEXT NOT NULL,
    commit_message TEXT NOT NULL
);";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceBinding {
    pub schema: String,
    pub workspace_id: String,
    pub note_uid: NoteUid,
    pub revision: String,
}

impl SourceBinding {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == SOURCE_SCHEMA,
            "語彙正本の保存形式に対応していない"
        );
        ensure!(
            crate::artifact::is_ulid(&self.workspace_id),
            "語彙正本のworkspace IDが不正"
        );
        ensure!(
            crate::artifact::is_ulid(&self.revision),
            "語彙正本のrevisionが不正"
        );
        Ok(())
    }

    fn parse(document: &str) -> Result<Self> {
        let binding: Self =
            serde_json::from_str(document).context("語彙正本の指定JSONを読めない")?;
        binding.validate()?;
        Ok(binding)
    }
}

#[derive(Debug, Serialize)]
pub struct SetSourceResult {
    pub binding: SourceBinding,
    pub stored: bool,
    pub export_pending: bool,
}

struct PendingExport {
    op_id: String,
    base_document: Option<String>,
    document: String,
    log_entry: String,
    commit_message: String,
}

/// 参照先の欠損と、指定そのものの欠損を混同しない。存在検査は呼出側の状態表示で行う。
pub fn read_binding(conn: &Connection) -> Result<Option<SourceBinding>> {
    let document: Option<String> = conn
        .query_row(
            "SELECT document FROM tag_vocabulary_sources WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    document.as_deref().map(SourceBinding::parse).transpose()
}

/// DBを別保管庫へ取り違えても、正本のUIDが偶然存在するだけでは使わせない。
pub(crate) fn ensure_workspace(vault: &Vault, conn: &Connection) -> Result<()> {
    if let Some(binding) = read_binding(conn)?
        && binding.workspace_id != crate::workspace::stored_workspace_id(vault)?
    {
        return Err(
            WriteRejection::TagVocabulary.validation("DBの語彙正本と接続先workspaceが一致しない")
        );
    }
    Ok(())
}

fn pending(conn: &Connection) -> Result<Option<PendingExport>> {
    let result = conn.query_row(
        "SELECT op_id,base_document,document,log_entry,commit_message FROM tag_vocabulary_source_exports WHERE singleton=1",
        [], |row| Ok(PendingExport {
            op_id: row.get(0)?, base_document: row.get(1)?, document: row.get(2)?,
            log_entry: row.get(3)?, commit_message: row.get(4)?,
        }),
    ).optional()?;
    if let Some(export) = &result {
        let next = SourceBinding::parse(&export.document)?;
        ensure!(
            export.op_id == next.revision,
            "語彙正本のoutbox revisionが一致しない"
        );
        ensure!(
            read_binding(conn)?.as_ref() == Some(&next),
            "語彙正本のoutboxとDB指定が一致しない"
        );
        if let Some(base) = export.base_document.as_deref() {
            let previous = SourceBinding::parse(base)?;
            ensure!(
                previous.workspace_id == next.workspace_id,
                "語彙正本のoutbox workspaceが一致しない"
            );
        }
        ensure!(
            !export.log_entry.is_empty() && !export.commit_message.is_empty(),
            "語彙正本の出力記録がない"
        );
    }
    Ok(result)
}

pub(crate) fn pending_count(conn: &Connection) -> Result<usize> {
    Ok(usize::from(pending(conn)?.is_some()))
}

/// 新旧の出力がどちらも存在し得る間は、両方の参照先を保護する。
pub fn protected_note_uids(conn: &Connection) -> Result<Vec<NoteUid>> {
    let mut uids = Vec::new();
    if let Some(binding) = read_binding(conn)? {
        uids.push(binding.note_uid);
    }
    if let Some(base) = pending(conn)?.and_then(|export| export.base_document) {
        let uid = SourceBinding::parse(&base)?.note_uid;
        if !uids.contains(&uid) {
            uids.push(uid);
        }
    }
    Ok(uids)
}

fn note_is_eligible(note: &Note) -> bool {
    note.front.note_uid.is_some()
        && note.front.authority.is_some()
        && note.front.effective_status() != crate::frontmatter::STATUS_DEPRECATED
        && crate::authority::validate_envelope(
            note.front.note_uid.as_ref(),
            note.front.authority.as_ref(),
            &note.front.relations,
        )
        .is_ok()
        && crate::proposal_workflow::derive_normal_reference_allowed(note)
}

pub fn guard_source_note_write(conn: &Connection, note: &Note) -> Result<()> {
    let protected = protected_note_uids(conn)?;
    if note
        .front
        .note_uid
        .as_ref()
        .is_some_and(|uid| protected.contains(uid))
    {
        ensure_source_eligible(note)?;
    }
    Ok(())
}

pub fn guard_source_note_removal(conn: &Connection, note: &Note) -> Result<()> {
    let protected = protected_note_uids(conn)?;
    if note
        .front
        .note_uid
        .as_ref()
        .is_some_and(|uid| protected.contains(uid))
    {
        return Err(WriteRejection::TagVocabulary.validation(
            "語彙正本または未出力の旧正本は削除できない。正本を先に付け替えて出力を完了する",
        ));
    }
    Ok(())
}

/// 破損したdocumentのUIDだけを信じると、DB列に固定された正本を削除できてしまう。
pub fn guard_source_note_id_removal(conn: &Connection, raw: &str) -> Result<()> {
    let id = crate::note_id::NoteId::parse(raw)?;
    let indexed_uid: Option<String> = conn
        .query_row(
            "SELECT note_uid FROM notes WHERE id=?1",
            [id.as_str()],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    let protected = protected_note_uids(conn)?;
    if indexed_uid
        .as_deref()
        .is_some_and(|uid| protected.iter().any(|bound| bound.as_str() == uid))
    {
        return Err(WriteRejection::TagVocabulary.validation("DB列が語彙正本または未出力の旧正本を指しているため削除できない。正本を先に付け替えて出力を完了する"));
    }
    guard_source_note_removal(conn, &crate::note_store::read(conn, id.as_str())?)
}

pub(crate) fn ensure_source_eligible(note: &Note) -> Result<()> {
    if !note_is_eligible(note) {
        return Err(WriteRejection::TagVocabulary.validation(
            "語彙正本には有効なUID/authorityを持ち、通常参照できる非deprecatedノートが必要",
        ));
    }
    Ok(())
}

fn target_note(conn: &Connection, uid: &NoteUid) -> Result<Note> {
    let row: Option<(String, bool)> = conn
        .query_row(
            "SELECT document,normal_reference_allowed=1 FROM notes WHERE note_uid=?1",
            [uid.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (document, allowed) = row.ok_or_else(|| {
        WriteRejection::TagVocabulary.validation("指定する語彙正本のnote_uidが存在しない")
    })?;
    let note = Note::parse(&document)?;
    ensure_source_eligible(&note)?;
    if note.front.note_uid.as_ref() != Some(uid) || !allowed {
        return Err(WriteRejection::TagVocabulary
            .validation("語彙正本のDB索引と原文が一致しないか通常参照できない"));
    }
    Ok(note)
}

pub fn set_source(
    vault: &Vault,
    conn: &Connection,
    workspace_id: &str,
    note_uid: &str,
    expected_revision: Option<&str>,
    reason: &str,
    client: &str,
) -> Result<SetSourceResult> {
    let binding = (|| -> Result<SourceBinding> {
        if !crate::artifact::is_ulid(workspace_id)
            || expected_revision.is_some_and(|value| !crate::artifact::is_ulid(value))
            || reason.trim().is_empty() || reason.chars().count() > 2000 || reason.chars().any(char::is_control)
            || client.trim().is_empty() || client.chars().count() > 200 || client.chars().any(char::is_control) {
            return Err(WriteRejection::InvalidArgument.validation("語彙正本の指定引数が不正"));
        }
        let uid = NoteUid::from_str(note_uid).map_err(|_| WriteRejection::InvalidArgument.validation("note_uidは26文字のULIDにする"))?;
        if crate::workspace::stored_workspace_id(vault)? != workspace_id {
            return Err(WriteRejection::InvalidArgument.validation("接続先と指定したworkspace IDが一致しない"));
        }
        let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        let current = read_binding(&tx)?;
        if current.as_ref().map(|binding| binding.revision.as_str()) != expected_revision {
            return Err(WriteRejection::TagVocabulary.validation("語彙正本のrevisionが変わった。tag_vocabularyを再取得して判断し直す"));
        }
        if current.as_ref().is_some_and(|binding| binding.workspace_id != workspace_id) {
            return Err(WriteRejection::TagVocabulary.validation("DBの語彙正本が別workspaceを指している"));
        }
        if pending_count(&tx)? != 0 {
            return Err(WriteRejection::TagVocabulary.validation("語彙正本の前回指定は保存済みだが未出力。出力を完了してから再指定する"));
        }
        let target = target_note(&tx, &uid)?;
        let previous = crate::tags::registered_vocabulary(&tx)?;
        let next = crate::tags::parse_glossary(note_uid.into(), &target.body)
            .entries.into_keys().collect();
        crate::tags::guard_vocabulary_removal(&tx, &previous, &next, None)?;
        let base_document = read_export_document(vault)?;
        let exported = base_document.as_deref().map(SourceBinding::parse).transpose()?;
        if exported != current {
            return Err(WriteRejection::TagVocabulary.validation("語彙正本の復元用JSONとDB指定が一致しない。外部変更を上書きしない"));
        }
        let binding = SourceBinding {
            schema: SOURCE_SCHEMA.into(), workspace_id: workspace_id.into(), note_uid: uid,
            revision: NoteUid::new().to_string(),
        };
        let document = serde_json::to_string_pretty(&binding)? + "\n";
        tx.execute("INSERT INTO tag_vocabulary_sources(singleton,document) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET document=excluded.document", [&document])?;
        tx.execute("INSERT INTO tag_vocabulary_source_exports(singleton,op_id,base_document,document,log_entry,commit_message) VALUES(1,?1,?2,?3,?4,?5)",
            params![binding.revision, base_document, document,
                format!("**Tag vocabulary source**: {} / {} (via {client})。{reason}", binding.note_uid, binding.revision),
                format!("tags: 正本を{}へ指定 (via {client})", binding.note_uid)])?;
        tx.commit()?;
        Ok(binding)
    })().map_err(crate::write_rejection::confirm_before_write)?;
    // 保存後の出力失敗を未保存として返すと、別revisionで同じ指定を再送してしまう。
    let export_pending = vault.flush_note_exports(conn).is_err();
    Ok(SetSourceResult {
        binding,
        stored: true,
        export_pending,
    })
}

fn read_regular_optional(vault: &Vault, filename: &str) -> Result<Option<String>> {
    let path = vault.root.join(filename);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            ensure!(
                metadata.file_type().is_file(),
                "語彙正本の出力先は通常ファイルにする"
            );
            Ok(Some(fs::read_to_string(path)?))
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn read_export_document(vault: &Vault) -> Result<Option<String>> {
    let document = read_regular_optional(vault, SOURCE_FILE)?;
    if document.is_none() {
        let repository = git2::Repository::open(&vault.root)?;
        ensure!(
            repository
                .index()?
                .get_path(std::path::Path::new(SOURCE_FILE), 0)
                .is_none(),
            "追跡済みの語彙正本JSONが欠けている。未指定として扱わない"
        );
    }
    if let Some(document) = &document {
        let binding = SourceBinding::parse(document)?;
        ensure!(
            binding.workspace_id == crate::workspace::stored_workspace_id(vault)?,
            "復元用の語彙正本workspaceが一致しない"
        );
    }
    Ok(document)
}

pub(crate) fn exported_binding(vault: &Vault) -> Result<Option<SourceBinding>> {
    read_export_document(vault)?
        .as_deref()
        .map(SourceBinding::parse)
        .transpose()
}

fn atomic_write(vault: &Vault, filename: &str, document: &str) -> Result<()> {
    // symlink・directoryを置換して問題を隠さず、入力検査と書込直前の両方で拒否する。
    read_regular_optional(vault, filename)?;
    let mut temporary = tempfile::NamedTempFile::new_in(&vault.root)?;
    temporary.write_all(document.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(vault.root.join(filename))
        .map_err(|error| error.error)?;
    Ok(())
}

/// ノートのexport後に呼ぶ。参照先のMarkdownを確定する前に正本指定だけをpushしない。
pub(crate) fn flush_exports(vault: &Vault, conn: &Connection) -> Result<usize> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let Some(export) = pending(&tx)? else {
        tx.commit()?;
        return Ok(0);
    };
    ensure!(
        crate::note_store::pending_count(&tx)? == 0,
        "ノートの出力を先に完了する"
    );
    let binding = SourceBinding::parse(&export.document)?;
    ensure!(
        binding.workspace_id == crate::workspace::stored_workspace_id(vault)?,
        "語彙正本の出力先workspaceが一致しない"
    );
    let current = read_export_document(vault)?;
    ensure!(
        current == export.base_document || current.as_deref() == Some(export.document.as_str()),
        "語彙正本の復元用JSONが外部変更されたため上書きしない"
    );
    atomic_write(vault, SOURCE_FILE, &export.document)?;
    let marker = format!("<!-- kb-export:{} -->", export.op_id);
    let mut log = read_regular_optional(vault, "log.md")?.unwrap_or_default();
    if !log.contains(&marker) {
        log.push_str(&format!("\n{} {marker}\n", export.log_entry));
        atomic_write(vault, "log.md", &log)?;
    }
    vault.commit(&[SOURCE_FILE, "log.md"], &export.commit_message)?;
    tx.execute(
        "DELETE FROM tag_vocabulary_source_exports WHERE singleton=1 AND op_id=?1",
        [&export.op_id],
    )?;
    tx.commit()?;
    Ok(1)
}

/// notesと同じimport transactionの最初に適用し、全note取込後に参照の整合を検査する。
pub(crate) fn import_binding(
    conn: &Connection,
    document: Option<&str>,
) -> Result<ImportedVocabulary> {
    ensure!(
        !conn.is_autocommit(),
        "語彙正本のimportにはtransactionが必要"
    );
    ensure!(
        pending_count(conn)? == 0,
        "未出力の語彙正本をimportで上書きしない"
    );
    let previous = crate::tags::registered_vocabulary(conn)?;
    match document {
        Some(document) => {
            SourceBinding::parse(document)?;
            conn.execute("INSERT INTO tag_vocabulary_sources(singleton,document) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET document=excluded.document", [document])?;
        }
        None => ensure!(
            read_binding(conn)?.is_none(),
            "指定済み語彙正本の復元用JSONが欠けている。指定を解除しない"
        ),
    }
    Ok(ImportedVocabulary { previous })
}

pub(crate) struct ImportedVocabulary {
    previous: BTreeSet<String>,
}

pub(crate) fn validate_imported_binding(
    conn: &Connection,
    imported: ImportedVocabulary,
) -> Result<()> {
    if let Some(binding) = read_binding(conn)? {
        target_note(conn, &binding.note_uid)?;
    }
    let next = crate::tags::registered_vocabulary(conn)?;
    crate::tags::guard_vocabulary_removal(conn, &imported.previous, &next, None)
}

pub(crate) fn verify_schema(conn: &Connection) -> Result<()> {
    for (table, columns) in [
        (
            TABLES[0],
            &[("singleton", "INTEGER"), ("document", "TEXT")][..],
        ),
        (
            TABLES[1],
            &[
                ("singleton", "INTEGER"),
                ("op_id", "TEXT"),
                ("base_document", "TEXT"),
                ("document", "TEXT"),
                ("log_entry", "TEXT"),
                ("commit_message", "TEXT"),
            ][..],
        ),
    ] {
        let normalize = |sql: &str| {
            sql.chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
                .to_ascii_lowercase()
        };
        let expected = SCHEMA_SQL
            .split(';')
            .find(|sql| {
                sql.trim_start()
                    .starts_with(&format!("CREATE TABLE {table}("))
            })
            .context("語彙正本のDDL定義がない")?;
        let actual_sql: String = conn.query_row(
            "SELECT sql FROM sqlite_schema WHERE type='table' AND name=?1",
            [table],
            |row| row.get(0),
        )?;
        ensure!(
            normalize(&actual_sql) == normalize(expected),
            "語彙正本のdurable table制約が不正: {table}"
        );
        let actual: Vec<(String, String)> = conn
            .prepare(&format!("PRAGMA table_info({table})"))?
            .query_map([], |row| Ok((row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        ensure!(
            actual
                .iter()
                .map(|(name, kind)| (name.as_str(), kind.as_str()))
                .eq(columns.iter().copied()),
            "語彙正本のdurable table定義が不正: {table}"
        );
        let invalid: i64 = conn.query_row(
            &format!("SELECT count(*) FROM {table} WHERE singleton != 1 OR singleton IS NULL"),
            [],
            |row| row.get(0),
        )?;
        ensure!(invalid == 0, "語彙正本のdurable row識別が不正");
        let count: i64 = conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })?;
        ensure!(count <= 1, "語彙正本のdurable rowが重複している");
    }
    read_binding(conn)?;
    pending(conn)?;
    Ok(())
}

pub(crate) fn initialize_recovery_schema(conn: &Connection) -> Result<()> {
    let tables: i64 = conn.query_row("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('tag_vocabulary_sources','tag_vocabulary_source_exports')", [], |row| row.get(0))?;
    if tables == 0 {
        conn.execute_batch(SCHEMA_SQL)?;
    }
    ensure!(tables != 1, "語彙正本のdurable tableが片方だけ欠けている");
    verify_schema(conn)
}

#[cfg(test)]
pub(crate) fn pin_for_test(
    vault: &Vault,
    conn: &Connection,
    note_id: &str,
) -> Result<SetSourceResult> {
    let uid = crate::note_store::read(conn, note_id)?
        .front
        .note_uid
        .context("テスト正本にUIDがない")?;
    let current = read_binding(conn)?;
    set_source(
        vault,
        conn,
        &crate::workspace::stored_workspace_id(vault)?,
        uid.as_str(),
        current.as_ref().map(|binding| binding.revision.as_str()),
        "テスト語彙の正本指定",
        "test/client",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Vault, Connection, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        let a = vault
            .propose_for_test(
                "vocabulary alpha",
                "## 語彙\n| タグ | 説明 |\n| --- | --- |\n| test | テスト |\n",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let b = vault
            .propose_for_test(
                "vocabulary beta",
                "## 語彙\n| タグ | 説明 |\n| --- | --- |\n| test | テスト |\n",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        (dir, vault, conn, a, b)
    }

    fn request(
        vault: &Vault,
        conn: &Connection,
        note: &str,
        expected: Option<&str>,
    ) -> Result<SetSourceResult> {
        let uid = crate::note_store::read(conn, note)?.front.note_uid.unwrap();
        set_source(
            vault,
            conn,
            &crate::workspace::stored_workspace_id(vault)?,
            uid.as_str(),
            expected,
            "既存語彙を一意にする",
            "test/client",
        )
    }

    #[test]
    fn source_binding_uses_cas_and_a_new_revision_for_every_change() {
        let (_dir, vault, conn, a, b) = setup();
        assert!(read_binding(&conn).unwrap().is_none());
        let first = request(&vault, &conn, &a, None).unwrap();
        assert!(first.stored);
        assert!(!first.export_pending);
        assert_eq!(
            exported_binding(&vault).unwrap(),
            Some(first.binding.clone())
        );
        assert!(request(&vault, &conn, &b, None).is_err());
        let second = request(&vault, &conn, &b, Some(&first.binding.revision)).unwrap();
        let third = request(&vault, &conn, &a, Some(&second.binding.revision)).unwrap();
        assert_ne!(first.binding.revision, third.binding.revision);
        assert!(request(&vault, &conn, &b, Some(&first.binding.revision)).is_err());
        assert_eq!(read_binding(&conn).unwrap(), Some(third.binding));
    }

    /// 2026-09-08: 保存済みを未保存と返さず、旧exportの参照先も出力完了まで保護する。
    #[test]
    fn failed_export_keeps_new_binding_and_protects_previous_source_until_retry() {
        let (_dir, vault, conn, a, b) = setup();
        let first = request(&vault, &conn, &a, None).unwrap();
        fs::write(vault.root.join(".git/index.lock"), "locked").unwrap();
        let saved = request(&vault, &conn, &b, Some(&first.binding.revision)).unwrap();
        assert!(saved.stored && saved.export_pending);
        assert_eq!(read_binding(&conn).unwrap(), Some(saved.binding.clone()));
        assert_eq!(protected_note_uids(&conn).unwrap().len(), 2);
        assert!(
            guard_source_note_removal(&conn, &crate::note_store::read(&conn, &a).unwrap()).is_err()
        );
        assert!(request(&vault, &conn, &a, Some(&saved.binding.revision)).is_err());
        fs::remove_file(vault.root.join(".git/index.lock")).unwrap();
        assert_eq!(vault.flush_note_exports(&conn).unwrap(), 1);
        assert_eq!(vault.flush_note_exports(&conn).unwrap(), 0);
        assert_eq!(
            protected_note_uids(&conn).unwrap(),
            vec![saved.binding.note_uid]
        );
        assert!(
            guard_source_note_removal(&conn, &crate::note_store::read(&conn, &a).unwrap()).is_ok()
        );
    }

    #[test]
    fn wrong_workspace_and_ineligible_targets_do_not_change_binding_or_export() {
        let (_dir, vault, conn, a, _) = setup();
        let uid = crate::note_store::read(&conn, &a)
            .unwrap()
            .front
            .note_uid
            .unwrap();
        assert!(
            set_source(
                &vault,
                &conn,
                &NoteUid::new().to_string(),
                uid.as_str(),
                None,
                "指定",
                "test/client"
            )
            .is_err()
        );
        conn.execute(
            "UPDATE notes SET normal_reference_allowed=0 WHERE id=?1",
            [&a],
        )
        .unwrap();
        assert!(request(&vault, &conn, &a, None).is_err());
        assert!(read_binding(&conn).unwrap().is_none());
        assert_eq!(pending_count(&conn).unwrap(), 0);
        assert!(!vault.root.join(SOURCE_FILE).exists());
    }

    #[test]
    fn bound_source_cannot_be_hidden_or_deleted_but_title_and_body_can_change() {
        let (_dir, vault, conn, a, _) = setup();
        request(&vault, &conn, &a, None).unwrap();
        let mut source = crate::note_store::read(&conn, &a).unwrap();
        source.front.title = Some("改名後の正本".into());
        source.body = "## 語彙\n".into();
        guard_source_note_write(&conn, &source).unwrap();
        source.front.status = Some("deprecated".into());
        assert!(guard_source_note_write(&conn, &source).is_err());
        assert!(guard_source_note_removal(&conn, &source).is_err());
    }

    #[test]
    fn missing_target_does_not_discard_stored_binding() {
        let (_dir, vault, conn, a, b) = setup();
        let first = request(&vault, &conn, &a, None).unwrap();
        conn.execute("DELETE FROM notes WHERE id=?1", [&a]).unwrap();
        assert_eq!(read_binding(&conn).unwrap(), Some(first.binding.clone()));
        verify_schema(&conn).unwrap();
        let repaired = request(&vault, &conn, &b, Some(&first.binding.revision)).unwrap();
        assert!(!repaired.export_pending);
    }

    /// 2026-09-08: 本文更新をせず別の正本へ切り替えても、使用中の語彙を削除できない。
    #[test]
    fn source_switch_cannot_remove_a_used_word() {
        let (_dir, vault, conn, a, b) = setup();
        let first = request(&vault, &conn, &a, None).unwrap().binding;
        let mut alternate = crate::note_store::read(&conn, &b).unwrap();
        alternate.body = "## 語彙\n| replacement | 別の語彙 |\n".into();
        crate::note_store::put(
            &vault,
            &conn,
            &b,
            &alternate,
            crate::note_store::WriteAttribution::new(
                "fixture",
                "test: fixture",
                &crate::provenance::test_context(),
            ),
        )
        .unwrap();
        vault.flush_note_exports(&conn).unwrap();
        let before = crate::index::test_support::logical_snapshot(&vault.index_db_path());
        let error = request(&vault, &conn, &b, Some(&first.revision)).unwrap_err();
        assert!(
            error.to_string().contains("使用中の語彙「test」"),
            "{error}"
        );
        assert_eq!(
            before,
            crate::index::test_support::logical_snapshot(&vault.index_db_path())
        );
        assert_eq!(read_binding(&conn).unwrap(), Some(first));
        assert_eq!(pending_count(&conn).unwrap(), 0);
    }

    /// 2026-09-08: importした正本指定で旧語彙を隠しても、取込前の使用語を失わせない。
    #[test]
    fn imported_source_switch_preserves_the_previous_vocabulary_boundary() {
        let (_dir, vault, conn, a, b) = setup();
        let first = request(&vault, &conn, &a, None).unwrap().binding;
        let before = crate::index::test_support::logical_snapshot(&vault.index_db_path());
        let mut alternate = crate::note_store::read(&conn, &b).unwrap();
        alternate.body = "## 語彙\n| replacement | 別の語彙 |\n".into();
        vault.write_note_fixture(&b, &alternate).unwrap();
        let mut next = first.clone();
        next.note_uid = alternate.front.note_uid.unwrap();
        next.revision = NoteUid::new().to_string();
        fs::write(
            vault.root.join(SOURCE_FILE),
            serde_json::to_string(&next).unwrap(),
        )
        .unwrap();
        let error = crate::index::import_markdown_snapshot(&vault, &conn).unwrap_err();
        assert!(
            error.to_string().contains("使用中の語彙「test」"),
            "{error}"
        );
        assert_eq!(
            before,
            crate::index::test_support::logical_snapshot(&vault.index_db_path())
        );
        assert_eq!(read_binding(&conn).unwrap(), Some(first));
    }

    #[test]
    fn source_definition_and_pending_contents_fail_closed() {
        let (_dir, vault, conn, a, _) = setup();
        request(&vault, &conn, &a, None).unwrap();
        conn.execute("UPDATE tag_vocabulary_sources SET document='{}'", [])
            .unwrap();
        assert!(read_binding(&conn).is_err());
        assert!(protected_note_uids(&conn).is_err());
        assert!(verify_schema(&conn).is_err());
        let other = Connection::open_in_memory().unwrap();
        other.execute_batch("CREATE TABLE tag_vocabulary_sources(singleton INTEGER,document TEXT); CREATE TABLE tag_vocabulary_source_exports(singleton INTEGER,op_id TEXT,base_document TEXT,document TEXT,log_entry TEXT,commit_message TEXT)").unwrap();
        assert!(verify_schema(&other).is_err());
    }

    #[test]
    fn fresh_clone_restores_binding_and_the_same_logical_digest() {
        let (dir, vault, conn, a, _) = setup();
        let expected = request(&vault, &conn, &a, None).unwrap().binding;
        let snapshot = crate::storage_contract::export(&vault).unwrap();
        assert_eq!(snapshot.snapshot.schema, crate::storage_contract::SCHEMA_V2);
        let cloned = dir.path().join("clone");
        git2::Repository::clone(vault.root.to_str().unwrap(), &cloned).unwrap();
        let restored_vault = Vault::open(cloned).unwrap();
        let restored = crate::index::open_db(&restored_vault).unwrap();
        assert_eq!(read_binding(&restored).unwrap(), Some(expected));
        assert_eq!(
            crate::storage_contract::export(&restored_vault)
                .unwrap()
                .digest,
            snapshot.digest
        );
        drop(restored);
        let reopened = crate::index::open_db(&restored_vault).unwrap();
        assert!(read_binding(&reopened).unwrap().is_some());
    }

    #[test]
    fn import_rejects_a_missing_target_and_rolls_back_notes_and_binding_together() {
        let (_dir, vault, conn, a, b) = setup();
        let first = request(&vault, &conn, &a, None).unwrap().binding;
        let before = crate::note_store::read(&conn, &b).unwrap();
        let mut changed = before.clone();
        changed.body = "import途中で変わらない".into();
        vault.write_note_fixture(&b, &changed).unwrap();
        let mut invalid = first.clone();
        invalid.note_uid = NoteUid::new();
        invalid.revision = NoteUid::new().to_string();
        fs::write(
            vault.root.join(SOURCE_FILE),
            serde_json::to_string(&invalid).unwrap(),
        )
        .unwrap();
        assert!(crate::index::import_markdown_snapshot(&vault, &conn).is_err());
        assert_eq!(read_binding(&conn).unwrap(), Some(first));
        assert_eq!(
            crate::note_store::read(&conn, &b).unwrap().body,
            before.body
        );
    }

    #[test]
    fn unbound_repository_keeps_v1_serialization_and_old_snapshot_deserializes() {
        let (_dir, vault, _conn, _a, _b) = setup();
        let snapshot = crate::storage_contract::snapshot(&vault).unwrap();
        assert_eq!(snapshot.schema, crate::storage_contract::SCHEMA_V1);
        let serialized = serde_json::to_value(&snapshot).unwrap();
        assert!(serialized.get("tag_vocabulary_source").is_none());
        let restored: crate::storage_contract::RepositorySnapshotV1 =
            serde_json::from_value(serialized).unwrap();
        assert!(restored.tag_vocabulary_source.is_none());
    }

    #[test]
    fn outbox_insert_failure_rolls_back_the_binding_and_no_files_are_written() {
        let (_dir, vault, conn, a, _) = setup();
        conn.execute_batch("CREATE TEMP TRIGGER fail_source_export BEFORE INSERT ON tag_vocabulary_source_exports BEGIN SELECT RAISE(ABORT,'test outbox failure'); END;").unwrap();
        assert!(request(&vault, &conn, &a, None).is_err());
        assert!(read_binding(&conn).unwrap().is_none());
        assert_eq!(pending_count(&conn).unwrap(), 0);
        assert!(!vault.root.join(SOURCE_FILE).exists());
    }

    #[test]
    fn workspace_mismatch_stops_normal_and_read_only_open_without_discarding_binding() {
        let (_dir, vault, conn, a, _) = setup();
        let binding = request(&vault, &conn, &a, None).unwrap().binding;
        fs::write(
            vault.root.join(crate::workspace::ID_FILE),
            format!("{}\n", NoteUid::new()),
        )
        .unwrap();
        assert!(ensure_workspace(&vault, &conn).is_err());
        assert!(crate::index::open_db(&vault).is_err());
        assert!(crate::index::open_db_read_only(&vault).is_err());
        assert_eq!(read_binding(&conn).unwrap(), Some(binding));
    }

    #[test]
    fn control_characters_are_rejected_before_log_or_binding_changes() {
        let (_dir, vault, conn, a, _) = setup();
        let workspace = crate::workspace::stored_workspace_id(&vault).unwrap();
        let uid = crate::note_store::read(&conn, &a)
            .unwrap()
            .front
            .note_uid
            .unwrap();
        for (reason, client) in [
            ("別行\nを挿入", "test/client"),
            ("正本指定", "test\rclient"),
            ("\u{1b}escape", "test/client"),
        ] {
            assert!(
                set_source(
                    &vault,
                    &conn,
                    &workspace,
                    uid.as_str(),
                    None,
                    reason,
                    client
                )
                .is_err()
            );
        }
        assert!(read_binding(&conn).unwrap().is_none());
        assert_eq!(pending_count(&conn).unwrap(), 0);
    }

    #[test]
    fn missing_tracked_export_does_not_turn_a_bound_snapshot_into_v1() {
        let (_dir, vault, conn, a, _) = setup();
        let binding = request(&vault, &conn, &a, None).unwrap().binding;
        fs::remove_file(vault.root.join(SOURCE_FILE)).unwrap();
        assert!(crate::storage_contract::snapshot(&vault).is_err());
        assert!(crate::index::import_markdown_snapshot(&vault, &conn).is_err());
        assert_eq!(read_binding(&conn).unwrap(), Some(binding));
    }

    /// 2026-09-08: DB列と原文のUIDが食い違っても、どちらかが正本なら削除しない。
    #[test]
    fn deletion_checks_indexed_uid_as_well_as_corrupt_document_identity() {
        let (_dir, vault, conn, a, b) = setup();
        let binding = request(&vault, &conn, &a, None).unwrap().binding;
        let other = crate::note_store::read(&conn, &b)
            .unwrap()
            .to_file_string()
            .unwrap();
        conn.execute(
            "UPDATE notes SET document=?1 WHERE id=?2",
            params![other, a],
        )
        .unwrap();
        let before = crate::index::test_support::logical_snapshot(&vault.index_db_path());
        assert!(guard_source_note_id_removal(&conn, &a).is_err());
        assert!(
            crate::note_store::delete(
                &vault,
                &conn,
                &a,
                crate::note_store::WriteAttribution::new(
                    "削除fixture",
                    "test: delete",
                    &crate::provenance::test_context()
                )
            )
            .is_err()
        );
        assert_eq!(
            crate::index::test_support::logical_snapshot(&vault.index_db_path()),
            before
        );
        assert_eq!(read_binding(&conn).unwrap(), Some(binding));
        assert_eq!(crate::note_store::pending_count(&conn).unwrap(), 0);
        assert_eq!(pending_count(&conn).unwrap(), 0);
    }
}
