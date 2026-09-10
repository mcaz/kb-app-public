//! 語彙表と利用ノートを同じsnapshotで変更する。判断はAI、適用条件はコアが守る。
//!
//! 数万件の原文をクライアントへ往復させず、短い操作とreceiptだけを受け取る。
//! planは本人承認ではない。適用前に同じDBを再走査し、途中の変更を拒否する。

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::frontmatter::{Generated, Note, now_iso};
use crate::tag_vocabulary_history::{HistoryNote, HistoryRun};
use crate::vault::Vault;

mod glossary;

// 応答サイズと1 transactionのメモリを別々に制限する。上限超過は部分適用にしない。
const SAMPLE_LIMIT: usize = 20;
const MAX_CHANGED_NOTES: usize = 100_000;
const MAX_OPERATIONS: usize = 1_000;
const MAX_DOCUMENT_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Changes {
    pub upsert: BTreeMap<String, String>,
    pub remove: Vec<String>,
    pub replace: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub schema: u32,
    pub workspace_id: String,
    pub source_note_uid: String,
    pub source_revision: String,
    pub source_document_hash: String,
    pub snapshot_digest: String,
    pub changes: Changes,
    pub reason: String,
    pub plan_hash: String,
}

#[derive(Debug, Serialize)]
pub struct Example {
    pub note: String,
    pub note_uid: Option<String>,
    pub title: Option<String>,
    pub before_tags: Vec<String>,
    pub after_tags: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Blocker {
    pub code: String,
    pub count: usize,
}

#[derive(Debug, Serialize)]
pub struct Plan {
    pub receipt: Receipt,
    pub changed_notes: usize,
    pub examples: Vec<Example>,
    pub blockers_count: usize,
    pub blockers: Vec<Blocker>,
    pub can_apply: bool,
}

#[derive(Debug, Serialize)]
pub struct ApplyResult {
    pub execution_id: String,
    pub stored: bool,
    pub changed_notes: usize,
    pub pending_exports: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown_export_error: Option<String>,
}

struct PreparedNote {
    id: String,
    before: String,
    after: Note,
}

/// 書き込みや出力を伴わない。複数のSELECTも同じread snapshotで評価する。
pub fn plan(conn: &Connection, changes: &Changes, reason: &str) -> Result<Plan> {
    let transaction = conn.unchecked_transaction()?;
    let (plan, _) = prepare(&transaction, changes, reason)?;
    transaction.commit()?;
    Ok(plan)
}

pub fn apply(
    vault: &Vault,
    conn: &Connection,
    execution_id: &str,
    receipt: &Receipt,
    client: Option<&str>,
) -> Result<ApplyResult> {
    ensure!(
        crate::artifact::is_ulid(execution_id),
        "execution_idはULIDで指定する"
    );
    ensure!(
        receipt.schema == 1,
        "語彙変更receiptのschemaに対応していない"
    );
    let client = client.unwrap_or("mcp");
    ensure!(
        !client.is_empty() && client.len() <= 200 && !client.chars().any(char::is_control),
        "clientが不正"
    );
    // 他の書込がplan再確認とcommitの間へ入らないよう、最初にwriter lockを取る。
    let changed_notes = (|| -> Result<usize> {
        let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        let exists =
            crate::tag_vocabulary_rollback::operation_id_exists(&transaction, execution_id)?;
        ensure!(
            !exists,
            "この語彙変更は実行済み。履歴を確認する: {execution_id}"
        );
        ensure!(
            crate::note_store::pending_count(&transaction)? == 0
                && crate::tag_vocabulary_source::pending_count(&transaction)? == 0,
            "未出力の更新があるため語彙変更を開始できない。出力を解消して再計画する"
        );
        crate::tag_vocabulary_source::ensure_workspace(vault, &transaction)?;
        let (current, mut prepared) = prepare(&transaction, &receipt.changes, &receipt.reason)?;
        ensure!(
            &current.receipt == receipt,
            "語彙変更planが現在のsnapshotと一致しない。plan_tag_vocabulary_changeからやり直す"
        );
        ensure!(
            current.can_apply,
            "語彙変更を適用できない。planのblockersを解消する"
        );
        let at = now_iso();
        let mut history = Vec::with_capacity(prepared.len());
        // 一括変更もノートへの書込。1件ずつ来歴イベントを発行し、迂回経路を作らない。
        let actor = crate::provenance::WriteActor::from_client_hint(client);
        let revision = crate::provenance::RevisionInput {
            kind: Some(crate::provenance::RevisionKind::Normalize),
            summary: Some(receipt.reason.clone()),
            ..crate::provenance::RevisionInput::default()
        };
        let context = crate::provenance::WriteContext {
            actor: &actor,
            revision: Some(&revision),
            operation: crate::provenance::Operation::Update,
        };
        // prepareは正本を最後へ配置する。利用タグを先に更新するので通常の削除guardも通る。
        for (i, item) in prepared.iter_mut().enumerate() {
            item.after.front.generated = Some(Generated {
                by: client.to_string(),
                at: at.clone(),
            });
            crate::note_store::queue_put(
                vault,
                &transaction,
                &item.id,
                &item.after,
                &format!("tag-vocabulary:{execution_id}:{i}"),
                crate::note_store::WriteAttribution::new(
                    &format!(
                        "**Tag vocabulary**: {}件を一括変更。実行 `{execution_id}`。理由: {}",
                        current.changed_notes, receipt.reason
                    ),
                    &format!(
                        "tags: 語彙と{}件を一括変更 ({execution_id}, via {client})",
                        current.changed_notes
                    ),
                    &context,
                ),
            )?;
            history.push(HistoryNote {
                note_id: item.id.clone(),
                note_uid: item.after.front.note_uid.as_ref().map(ToString::to_string),
                before_document: std::mem::take(&mut item.before),
                after_document: item.after.to_file_string()?,
            });
        }
        crate::index::validate_authority_index(&transaction)?;
        crate::tag_vocabulary_history::record(
            &transaction,
            &HistoryRun {
                execution_id: execution_id.to_string(),
                workspace_id: receipt.workspace_id.clone(),
                source_note_uid: receipt.source_note_uid.clone(),
                source_revision: receipt.source_revision.clone(),
                plan_hash: receipt.plan_hash.clone(),
                operations_json: serde_json::to_string(&receipt.changes)?,
                reason: receipt.reason.clone(),
                client: client.to_string(),
                applied_at: at,
                changed_notes: history.len(),
            },
            &history,
        )?;
        transaction.commit()?;
        Ok(current.changed_notes)
    })()
    .map_err(crate::write_rejection::confirm_before_write)?;
    let (pending_exports, markdown_export_error) = export_result(vault, conn);
    Ok(ApplyResult {
        execution_id: execution_id.to_string(),
        stored: true,
        changed_notes,
        pending_exports,
        markdown_export_error,
    })
}

/// 確定後の取得エラーも未保存へ変換しない。適用と復元で同じ応答境界を使う。
pub(crate) fn export_result(vault: &Vault, conn: &Connection) -> (Option<usize>, Option<String>) {
    let mut markdown_export_error = vault
        .flush_note_exports(conn)
        .err()
        .map(|error| format!("{error:#}"));
    let pending_exports = match crate::note_store::pending_count(conn) {
        Ok(count) => Some(count),
        Err(error) => {
            let warning = format!("保存済みだが出力待ち件数を確認できない: {error:#}");
            markdown_export_error = Some(match markdown_export_error {
                Some(previous) => format!("{previous}; {warning}"),
                None => warning,
            });
            None
        }
    };
    (pending_exports, markdown_export_error)
}

fn normalize_changes(changes: &Changes, reason: &str) -> Result<Changes> {
    ensure!(
        !reason.is_empty()
            && reason.trim() == reason
            && reason.chars().count() <= 500
            && !reason.chars().any(char::is_control),
        "語彙変更の理由は1〜500文字の一行で指定する"
    );
    let count = changes.upsert.len() + changes.remove.len() + changes.replace.len();
    ensure!(
        (1..=MAX_OPERATIONS).contains(&count),
        "語彙変更は1〜{MAX_OPERATIONS}操作で指定する"
    );
    let mut normalized = changes.clone();
    normalized.remove.sort();
    let mut operated = BTreeSet::new();
    for tag in changes
        .upsert
        .keys()
        .chain(changes.remove.iter())
        .chain(changes.replace.keys())
    {
        crate::tags::validate_shape(tag)?;
        ensure!(operated.insert(tag), "同じ語への操作が重複している: {tag}");
    }
    for description in changes.upsert.values() {
        ensure!(
            !description.is_empty()
                && description.trim() == description
                && description.chars().count() <= 500
                && !description.contains('|')
                && !description.chars().any(char::is_control),
            "語彙の説明は1〜500文字の一行で指定し、表区切りの | を含めない"
        );
    }
    for (from, to) in &changes.replace {
        crate::tags::validate_shape(to)?;
        ensure!(
            from != to && !changes.replace.contains_key(to),
            "タグ置換の自己参照・連鎖・循環は使わない"
        );
        ensure!(
            !changes.remove.contains(to),
            "削除するタグへ置換できない: {to}"
        );
    }
    Ok(normalized)
}

/// hashが自己整合でも、タグ以外の編集を一括変更履歴として復元してはならない。
pub(crate) fn validate_recorded_change(run: &HistoryRun, notes: &[HistoryNote]) -> Result<()> {
    let changes: Changes = serde_json::from_str(&run.operations_json)?;
    let changes = normalize_changes(&changes, &run.reason)?;
    let source = notes
        .iter()
        .find(|note| note.note_uid.as_deref() == Some(run.source_note_uid.as_str()))
        .context("語彙変更履歴に正本がない")?;
    let before = Note::parse(&source.before_document)?;
    let previous = crate::tags::parse_glossary(source.note_id.clone(), &before.body).entries;
    let mut entries = previous.clone();
    for tag in changes.remove.iter().chain(changes.replace.keys()) {
        ensure!(entries.remove(tag).is_some(), "履歴の変更元が語彙表にない");
    }
    entries.extend(changes.upsert.clone());
    ensure!(entries != previous, "履歴の語彙表に実変更がない");
    ensure!(
        changes
            .replace
            .values()
            .all(|tag| entries.contains_key(tag)),
        "履歴の置換先が語彙表にない"
    );
    let source_body = glossary::rewrite(&before.body, &entries)?;
    for stored in notes {
        let mut expected = Note::parse(&stored.before_document)?;
        let affected = stored.note_id == source.note_id
            || expected
                .front
                .tags
                .iter()
                .any(|tag| changes.replace.contains_key(tag));
        ensure!(affected, "履歴に語彙変更の対象外ノートがある");
        ensure!(
            !expected
                .front
                .tags
                .iter()
                .any(|tag| changes.remove.contains(tag)),
            "履歴が使用中語の削除を含む"
        );
        let mut seen = BTreeSet::new();
        expected.front.tags = expected
            .front
            .tags
            .iter()
            .map(|tag| changes.replace.get(tag).unwrap_or(tag).clone())
            .filter(|tag| seen.insert(tag.clone()))
            .collect();
        if stored.note_id == source.note_id {
            expected.body = source_body.clone();
        }
        expected.front.generated = Some(Generated {
            by: run.client.clone(),
            at: run.applied_at.clone(),
        });
        ensure!(
            expected.to_file_string()? == stored.after_document,
            "履歴の前後原文が記録された語彙操作と一致しない"
        );
    }
    Ok(())
}

fn prepare(
    conn: &Connection,
    changes: &Changes,
    reason: &str,
) -> Result<(Plan, Vec<PreparedNote>)> {
    let changes = normalize_changes(changes, reason)?;
    let overview = crate::tags::vocabulary_overview(conn)?;
    ensure!(
        overview.source_status == crate::tags::SourceStatus::Pinned,
        "語彙変更には利用可能な正本の明示指定が必要。tag_vocabularyを確認する"
    );
    let source = overview.source.context("語彙正本の指定がない")?;
    let source_id = overview.glossary_note.context("語彙正本のノートがない")?;
    let source_document: String = conn.query_row(
        "SELECT document FROM notes WHERE id=?1",
        [&source_id],
        |r| r.get(0),
    )?;
    let source_note = Note::parse(&source_document)?;
    let mut entries = overview.entries.clone();
    for tag in changes.remove.iter().chain(changes.replace.keys()) {
        ensure!(
            entries.remove(tag).is_some(),
            "変更元のタグが語彙表にない: {tag}"
        );
    }
    for (tag, desc) in &changes.upsert {
        entries.insert(tag.clone(), desc.clone());
    }
    for target in changes.replace.values() {
        ensure!(
            entries.contains_key(target),
            "置換先を最終語彙へ登録する: {target}"
        );
    }
    ensure!(entries != overview.entries, "語彙表に実変更がない");
    let new_body = glossary::rewrite(&source_note.body, &entries)?;
    let mut snapshot = Sha256::new();
    let mut stmt =
        conn.prepare("SELECT id, document, tags, normal_reference_allowed FROM notes ORDER BY id")?;
    let mut rows = stmt.query([])?;
    let mut prepared = Vec::new();
    let mut source_prepared = None;
    let mut examples = Vec::new();
    let mut blockers = BTreeMap::<String, usize>::new();
    let mut changed_notes = 0;
    let mut document_bytes = 0usize;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let document: String = row.get(1)?;
        let indexed_tags: String = row.get(2)?;
        let allowed: bool = row.get(3)?;
        for value in [&id, &document, &indexed_tags] {
            snapshot.update((value.len() as u64).to_le_bytes());
            snapshot.update(value.as_bytes());
        }
        snapshot.update([u8::from(allowed)]);
        let affected = id == source_id
            || indexed_tags.split_whitespace().any(|tag| {
                changes.replace.contains_key(tag) || changes.remove.iter().any(|item| item == tag)
            });
        if !affected {
            continue;
        }
        changed_notes += 1;
        ensure!(
            changed_notes <= MAX_CHANGED_NOTES,
            "対象が{MAX_CHANGED_NOTES}件を超えるため適用しない"
        );
        document_bytes = document_bytes.saturating_add(document.len());
        ensure!(
            document_bytes <= MAX_DOCUMENT_BYTES,
            "変更対象の原文が256 MiBを超えるため適用しない"
        );
        let mut after = Note::parse(&document)?;
        let reference_allowed =
            allowed && crate::proposal_workflow::derive_normal_reference_allowed(&after);
        ensure!(
            after.front.tags.join(" ") == indexed_tags,
            "対象ノートのタグ索引と原文が一致しない"
        );
        let before_tags = after.front.tags.clone();
        let mut seen = BTreeSet::new();
        after.front.tags = before_tags
            .iter()
            .map(|tag| changes.replace.get(tag).unwrap_or(tag).clone())
            .filter(|tag| seen.insert(tag.clone()))
            .collect();
        if id == source_id {
            after.body = new_body.clone();
        }
        let problem = if before_tags.iter().any(|tag| changes.remove.contains(tag)) {
            Some("removed_tag_in_use")
        } else if after.front.origin.as_deref() != Some("agent") {
            Some("protected_origin")
        } else if !reference_allowed || after.front.extra.contains_key("proposal_ticket") {
            Some("protected_proposal")
        } else if crate::tags::validate_structure(&after.front.tags).is_err() {
            Some("invalid_tag_structure")
        } else if after
            .front
            .tags
            .iter()
            .any(|tag| !entries.contains_key(tag))
        {
            Some("unknown_final_tag")
        } else {
            None
        };
        if let Some(problem) = problem {
            *blockers.entry(problem.to_string()).or_default() += 1;
        }
        if reference_allowed && examples.len() < SAMPLE_LIMIT {
            examples.push(Example {
                note: id.clone(),
                note_uid: after.front.note_uid.as_ref().map(ToString::to_string),
                title: after.front.title.clone(),
                before_tags,
                after_tags: after.front.tags.clone(),
            });
        }
        let item = PreparedNote {
            id: id.clone(),
            before: document,
            after,
        };
        if id == source_id {
            source_prepared = Some(item);
        } else {
            prepared.push(item);
        }
    }
    prepared.push(source_prepared.context("語彙正本がsnapshotにない")?);
    let mut receipt = Receipt {
        schema: 1,
        workspace_id: source.workspace_id,
        source_note_uid: source.note_uid.to_string(),
        source_revision: source.revision,
        source_document_hash: crate::distillation::sha256(source_document.as_bytes()),
        snapshot_digest: format!("sha256:{:x}", snapshot.finalize()),
        changes,
        reason: reason.to_string(),
        plan_hash: String::new(),
    };
    receipt.plan_hash = crate::distillation::sha256(&serde_json::to_vec(&receipt)?);
    let blockers_count = blockers.values().sum();
    Ok((
        Plan {
            receipt,
            changed_notes,
            examples,
            blockers_count,
            blockers: blockers
                .into_iter()
                .map(|(code, count)| Blocker { code, count })
                .collect(),
            can_apply: blockers_count == 0,
        },
        prepared,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::NoteUid;

    struct Fixture {
        _dir: tempfile::TempDir,
        vault: Vault,
        conn: Connection,
        source: String,
        target: String,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        let source = vault.propose_for_test("語彙の正本", "# 運用\nAI判断で統合する。\n\n## 語彙\n| タグ | 説明 |\n| --- | --- |\n| old | 旧語 |\n| new | 新語 |\n| unused | 未使用 |\n\n## 計測\nこの記録を残す。", None, &["old".into()], "test").unwrap();
        let target = vault
            .propose_for_test(
                "対象ノート",
                "残す本文と[リンク](https://example.com)。",
                Some("残す説明"),
                &["old".into(), "new".into()],
                "test",
            )
            .unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        crate::tag_vocabulary_source::pin_for_test(&vault, &conn, &source).unwrap();
        Fixture {
            _dir: dir,
            vault,
            conn,
            source,
            target,
        }
    }

    fn merge() -> Changes {
        Changes {
            upsert: BTreeMap::new(),
            remove: Vec::new(),
            replace: BTreeMap::from([("old".into(), "new".into())]),
        }
    }

    fn document(conn: &Connection, id: &str) -> String {
        conn.query_row("SELECT document FROM notes WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .unwrap()
    }

    fn receipt(f: &Fixture) -> Receipt {
        plan(&f.conn, &merge(), "重複語を統合する").unwrap().receipt
    }

    fn run_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT count(*) FROM tag_vocabulary_runs", [], |row| {
            row.get(0)
        })
        .unwrap()
    }

