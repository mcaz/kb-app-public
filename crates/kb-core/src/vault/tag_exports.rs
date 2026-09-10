//! 語彙変更の出力単位を保ち、N件の索引全走査・Git commitを1回へ畳む。

use std::collections::BTreeSet;

use anyhow::{Context, Result, ensure};
use rusqlite::Connection;

use crate::frontmatter::Note;
use crate::note_id::NoteId;
use crate::note_store::{ExportOperation, PendingExport};
use crate::vault::Vault;

pub(super) fn execution_id(operation_id: &str) -> Option<&str> {
    let (id, seq) = operation_id
        .strip_prefix("tag-vocabulary:")?
        .split_once(':')?;
    (crate::artifact::is_ulid(id) && seq.parse::<usize>().is_ok()).then_some(id)
}

impl Vault {
    pub(super) fn flush_tag_exports(
        &self,
        conn: &Connection,
        id: &str,
        exports: &[PendingExport],
    ) -> Result<()> {
        let first = exports.first().context("一括語彙変更の出力がない")?;
        let mut note_ids = BTreeSet::new();
        let mut paths = Vec::with_capacity(exports.len() + 2);
        // 開始前に全対象を検査する。中断後は各ファイルのbefore/afterどちらも受理する。
        for export in exports {
            ensure!(
                export.operation == ExportOperation::Upsert
                    && export.commit_message == first.commit_message
                    && export.log_entry == first.log_entry
                    && note_ids.insert(&export.note_id),
                "一括語彙変更outboxの構成が不正"
            );
            let note_id = NoteId::parse(&export.note_id)?;
            let document = export
                .document
                .as_deref()
                .context("語彙変更outboxに原文がない")?;
            Note::parse(document)?;
            self.ensure_export_base(&note_id, export.base_document.as_deref(), Some(document))?;
            paths.push(
                note_id
                    .markdown_relative_path()
                    .to_str()
                    .context("ノートIDがUTF-8ではない")?
                    .to_string(),
            );
        }
        // 来歴イベントは正本(.kb-events)へ追記してから、同じcommitへ載せる。
        // 旧版が積んだexportにはイベントが無いので、その場合は従来どおり処理する。
        let mut events = Vec::with_capacity(exports.len());
        for export in exports {
            let note_id = NoteId::parse(&export.note_id)?;
            let document = export
                .document
                .as_deref()
                .context("語彙変更outboxに原文がない")?;
            self.ensure_export_base(&note_id, export.base_document.as_deref(), Some(document))?;
            self.write_note(note_id.as_str(), &Note::parse(document)?)?;
            if let Some(event) = crate::provenance::event_by_id(conn, &export.op_id)? {
                crate::provenance::append_event(&self.root, &event)?;
                let shard = crate::provenance::shard_relative_path(&event.at);
                if !paths.contains(&shard) {
                    paths.push(shard);
                }
                events.push(event);
            }
        }
        self.append_log_once(&format!("tag-vocabulary:{id}"), &first.log_entry)?;
        self.write_index_md()?;
        paths.extend(["index.md".into(), "log.md".into()]);
        self.commit(
            &paths.iter().map(String::as_str).collect::<Vec<_>>(),
            &first.commit_message,
        )?;
        // commit後にクラッシュしても再出力は同じ内容・log markerなので冪等。
        let transaction = conn.unchecked_transaction()?;
        for export in exports {
            crate::note_store::complete(&transaction, export.seq)?;
        }
        for event in &events {
            crate::provenance::mark_exported(&transaction, &event.event_id)?;
        }
        transaction.commit()?;
        Ok(())
    }
}
