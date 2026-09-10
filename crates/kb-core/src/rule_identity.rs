//! 応答を生成した共通契約・案内と、同じDB snapshotで確認した語彙正本を識別する。
//! hostの受信やモデルの遵守を証明するreceiptではなく、観測した版の比較に限る。

use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::client_surface::ClientSurface;
use crate::mcp::ToolSurface;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RuleIdentity {
    pub schema: u32,
    pub contract_sha256: String,
    pub instructions_sha256: String,
    pub client_surface: ClientSurface,
    pub tool_surface: ToolSurface,
    pub server_version: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct WorkspaceRuleIdentity {
    pub schema: u32,
    pub workspace_id: String,
    pub vocabulary: VocabularyIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct VocabularyIdentity {
    pub source_status: VocabularySourceStatus,
    pub source_note_uid: Option<String>,
    /// 正本を指定した版。語彙本文の版ではない。
    pub source_revision: Option<String>,
    pub source_document_sha256: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum VocabularySourceStatus {
    Unconfigured,
    Pinned,
    Missing,
    Unavailable,
}

impl RuleIdentity {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.schema == 1, "規則識別情報のschemaが不明");
        ensure!(
            is_sha256(&self.contract_sha256) && is_sha256(&self.instructions_sha256),
            "規則識別情報のdigestが不正"
        );
        ensure!(
            !self.server_version.is_empty()
                && self.server_version.len() <= 128
                && !self.server_version.chars().any(char::is_control),
            "server versionが不正"
        );
        Ok(())
    }
}

impl WorkspaceRuleIdentity {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.schema == 1, "workspace規則識別情報のschemaが不明");
        ensure!(
            crate::artifact::is_ulid(&self.workspace_id),
            "workspace IDが不正"
        );
        let source = &self.vocabulary;
        let valid_uid = source
            .source_note_uid
            .as_deref()
            .is_some_and(crate::artifact::is_ulid);
        let valid_revision = source
            .source_revision
            .as_deref()
            .is_some_and(crate::artifact::is_ulid);
        let valid_hash = source
            .source_document_sha256
            .as_deref()
            .is_some_and(is_sha256);
        ensure!(
            match source.source_status {
                VocabularySourceStatus::Unconfigured =>
                    source.source_note_uid.is_none()
                        && source.source_revision.is_none()
                        && source.source_document_sha256.is_none(),
                VocabularySourceStatus::Pinned => valid_uid && valid_revision && valid_hash,
                VocabularySourceStatus::Missing | VocabularySourceStatus::Unavailable =>
                    valid_uid && valid_revision && source.source_document_sha256.is_none(),
            },
            "語彙正本の状態と識別情報が不整合"
        );
        Ok(())
    }
}

/// 組み立て済みの実際の案内を受け取り、OFFでもVaultや端末設定を読まずに識別する。
pub fn for_instructions(
    client_surface: ClientSurface,
    tool_surface: ToolSurface,
    instructions: &str,
) -> RuleIdentity {
    RuleIdentity {
        schema: 1,
        contract_sha256: sha256(include_bytes!("../../../docs/contract.md")),
        instructions_sha256: sha256(instructions.as_bytes()),
        client_surface,
        tool_surface,
        server_version: env!("CARGO_PKG_VERSION").into(),
    }
}