    /// 2026-09-08: 正本自身が統合元タグを使っていても、表と自身を同時変更する。
    #[test]
    fn merge_keeps_content_identity_and_metadata_and_exports_one_commit() {
        let f = fixture();
        let before = crate::note_store::read(&f.conn, &f.target).unwrap();
        let repo = git2::Repository::open(&f.vault.root).unwrap();
        let before_commit = repo.head().unwrap().target().unwrap();
        let id = NoteUid::new().to_string();
        let receipt = receipt(&f);
        let result = apply(&f.vault, &f.conn, &id, &receipt, Some("test/bulk")).unwrap();
        assert!(result.stored);
        assert_eq!(result.changed_notes, 2);
        assert_eq!(result.pending_exports, Some(0));
        let after = crate::note_store::read(&f.conn, &f.target).unwrap();
        let mut preserved = before.clone();
        preserved.front.tags = vec!["new".into()];
        preserved.front.generated = after.front.generated.clone();
        assert_eq!(
            preserved.to_file_string().unwrap(),
            after.to_file_string().unwrap()
        );
        let source = crate::note_store::read(&f.conn, &f.source).unwrap();
        assert!(source.body.starts_with("# 運用\nAI判断で統合する。\n"));
        assert!(source.body.ends_with("## 計測\nこの記録を残す。\n"));
        assert_eq!(source.front.tags, vec!["new"]);
        assert!(!crate::tags::vocabulary(&f.conn).unwrap().contains("old"));
        assert_eq!(
            repo.head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .parent_id(0)
                .unwrap(),
            before_commit
        );
        assert_eq!(run_count(&f.conn), 1);
        assert!(apply(&f.vault, &f.conn, &id, &receipt, Some("test/bulk")).is_err());
        assert!(
            apply(
                &f.vault,
                &f.conn,
                &NoteUid::new().to_string(),
                &receipt,
                None
            )
            .is_err()
        );
        crate::tag_vocabulary_history::verify_integrity(&f.conn).unwrap();
    }

