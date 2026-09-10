//! 保存済みの前後原文を照合し、後続の編集を上書きせず語彙変更を戻す。
//!
//! 復元前の無関係な編集は許容するが、計画後の変更はsnapshotで拒否する。
//! 元の実行履歴は消さず、復元のactor・理由・時刻を別の台帳へ残す。

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::frontmatter::{Note, now_iso};
use crate::tag_vocabulary_changes::{Blocker, Example};
use crate::tag_vocabulary_history::{HistoryNote, RollbackRecord};
use crate::vault::Vault;

// 履歴は前後両方を読むため、合計で上限を適用する。超過時に一部だけ戻さない。
const MAX_NOTES: usize = 100_000;
const MAX_DOCUMENT_BYTES: usize = 256 * 1024 * 1024;
const SAMPLE_LIMIT: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub schema: u32,
    pub workspace_id: String,
    pub execution_id: String,
    pub source_note_uid: String,
    pub source_revision: String,
    pub history_hash: String,
    pub snapshot_digest: String,
    pub reason: String,
    pub plan_hash: String,
}

#[derive(Debug, Serialize)]
pub struct Plan {
    pub receipt: Receipt,
    pub restored_notes: usize,
    pub examples: Vec<Example>,
    pub blockers_count: usize,
    pub blockers: Vec<Blocker>,
    pub can_apply: bool,
}

#[derive(Debug, Serialize)]
pub struct RollbackResult {
    pub execution_id: String,
    pub rollback_id: String,
    pub stored: bool,
    pub restored_notes: usize,
    pub pending_exports: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown_export_error: Option<String>,
}

/// 計画はDBの読取だけで完結し、同期・出力・履歴への記録を伴わない。
pub fn plan(conn: &Connection, execution_id: &str, reason: &str) -> Result<Plan> {
    let transaction = conn.unchecked_transaction()?;
    let (plan, _) = prepare(&transaction, execution_id, reason)?;
    transaction.commit()?;
    Ok(plan)
}

pub fn rollback(
    vault: &Vault,
    conn: &Connection,
    rollback_id: &str,
    receipt: &Receipt,
    client: Option<&str>,
) -> Result<RollbackResult> {
    ensure!(
        crate::artifact::is_ulid(rollback_id),
        "rollback_idはULIDで指定する"
    );
    ensure!(
        receipt.schema == 1,
        "語彙復元receiptのschemaに対応していない"
    );
    let client = client.unwrap_or("mcp");
    ensure!(
        !client.is_empty() && client.len() <= 200 && !client.chars().any(char::is_control),
        "clientが不正"
    );
    let restored_notes = (|| -> Result<usize> {
        let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        ensure!(
            !operation_id_exists(&transaction, rollback_id)?,
            "この実行IDは使用済み。元のexecution_idで語彙変更履歴を確認する"
        );
        crate::tag_vocabulary_source::ensure_workspace(vault, &transaction)?;
        ensure!(
            receipt.workspace_id == crate::workspace::stored_workspace_id(vault)?,
            "復元計画と接続先workspaceが一致しない"
        );
        let (current, prepared) = prepare(&transaction, &receipt.execution_id, &receipt.reason)?;
        ensure!(
            &current.receipt == receipt,
            "語彙復元planが現在のsnapshotと一致しない。plan_tag_vocabulary_rollbackからやり直す"
        );
        ensure!(current.can_apply, "語彙変更を復元できない。planのblockersを確認する");
        // 復元もノートへの書込。逆向きの改版として来歴イベントを1件ずつ残す。
        let actor = crate::provenance::WriteActor::from_client_hint(client);
        let revision = crate::provenance::RevisionInput {
            kind: Some(crate::provenance::RevisionKind::Reverse),
            summary: Some(receipt.reason.clone()),
            ..crate::provenance::RevisionInput::default()
        };
        let context = crate::provenance::WriteContext {
            actor: &actor,
            revision: Some(&revision),
            operation: crate::provenance::Operation::Update,
        };
        for (i, item) in prepared.iter().enumerate() {
            // 元のgeneratedを含めた原文へ戻す。今回の実行者は復元台帳へ記録する。
            let restored = parse_document(&item.before_document)?;
            crate::note_store::queue_put(
                vault,
                &transaction,
                &item.note_id,
                &restored,
                &format!("tag-vocabulary:{rollback_id}:{i}"),
                crate::note_store::WriteAttribution::new(
                    &format!(
                        "**Tag vocabulary rollback**: {}件を復元。元の実行 `{}`、復元 `{rollback_id}`。理由: {}",
                        current.restored_notes, receipt.execution_id, receipt.reason
                    ),
                    &format!(
                        "tags: 語彙と{}件を復元 ({rollback_id}, via {client})",
                        current.restored_notes
                    ),
                    &context,
                ),
            )?;
        }
        crate::index::validate_authority_index(&transaction)?;
        crate::tag_vocabulary_history::record_rollback(
            &transaction,
            &RollbackRecord {
                rollback_id: rollback_id.to_string(),
                execution_id: receipt.execution_id.clone(),
                plan_hash: receipt.plan_hash.clone(),
                reason: receipt.reason.clone(),
                client: client.to_string(),
                restored_at: now_iso(),
                restored_notes: current.restored_notes,
            },
        )?;
        transaction.commit()?;
        Ok(current.restored_notes)
    })()
    .map_err(crate::write_rejection::confirm_before_write)?;
    let (pending_exports, markdown_export_error) =
        crate::tag_vocabulary_changes::export_result(vault, conn);
    Ok(RollbackResult {
        execution_id: receipt.execution_id.clone(),
        rollback_id: rollback_id.to_string(),
        stored: true,
        restored_notes,
        pending_exports,
        markdown_export_error,
    })
}