/// 呼出し側のread transactionに参加する。未指定時に候補や全タグを走査して正本を推測しない。
/// workspace IDは既存の接続先照合で確認した値を渡し、指定JSONの所属とも照合する。
pub fn workspace_snapshot(conn: &Connection, workspace_id: &str) -> Result<WorkspaceRuleIdentity> {
    ensure!(crate::artifact::is_ulid(workspace_id), "workspace IDが不正");
    let mut identity = WorkspaceRuleIdentity {
        schema: 1,
        workspace_id: workspace_id.into(),
        vocabulary: VocabularyIdentity {
            source_status: VocabularySourceStatus::Unconfigured,
            source_note_uid: None,
            source_revision: None,
            source_document_sha256: None,
        },
    };
    let Some(binding) = crate::tag_vocabulary_source::read_binding(conn)? else {
        return Ok(identity);
    };
    ensure!(
        binding.workspace_id == workspace_id,
        "語彙正本のworkspaceが不一致"
    );
    identity.vocabulary.source_note_uid = Some(binding.note_uid.to_string());
    identity.vocabulary.source_revision = Some(binding.revision);
    let row: Option<(String, bool, String)> = conn
        .query_row(
            "SELECT document, normal_reference_allowed = 1, status FROM notes WHERE note_uid = ?1",
            [binding.note_uid.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((document, allowed, status)) = row else {
        identity.vocabulary.source_status = VocabularySourceStatus::Missing;
        return Ok(identity);
    };
    let note = crate::frontmatter::Note::parse(&document)?;
    ensure!(
        note.front.note_uid.as_ref() == Some(&binding.note_uid) && note.front.authority.is_some(),
        "語彙正本の指定UIDとDB原文のidentityが不一致"
    );
    crate::authority::validate_envelope(
        note.front.note_uid.as_ref(),
        note.front.authority.as_ref(),
        &note.front.relations,
    )?;
    if !allowed
        || status == "deprecated"
        || note.front.effective_status() == "deprecated"
        || !crate::proposal_workflow::derive_normal_reference_allowed(&note)
    {
        // 通常参照できない正本の内容hashも公開しない。指定の消失とは区別する。
        identity.vocabulary.source_status = VocabularySourceStatus::Unavailable;
        return Ok(identity);
    }
    identity.vocabulary.source_status = VocabularySourceStatus::Pinned;
    identity.vocabulary.source_document_sha256 = Some(sha256(document.as_bytes()));
    Ok(identity)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, crate::vault::Vault, Connection, String) {
        let dir = tempfile::tempdir().unwrap();
        let vault = crate::vault::Vault::create(dir.path().join("vault")).unwrap();
        let note = vault
            .propose_for_test(
                "語彙の正本",
                "## 語彙\n| test | テスト |\n",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        (dir, vault, conn, note)
    }

    #[test]
    fn identity_tracks_actual_instructions_without_claiming_host_reception() {
        let codex = for_instructions(ClientSurface::CodexCli, ToolSurface::Read, "共通案内");
        let claude = for_instructions(ClientSurface::ClaudeCode, ToolSurface::Read, "共通案内");
        assert_eq!(codex.contract_sha256, claude.contract_sha256);
        assert_eq!(codex.instructions_sha256, claude.instructions_sha256);
        assert_ne!(codex.client_surface, claude.client_surface);
        let changed = for_instructions(ClientSurface::CodexCli, ToolSurface::Read, "更新案内");
        assert_eq!(codex.contract_sha256, changed.contract_sha256);
        assert_ne!(codex.instructions_sha256, changed.instructions_sha256);
        let unknown = for_instructions(ClientSurface::Unknown, ToolSurface::Read, "停止案内");
        assert_eq!(unknown.client_surface, ClientSurface::Unknown);
        let json = serde_json::to_value(&unknown).unwrap();
        assert!(json.get("received").is_none());
        assert!(json.get("workspace_id").is_none());
        unknown.validate().unwrap();
    }

    #[test]
    fn stored_identity_validation_rejects_unknown_versions_and_inconsistent_status() {
        let valid = for_instructions(ClientSurface::CodexCli, ToolSurface::Read, "共通案内");
        for replacement in ["A".repeat(64), "a".repeat(63), "g".repeat(64)] {
            let mut invalid = valid.clone();
            invalid.instructions_sha256 = replacement;
            assert!(invalid.validate().is_err());
        }
        let mut invalid = valid.clone();
        invalid.schema = 2;
        assert!(invalid.validate().is_err());
        for version in ["".into(), "a".repeat(129), "1.0\n".into()] {
            invalid = valid.clone();
            invalid.server_version = version;
            assert!(invalid.validate().is_err());
        }
        let identity = WorkspaceRuleIdentity {
            schema: 1,
            workspace_id: "01AAAAAAAAAAAAAAAAAAAAAAAA".into(),
            vocabulary: VocabularyIdentity {
                source_status: VocabularySourceStatus::Unconfigured,
                source_note_uid: None,
                source_revision: None,
                source_document_sha256: None,
            },
        };
        identity.validate().unwrap();
        let mut invalid = identity.clone();
        invalid.vocabulary.source_status = VocabularySourceStatus::Pinned;
        assert!(invalid.validate().is_err());
        invalid = identity.clone();
        invalid.vocabulary.source_document_sha256 = Some("a".repeat(64));
        assert!(invalid.validate().is_err());
        invalid = identity;
        invalid.workspace_id = "unknown".into();
        assert!(invalid.validate().is_err());
    }

    /// 2026-09-08: 指定revisionが同じでも語彙本文は変わる。古い本文を同じ版に見せない。
    #[test]
    fn vocabulary_content_hash_changes_without_changing_the_source_revision() {
        let (_dir, vault, conn, note_id) = setup();
        let workspace = crate::workspace::stored_workspace_id(&vault).unwrap();
        let unconfigured = workspace_snapshot(&conn, &workspace).unwrap();
        assert_eq!(
            unconfigured.vocabulary.source_status,
            VocabularySourceStatus::Unconfigured
        );
        assert!(unconfigured.vocabulary.source_note_uid.is_none());
        crate::tag_vocabulary_source::pin_for_test(&vault, &conn, &note_id).unwrap();
        let before = workspace_snapshot(&conn, &workspace).unwrap();
        let mut note = crate::note_store::read(&conn, &note_id).unwrap();
        note.body.push_str("\n説明を追加した。\n");
        conn.execute(
            "UPDATE notes SET document=?1,body=?2 WHERE id=?3",
            rusqlite::params![note.to_file_string().unwrap(), note.body, note_id],
        )
        .unwrap();
        let after = workspace_snapshot(&conn, &workspace).unwrap();
        assert_eq!(
            before.vocabulary.source_note_uid,
            after.vocabulary.source_note_uid
        );
        assert_eq!(
            before.vocabulary.source_revision,
            after.vocabulary.source_revision
        );
        assert_ne!(
            before.vocabulary.source_document_sha256,
            after.vocabulary.source_document_sha256
        );
    }

    #[test]
    fn missing_unavailable_and_mismatched_sources_never_look_unconfigured() {
        let (_dir, vault, conn, note) = setup();
        let workspace = crate::workspace::stored_workspace_id(&vault).unwrap();
        crate::tag_vocabulary_source::pin_for_test(&vault, &conn, &note).unwrap();
        assert!(workspace_snapshot(&conn, "01AAAAAAAAAAAAAAAAAAAAAAAA").is_err());
        conn.execute(
            "UPDATE notes SET normal_reference_allowed=0 WHERE id=?1",
            [&note],
        )
        .unwrap();
        let unavailable = workspace_snapshot(&conn, &workspace).unwrap();
        assert_eq!(
            unavailable.vocabulary.source_status,
            VocabularySourceStatus::Unavailable
        );
        assert!(unavailable.vocabulary.source_document_sha256.is_none());
        conn.execute("DELETE FROM notes WHERE id=?1", [&note])
            .unwrap();
        let missing = workspace_snapshot(&conn, &workspace).unwrap();
        assert_eq!(
            missing.vocabulary.source_status,
            VocabularySourceStatus::Missing
        );
        assert!(missing.vocabulary.source_note_uid.is_some());
        assert!(missing.vocabulary.source_document_sha256.is_none());
    }

    #[test]
    fn vocabulary_identity_uses_the_callers_read_snapshot() {
        let (_dir, vault, conn, note_id) = setup();
        let workspace = crate::workspace::stored_workspace_id(&vault).unwrap();
        crate::tag_vocabulary_source::pin_for_test(&vault, &conn, &note_id).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL").unwrap();
        let snapshot = conn.unchecked_transaction().unwrap();
        let before = workspace_snapshot(&snapshot, &workspace).unwrap();
        let other = Connection::open(vault.index_db_path()).unwrap();
        let mut note = crate::note_store::read(&other, &note_id).unwrap();
        note.body.push_str("\n別接続による変更。\n");
        other
            .execute(
                "UPDATE notes SET document=?1,body=?2 WHERE id=?3",
                rusqlite::params![note.to_file_string().unwrap(), note.body, note_id],
            )
            .unwrap();
        assert_eq!(workspace_snapshot(&snapshot, &workspace).unwrap(), before);
        snapshot.commit().unwrap();
        assert_ne!(workspace_snapshot(&conn, &workspace).unwrap(), before);
    }
}