    #[test]
    fn additions_descriptions_and_unused_removal_are_structured_changes() {
        let f = fixture();
        let changes = Changes {
            upsert: BTreeMap::from([
                ("added".into(), "追加した語".into()),
                ("new".into(), "更新した説明".into()),
            ]),
            remove: vec!["unused".into()],
            replace: BTreeMap::new(),
        };
        let p = plan(&f.conn, &changes, "説明更新と不要語整理").unwrap();
        assert_eq!(p.changed_notes, 1);
        apply(
            &f.vault,
            &f.conn,
            &NoteUid::new().to_string(),
            &p.receipt,
            None,
        )
        .unwrap();
        let overview = crate::tags::vocabulary_overview(&f.conn).unwrap();
        assert_eq!(overview.entries.get("new").unwrap(), "更新した説明");
        assert!(overview.entries.contains_key("added"));
        assert!(!overview.entries.contains_key("unused"));
    }

    /// 2026-09-08: 最後の正本writeが失敗しても、先に変更したノート・索引・履歴を残さない。
    #[test]
    fn late_failure_rolls_back_documents_indices_outbox_and_history() {
        let f = fixture();
        let before = crate::index::test_support::durable_rows_snapshot(&f.conn);
        let receipt = receipt(&f);
        f.conn.execute_batch("CREATE TRIGGER fixture_fail BEFORE UPDATE ON notes WHEN NEW.title='語彙の正本' BEGIN SELECT RAISE(FAIL,'fixture failure'); END;").unwrap();
        assert!(
            apply(
                &f.vault,
                &f.conn,
                &NoteUid::new().to_string(),
                &receipt,
                None
            )
            .is_err()
        );
        assert_eq!(
            before,
            crate::index::test_support::durable_rows_snapshot(&f.conn)
        );
        assert_eq!(run_count(&f.conn), 0);
        assert_eq!(crate::note_store::pending_count(&f.conn).unwrap(), 0);
        assert!(crate::tags::vocabulary(&f.conn).unwrap().contains("old"));
        crate::index::validate_authority_index(&f.conn).unwrap();
    }