/// applyとrollbackで同じ出力グループを使うため、log markerも含めIDを共有して予約する。
pub(crate) fn operation_id_exists(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM tag_vocabulary_runs WHERE execution_id=?1) OR EXISTS(SELECT 1 FROM tag_vocabulary_rollbacks WHERE rollback_id=?1)",
        [id], |row| row.get(0),
    )?)
}

fn hash_field(hash: &mut Sha256, value: &str) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value.as_bytes());
}

fn parse_document(document: &str) -> Result<Note> {
    // 非参照ノートの不正な値をparse errorの詳細から返さない。
    Note::parse(document).map_err(|_| anyhow::anyhow!("復元に必要なノート原文を読み取れない"))
}

fn prepare(
    conn: &Connection,
    execution_id: &str,
    reason: &str,
) -> Result<(Plan, Vec<HistoryNote>)> {
    ensure!(
        crate::artifact::is_ulid(execution_id),
        "execution_idはULIDで指定する"
    );
    ensure!(
        !reason.is_empty()
            && reason.trim() == reason
            && reason.chars().count() <= 500
            && !reason.chars().any(char::is_control),
        "語彙復元の理由は1〜500文字の一行で指定する"
    );
    let mut history = crate::tag_vocabulary_history::read_for_rollback(
        conn,
        execution_id,
        MAX_NOTES,
        MAX_DOCUMENT_BYTES,
    )?
    .context("この端末に語彙変更履歴がない。実行IDと接続先を確認する")?;
    crate::tag_vocabulary_changes::validate_recorded_change(&history.run, &history.notes)
        .map_err(|_| anyhow::anyhow!("語彙変更履歴の操作と前後原文が一致しない"))?;
    history.notes.sort_by(|a, b| a.note_id.cmp(&b.note_id));
    let source_index = history
        .notes
        .iter()
        .position(|note| note.note_uid.as_deref() == Some(history.run.source_note_uid.as_str()))
        .context("語彙変更履歴に正本がない")?;
    let source = &history.notes[source_index];
    let before_source = parse_document(&source.before_document)?;
    let after_source = parse_document(&source.after_document)?;
    let restored_entries =
        crate::tags::parse_glossary(source.note_id.clone(), &before_source.body).entries;
    let changed_entries =
        crate::tags::parse_glossary(source.note_id.clone(), &after_source.body).entries;
    let removed_tags: BTreeSet<_> = changed_entries
        .keys()
        .filter(|tag| !restored_entries.contains_key(*tag))
        .map(String::as_str)
        .collect();
    let mut blockers = BTreeMap::<String, usize>::new();
    if history.rollback.is_some() {
        *blockers.entry("already_rolled_back".into()).or_default() += 1;
    }
    let pending = crate::note_store::pending_count(conn)?
        + crate::tag_vocabulary_source::pending_count(conn)?;
    if pending > 0 {
        blockers.insert("pending_exports".into(), pending);
    }
    let binding = crate::tag_vocabulary_source::read_binding(conn)?;
    if !binding.as_ref().is_some_and(|binding| {
        binding.workspace_id == history.run.workspace_id
            && binding.note_uid.as_str() == history.run.source_note_uid
            && binding.revision == history.run.source_revision
    }) {
        blockers.insert("source_binding_changed".into(), 1);
    }
    let mut history_hash = Sha256::new();
    for field in [
        &history.run.execution_id,
        &history.run.workspace_id,
        &history.run.source_note_uid,
        &history.run.source_revision,
        &history.run.plan_hash,
        &history.run.operations_json,
        &history.run.reason,
        &history.run.client,
        &history.run.applied_at,
    ] {
        hash_field(&mut history_hash, field);
    }
    hash_field(
        &mut history_hash,
        &serde_json::to_string(&history.rollback)?,
    );
    let mut expected = BTreeMap::new();
    for note in &history.notes {
        for field in [
            &note.note_id,
            note.note_uid.as_deref().unwrap_or(""),
            &note.before_document,
            &note.after_document,
        ] {
            hash_field(&mut history_hash, field);
        }
        expected.insert(note.note_id.as_str(), note);
    }
    let mut snapshot = Sha256::new();
    hash_field(&mut snapshot, &serde_json::to_string(&binding)?);
    let mut stmt = conn.prepare(
        "SELECT id,document,tags,normal_reference_allowed,note_uid FROM notes ORDER BY id",
    )?;
    let mut rows = stmt.query([])?;
    let mut examples = Vec::new();
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let document: String = row.get(1)?;
        let tags: String = row.get(2)?;
        let allowed: bool = row.get(3)?;
        let uid: Option<String> = row.get(4)?;
        for field in [&id, &document, &tags, uid.as_deref().unwrap_or("")] {
            hash_field(&mut snapshot, field);
        }
        snapshot.update([u8::from(allowed)]);
        let Some(stored) = expected.remove(id.as_str()) else {
            if tags
                .split_whitespace()
                .any(|tag| removed_tags.contains(tag))
            {
                *blockers
                    .entry("new_tag_used_outside_execution".into())
                    .or_default() += 1;
            }
            continue;
        };
        let current = parse_document(&document)?;
        let restored = parse_document(&stored.before_document)?;
        let normal = allowed && crate::proposal_workflow::derive_normal_reference_allowed(&current);
        let problem = if document != stored.after_document || uid != stored.note_uid {
            Some("target_changed")
        } else if tags != current.front.tags.join(" ") {
            Some("inconsistent_tag_index")
        } else if current.front.origin.as_deref() != Some("agent")
            || restored.front.origin.as_deref() != Some("agent")
        {
            Some("protected_origin")
        } else if !normal
            || current.front.extra.contains_key("proposal_ticket")
            || !crate::proposal_workflow::derive_normal_reference_allowed(&restored)
            || restored.front.extra.contains_key("proposal_ticket")
        {
            Some("protected_proposal")
        } else if crate::tags::validate_structure(&restored.front.tags).is_err() {
            Some("invalid_restored_tags")
        } else if restored
            .front
            .tags
            .iter()
            .any(|tag| !restored_entries.contains_key(tag))
        {
            Some("unknown_restored_tag")
        } else if restored.to_file_string()? != stored.before_document {
            Some("noncanonical_history_document")
        } else {
            None
        };
        if let Some(problem) = problem {
            *blockers.entry(problem.into()).or_default() += 1;
        }
        if normal
            && document == stored.after_document
            && uid == stored.note_uid
            && examples.len() < SAMPLE_LIMIT
        {
            examples.push(Example {
                note: id,
                note_uid: uid,
                title: current.front.title,
                before_tags: current.front.tags,
                after_tags: restored.front.tags,
            });
        }
    }
    if !expected.is_empty() {
        blockers.insert("target_missing".into(), expected.len());
    }
    let mut receipt = Receipt {
        schema: 1,
        workspace_id: history.run.workspace_id,
        execution_id: execution_id.to_string(),
        source_note_uid: history.run.source_note_uid,
        source_revision: history.run.source_revision,
        history_hash: format!("sha256:{:x}", history_hash.finalize()),
        snapshot_digest: format!("sha256:{:x}", snapshot.finalize()),
        reason: reason.to_string(),
        plan_hash: String::new(),
    };
    receipt.plan_hash = crate::distillation::sha256(&serde_json::to_vec(&receipt)?);
    let blockers_count = blockers.values().sum();
    let source = history.notes.remove(source_index);
    // 利用ノートを先に戻してから正本を戻し、使用中語の削除guardを共通経路で通す。
    history.notes.push(source);
    Ok((
        Plan {
            receipt,
            restored_notes: history.notes.len(),
            examples,
            blockers_count,
            blockers: blockers
                .into_iter()
                .map(|(code, count)| Blocker { code, count })
                .collect(),
            can_apply: blockers_count == 0,
        },
        history.notes,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::NoteUid;
    use crate::tag_vocabulary_changes::{Changes, apply};

    struct Fixture {
        _dir: tempfile::TempDir,
        vault: Vault,
        conn: Connection,
        source: String,
        target: String,
        before: BTreeMap<String, String>,
        execution_id: String,
    }

    fn document(conn: &Connection, id: &str) -> String {
        conn.query_row("SELECT document FROM notes WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .unwrap()
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        let source = vault.propose_for_test("語彙正本", "運用本文を残す。\n\n## 語彙\n| タグ | 説明 |\n| --- | --- |\n| old | 旧語 |\n| new | 新語 |\n| unused | 未使用 |\n\n## 関連\nこの節を保持する。", None, &["old".into()], "test").unwrap();
        let target = vault
            .propose_for_test(
                "対象",
                "変えない本文と[外部リンク](https://example.com)。",
                Some("変えない説明"),
                &["old".into(), "new".into()],
                "test",
            )
            .unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        crate::tag_vocabulary_source::pin_for_test(&vault, &conn, &source).unwrap();
        let before = [source.clone(), target.clone()]
            .into_iter()
            .map(|id| {
                let doc = document(&conn, &id);
                (id, doc)
            })
            .collect();
        let changes = Changes {
            upsert: BTreeMap::from([("joined".into(), "統合後の語".into())]),
            remove: vec!["unused".into()],
            replace: BTreeMap::from([
                ("old".into(), "joined".into()),
                ("new".into(), "joined".into()),
            ]),
        };
        let p =
            crate::tag_vocabulary_changes::plan(&conn, &changes, "重複した語をまとめる").unwrap();
        let execution_id = NoteUid::new().to_string();
        apply(&vault, &conn, &execution_id, &p.receipt, Some("test/apply")).unwrap();
        Fixture {
            _dir: dir,
            vault,
            conn,
            source,
            target,
            before,
            execution_id,
        }
    }

    fn receipt(f: &Fixture) -> Receipt {
        plan(&f.conn, &f.execution_id, "分類が粗すぎたため元に戻す")
            .unwrap()
            .receipt
    }

    fn rollback_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT count(*) FROM tag_vocabulary_rollbacks", [], |row| {
            row.get(0)
        })
        .unwrap()
    }

    fn change_body(f: &Fixture, id: &str) {
        let mut note = crate::note_store::read(&f.conn, id).unwrap();
        note.body.push_str("後からの編集を残す。\n");
        crate::index::upsert(&f.conn, &f.vault, id, 1, &note).unwrap();
    }

    fn add_outside(f: &Fixture, tag: &str) -> String {
        let mut note = crate::note_store::read(&f.conn, &f.target).unwrap();
        note.front.note_uid = Some(NoteUid::new());
        note.front.tags = vec![tag.into()];
        let id = "notes/outside".to_string();
        crate::index::upsert(&f.conn, &f.vault, &id, 1, &note).unwrap();
        id
    }

    /// 2026-09-08: 統合で重複除去したタグ順とgeneratedも復元し、元履歴を消さない。
    #[test]
    fn restores_original_documents_and_records_one_rollback_commit() {
        let f = fixture();
        let before_rows = crate::index::test_support::durable_rows_snapshot(&f.conn);
        let p = plan(&f.conn, &f.execution_id, "分類が粗すぎたため元に戻す").unwrap();
        assert!(p.can_apply);
        assert_eq!(p.restored_notes, 2);
        assert_eq!(
            before_rows,
            crate::index::test_support::durable_rows_snapshot(&f.conn)
        );
        let repo = git2::Repository::open(&f.vault.root).unwrap();
        let applied_commit = repo.head().unwrap().target().unwrap();
        let id = NoteUid::new().to_string();
        let result = rollback(&f.vault, &f.conn, &id, &p.receipt, Some("test/rollback")).unwrap();
        assert!(result.stored);
        assert_eq!(result.pending_exports, Some(0));
        assert_eq!(result.restored_notes, 2);
        for (note, before) in &f.before {
            assert_eq!(&document(&f.conn, note), before);
            assert_eq!(
                std::fs::read_to_string(f.vault.root.join(format!("{note}.md"))).unwrap(),
                *before
            );
        }
        assert_eq!(
            repo.head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .parent_id(0)
                .unwrap(),
            applied_commit
        );
        let history = crate::tag_vocabulary_history::get(&f.conn, &f.execution_id, None, None)
            .unwrap()
            .unwrap();
        let restored = history.run.rollback.unwrap();
        assert_eq!(restored.rollback_id, id);
        assert_eq!(restored.client, "test/rollback");
        assert_eq!(history.notes.len(), 2);
        assert_eq!(rollback_count(&f.conn), 1);
        crate::tag_vocabulary_history::verify_integrity(&f.conn).unwrap();
        assert!(rollback(&f.vault, &f.conn, &id, &p.receipt, None).is_err());
        let p = plan(&f.conn, &f.execution_id, "再試行").unwrap();
        assert!(!p.can_apply);
        assert!(p.blockers.iter().any(|b| b.code == "already_rolled_back"));
        assert!(
            rollback(
                &f.vault,
                &f.conn,
                &NoteUid::new().to_string(),
                &p.receipt,
                None
            )
            .is_err()
        );
        let changes = Changes {
            upsert: BTreeMap::from([("extra".into(), "追加".into())]),
            remove: vec![],
            replace: BTreeMap::new(),
        };
        let apply_plan =
            crate::tag_vocabulary_changes::plan(&f.conn, &changes, "復元IDの再使用を検査").unwrap();
        assert!(apply(&f.vault, &f.conn, &id, &apply_plan.receipt, None).is_err());
    }

    #[test]
    fn tampered_receipts_and_operation_id_collisions_are_rejected() {
        let f = fixture();
        let original = receipt(&f);
        for variant in [
            "reason",
            "history",
            "snapshot",
            "workspace",
            "schema",
            "execution",
        ] {
            let mut r = original.clone();
            match variant {
                "reason" => r.reason = "別の理由".into(),
                "history" => r.history_hash.push('0'),
                "snapshot" => r.snapshot_digest.push('0'),
                "workspace" => r.workspace_id = NoteUid::new().to_string(),
                "schema" => r.schema = 2,
                _ => r.execution_id = NoteUid::new().to_string(),
            }
            assert!(
                rollback(&f.vault, &f.conn, &NoteUid::new().to_string(), &r, None).is_err(),
                "{variant}"
            );
        }
        assert!(rollback(&f.vault, &f.conn, &f.execution_id, &original, None).is_err());
        assert!(rollback(&f.vault, &f.conn, "not-ulid", &original, None).is_err());
        assert!(plan(&f.conn, &f.execution_id, "理由\n改行").is_err());
        assert!(plan(&f.conn, &NoteUid::new().to_string(), "別端末の履歴").is_err());
        assert_eq!(rollback_count(&f.conn), 0);
    }

    /// 2026-09-08: 変更後の本文・タグ・正本指定・参照状態を復元操作で上書きしない。
    #[test]
    fn later_target_changes_and_protected_notes_block_the_whole_restore() {
        for variant in [
            "body", "source", "binding", "hidden", "missing", "uid", "index",
        ] {
            let f = fixture();
            match variant {
                "body" => change_body(&f, &f.target),
                "source" => change_body(&f, &f.source),
                "binding" => {
                    crate::tag_vocabulary_source::pin_for_test(&f.vault, &f.conn, &f.source)
                        .unwrap();
                }
                "hidden" => {
                    f.conn
                        .execute(
                            "UPDATE notes SET normal_reference_allowed=0 WHERE id=?1",
                            [&f.target],
                        )
                        .unwrap();
                }
                "missing" => {
                    f.conn
                        .execute("DELETE FROM notes WHERE id=?1", [&f.target])
                        .unwrap();
                }
                "uid" => {
                    f.conn
                        .execute(
                            "UPDATE notes SET note_uid=?1 WHERE id=?2",
                            rusqlite::params![NoteUid::new().to_string(), f.target],
                        )
                        .unwrap();
                }
                _ => {
                    f.conn
                        .execute("UPDATE notes SET tags='old' WHERE id=?1", [&f.target])
                        .unwrap();
                }
            }
            let before = crate::index::test_support::durable_rows_snapshot(&f.conn);
            let p = plan(&f.conn, &f.execution_id, "後続の編集との競合を確認").unwrap();
            assert!(!p.can_apply, "{variant}");
            if matches!(variant, "hidden" | "uid" | "missing" | "body") {
                assert!(p.examples.iter().all(|e| e.note != f.target));
            }
            assert!(
                rollback(
                    &f.vault,
                    &f.conn,
                    &NoteUid::new().to_string(),
                    &p.receipt,
                    None
                )
                .is_err()
            );
            assert_eq!(
                before,
                crate::index::test_support::durable_rows_snapshot(&f.conn)
            );
            assert_eq!(rollback_count(&f.conn), 0);
        }
    }

    #[test]
    fn new_usage_outside_the_run_blocks_removing_an_added_tag_without_exposing_ids() {
        let f = fixture();
        let id = add_outside(&f, "joined");
        f.conn
            .execute(
                "UPDATE notes SET normal_reference_allowed=0 WHERE id=?1",
                [&id],
            )
            .unwrap();
        let p = plan(&f.conn, &f.execution_id, "元の語彙へ戻す").unwrap();
        assert!(!p.can_apply);
        assert!(
            p.blockers
                .iter()
                .any(|b| b.code == "new_tag_used_outside_execution" && b.count == 1)
        );
        assert!(!serde_json::to_string(&p).unwrap().contains(&id));
        assert!(
            rollback(
                &f.vault,
                &f.conn,
                &NoteUid::new().to_string(),
                &p.receipt,
                None
            )
            .is_err()
        );
    }

    #[test]
    fn unrelated_changes_before_plan_are_preserved_but_changes_after_plan_are_stale() {
        let f = fixture();
        // 以前からの語彙外タグを使う対象外の旧ノートは、今回の復元では修復しない。
        let id = add_outside(&f, "legacy");
        let r = receipt(&f);
        change_body(&f, &id);
        let later = document(&f.conn, &id);
        assert!(rollback(&f.vault, &f.conn, &NoteUid::new().to_string(), &r, None).is_err());
        let p = plan(&f.conn, &f.execution_id, "無関係な編集を残して戻す").unwrap();
        assert!(p.can_apply);
        rollback(
            &f.vault,
            &f.conn,
            &NoteUid::new().to_string(),
            &p.receipt,
            None,
        )
        .unwrap();
        assert_eq!(document(&f.conn, &id), later);
    }

    /// 2026-09-08: 最終ノートや復元履歴の保存失敗で、一部だけ元へ戻ることを防ぐ。
    #[test]
    fn late_write_or_history_failure_keeps_all_documents_and_history_applied() {
        for history_failure in [false, true] {
            let f = fixture();
            let r = receipt(&f);
            let before = crate::index::test_support::durable_rows_snapshot(&f.conn);
            if history_failure {
                f.conn.execute_batch("CREATE TRIGGER fixture_fail BEFORE INSERT ON tag_vocabulary_rollbacks BEGIN SELECT RAISE(FAIL,'fixture failure'); END;").unwrap();
            } else {
                f.conn.execute_batch("CREATE TRIGGER fixture_fail BEFORE UPDATE ON notes WHEN NEW.title='語彙正本' BEGIN SELECT RAISE(FAIL,'fixture failure'); END;").unwrap();
            }
            assert!(rollback(&f.vault, &f.conn, &NoteUid::new().to_string(), &r, None).is_err());
            assert_eq!(
                before,
                crate::index::test_support::durable_rows_snapshot(&f.conn)
            );
            assert_eq!(rollback_count(&f.conn), 0);
            assert_eq!(crate::note_store::pending_count(&f.conn).unwrap(), 0);
        }
    }

    #[test]
    fn export_failure_is_saved_and_the_entire_rollback_export_can_resume() {
        let f = fixture();
        let applied = document(&f.conn, &f.target);
        let r = receipt(&f);
        let path = f.vault.root.join(format!("{}.md", f.target));
        std::fs::write(&path, "別経路の変更").unwrap();
        let result = rollback(&f.vault, &f.conn, &NoteUid::new().to_string(), &r, None).unwrap();
        assert!(result.stored);
        assert_eq!(result.pending_exports, Some(2));
        assert!(result.markdown_export_error.is_some());
        assert_eq!(rollback_count(&f.conn), 1);
        assert_eq!(document(&f.conn, &f.target), f.before[&f.target]);
        std::fs::write(&path, applied).unwrap();
        f.vault.flush_note_exports(&f.conn).unwrap();
        assert_eq!(crate::note_store::pending_count(&f.conn).unwrap(), 0);
        assert_eq!(std::fs::read_to_string(path).unwrap(), f.before[&f.target]);
    }

    #[test]
    fn pending_exports_block_the_plan_and_raw_or_coherent_history_tampering_is_rejected() {
        let f = fixture();
        let original = document(&f.conn, &f.target);
        let note = crate::note_store::read(&f.conn, &f.target).unwrap();
        let tx = f.conn.unchecked_transaction().unwrap();
        crate::note_store::queue_put(
            &f.vault,
            &tx,
            &f.target,
            &note,
            "fixture-pending",
            crate::note_store::WriteAttribution::new(
                "pending",
                "pending",
                &crate::provenance::test_context(),
            ),
        )
        .unwrap();
        tx.commit().unwrap();
        let p = plan(&f.conn, &f.execution_id, "出力待ちを確認").unwrap();
        assert!(!p.can_apply);
        assert!(p.blockers.iter().any(|b| b.code == "pending_exports"));
        assert!(
            rollback(
                &f.vault,
                &f.conn,
                &NoteUid::new().to_string(),
                &p.receipt,
                None
            )
            .is_err()
        );
        f.vault.flush_note_exports(&f.conn).unwrap();
        assert_eq!(document(&f.conn, &f.target), original);
        f.conn.execute("UPDATE tag_vocabulary_run_notes SET before_document='bad' WHERE execution_id=?1 AND note_id=?2", rusqlite::params![f.execution_id, f.target]).unwrap();
        assert!(plan(&f.conn, &f.execution_id, "壊れた履歴を拒否").is_err());
        let mut forged = parse_document(&f.before[&f.target]).unwrap();
        forged.body = "過去の本文を偽装".into();
        let forged = forged.to_file_string().unwrap();
        f.conn.execute("UPDATE tag_vocabulary_run_notes SET before_document=?1,before_hash=?2 WHERE execution_id=?3 AND note_id=?4", rusqlite::params![forged, crate::distillation::sha256(forged.as_bytes()), f.execution_id, f.target]).unwrap();
        let error = plan(&f.conn, &f.execution_id, "操作と一致しない履歴を拒否").unwrap_err();
        assert!(error.to_string().contains("操作と前後原文"));
        assert!(!format!("{error:#}").contains("過去の本文を偽装"));
    }
}