    #[test]
    fn concurrent_body_or_new_usage_and_receipt_tampering_are_stale() {
        let f = fixture();
        let mut r = receipt(&f);
        r.reason = "別の理由".into();
        assert!(apply(&f.vault, &f.conn, &NoteUid::new().to_string(), &r, None).is_err());
        let r = receipt(&f);
        let mut after = crate::note_store::read(&f.conn, &f.target).unwrap();
        after.body.push_str("後続の更新\n");
        crate::index::upsert(&f.conn, &f.vault, &f.target, 1, &after).unwrap();
        assert!(apply(&f.vault, &f.conn, &NoteUid::new().to_string(), &r, None).is_err());
        let r = receipt(&f);
        after.front.note_uid = Some(NoteUid::new());
        crate::index::upsert(&f.conn, &f.vault, "notes/new-usage", 1, &after).unwrap();
        assert!(apply(&f.vault, &f.conn, &NoteUid::new().to_string(), &r, None).is_err());
        assert_eq!(run_count(&f.conn), 0);
    }

    #[test]
    fn used_removal_protected_notes_and_invalid_final_tags_block_all() {
        for variant in ["remove", "human", "hidden", "unknown", "five"] {
            let f = fixture();
            let mut changes = merge();
            if variant == "remove" {
                changes = Changes {
                    upsert: BTreeMap::new(),
                    remove: vec!["old".into()],
                    replace: BTreeMap::new(),
                };
            } else {
                let mut note = crate::note_store::read(&f.conn, &f.target).unwrap();
                if variant == "human" {
                    note.front.origin = Some("human".into());
                }
                if variant == "unknown" {
                    note.front.tags.push("outside".into());
                }
                if variant == "five" {
                    note.front.tags.extend([
                        "one".into(),
                        "two".into(),
                        "three".into(),
                        "four".into(),
                    ]);
                }
                crate::index::upsert(&f.conn, &f.vault, &f.target, 1, &note).unwrap();
                if variant == "hidden" {
                    f.conn
                        .execute(
                            "UPDATE notes SET normal_reference_allowed=0 WHERE id=?1",
                            [&f.target],
                        )
                        .unwrap();
                }
            }
            let p = plan(&f.conn, &changes, "不成立の計画").unwrap();
            assert!(!p.can_apply, "{variant}");
            if variant == "hidden" {
                assert!(!serde_json::to_string(&p).unwrap().contains(&f.target));
            }
            assert!(
                apply(
                    &f.vault,
                    &f.conn,
                    &NoteUid::new().to_string(),
                    &p.receipt,
                    None
                )
                .is_err()
            );
            assert_eq!(run_count(&f.conn), 0);
        }
    }

    #[test]
    fn export_conflict_keeps_saved_history_and_retries_the_whole_group() {
        let f = fixture();
        let before_source = std::fs::read_to_string(f.vault.note_path(&f.source).unwrap()).unwrap();
        let mut external = f.vault.read_note(&f.target).unwrap();
        external.body.push_str("外部編集\n");
        f.vault.write_note_fixture(&f.target, &external).unwrap();
        let result = apply(
            &f.vault,
            &f.conn,
            &NoteUid::new().to_string(),
            &receipt(&f),
            None,
        )
        .unwrap();
        assert!(result.stored && result.markdown_export_error.is_some());
        assert_eq!(result.pending_exports, Some(2));
        assert_eq!(
            std::fs::read_to_string(f.vault.note_path(&f.source).unwrap()).unwrap(),
            before_source
        );
        assert_eq!(run_count(&f.conn), 1);
        let conflict = f
            .vault
            .inspect_markdown_export_conflict(&f.conn, &f.target)
            .unwrap();
        f.vault
            .resolve_markdown_export_keep_db(
                &f.conn,
                &f.target,
                &conflict.markdown_hash,
                &conflict.pending_document_hash,
            )
            .unwrap();
        assert_eq!(crate::note_store::pending_count(&f.conn).unwrap(), 0);
        let log = std::fs::read_to_string(f.vault.root.join("log.md")).unwrap();
        assert_eq!(log.matches("**Tag vocabulary**").count(), 1);
        assert_eq!(f.vault.flush_note_exports(&f.conn).unwrap(), 0);
    }

    #[test]
    fn interrupted_export_after_files_are_written_is_idempotent() {
        let f = fixture();
        let lock = f.vault.root.join(".git/index.lock");
        std::fs::write(&lock, "fixture").unwrap();
        let result = apply(
            &f.vault,
            &f.conn,
            &NoteUid::new().to_string(),
            &receipt(&f),
            None,
        )
        .unwrap();
        assert!(result.stored && result.markdown_export_error.is_some());
        assert_eq!(result.pending_exports, Some(2));
        std::fs::remove_file(lock).unwrap();
        f.vault.flush_note_exports(&f.conn).unwrap();
        let log = std::fs::read_to_string(f.vault.root.join("log.md")).unwrap();
        assert_eq!(log.matches("**Tag vocabulary**").count(), 1);
    }

    fn add_targets(f: &Fixture, count: usize) {
        let template = crate::note_store::read(&f.conn, &f.target).unwrap();
        let tx = f.conn.unchecked_transaction().unwrap();
        for i in 0..count {
            let mut note = template.clone();
            note.front.note_uid = Some(NoteUid::new());
            note.front.title = Some(format!("性能fixture {i}"));
            crate::index::upsert(&tx, &f.vault, &format!("notes/bulk-{i:05}"), 1, &note).unwrap();
        }
        tx.commit().unwrap();
    }

    #[test]
    fn previews_are_bounded_and_legacy_uid_is_preserved() {
        let f = fixture();
        add_targets(&f, 24);
        let mut note = crate::note_store::read(&f.conn, &f.target).unwrap();
        note.front.note_uid = None;
        note.front.authority = None;
        crate::index::upsert(&f.conn, &f.vault, "notes/legacy", 1, &note).unwrap();
        let p = plan(&f.conn, &merge(), "規模を確認する").unwrap();
        assert_eq!(p.changed_notes, 27);
        assert_eq!(p.examples.len(), SAMPLE_LIMIT);
        apply(
            &f.vault,
            &f.conn,
            &NoteUid::new().to_string(),
            &p.receipt,
            None,
        )
        .unwrap();
        assert!(
            crate::note_store::read(&f.conn, "notes/legacy")
                .unwrap()
                .front
                .note_uid
                .is_none()
        );
        assert!(!document(&f.conn, "notes/legacy").contains("note_uid:"));
    }

    #[test]
    #[ignore = "1万件の原文・履歴・Markdown出力まで計測する明示実行用"]
    fn bulk_ten_thousand_notes_are_applied_with_bounded_receipt() {
        let f = fixture();
        add_targets(&f, 9_998);
        let document_digest = || {
            let mut hash = Sha256::new();
            let mut stmt = f
                .conn
                .prepare("SELECT id,document FROM notes ORDER BY id")
                .unwrap();
            for row in stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .unwrap()
            {
                let (id, document) = row.unwrap();
                for part in [id, document] {
                    hash.update((part.len() as u64).to_le_bytes());
                    hash.update(part.as_bytes());
                }
            }
            format!("{:x}", hash.finalize())
        };
        let before_digest = document_digest();
        let start = std::time::Instant::now();
        let p = plan(&f.conn, &merge(), "1万件の語彙を統合する").unwrap();
        let plan_elapsed = start.elapsed();
        assert_eq!(p.changed_notes, 10_000);
        assert!(serde_json::to_vec(&p).unwrap().len() < 20_000);
        let start = std::time::Instant::now();
        let result = apply(
            &f.vault,
            &f.conn,
            &NoteUid::new().to_string(),
            &p.receipt,
            None,
        )
        .unwrap();
        assert_eq!(result.changed_notes, 10_000);
        assert_eq!(result.pending_exports, Some(0));
        assert_eq!(run_count(&f.conn), 1);
        eprintln!(
            "tag vocabulary 10k: plan={plan_elapsed:?}, apply+history+export={:?}, response_bytes={}",
            start.elapsed(),
            serde_json::to_vec(&p).unwrap().len()
        );
        let start = std::time::Instant::now();
        let rollback_plan = crate::tag_vocabulary_rollback::plan(
            &f.conn,
            &result.execution_id,
            "一括復元の性能と原文一致を検証する",
        )
        .unwrap();
        let plan_elapsed = start.elapsed();
        assert!(rollback_plan.can_apply);
        assert_eq!(rollback_plan.restored_notes, 10_000);
        assert!(serde_json::to_vec(&rollback_plan).unwrap().len() < 20_000);
        let start = std::time::Instant::now();
        let restored = crate::tag_vocabulary_rollback::rollback(
            &f.vault,
            &f.conn,
            &NoteUid::new().to_string(),
            &rollback_plan.receipt,
            None,
        )
        .unwrap();
        let rollback_elapsed = start.elapsed();
        assert!(restored.stored);
        assert_eq!(restored.restored_notes, 10_000);
        assert_eq!(restored.pending_exports, Some(0));
        assert_eq!(document_digest(), before_digest);
        let stats = crate::tag_vocabulary_stats::read(&f.conn).unwrap();
        assert_eq!(
            (
                stats.apply_runs,
                stats.rollback_runs,
                stats.restored_note_changes
            ),
            (1, 1, 10_000)
        );
        eprintln!(
            "tag vocabulary rollback 10k: plan={plan_elapsed:?}, rollback+history+export={rollback_elapsed:?}, response_bytes={}",
            serde_json::to_vec(&rollback_plan).unwrap().len()
        );
    }
}
