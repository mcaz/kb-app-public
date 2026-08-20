//! 実行時ノートストア+派生索引(SQLite 1ファイル)。二本立て FTS(lindera 主索引+
//! trigram レスキュー)+リンクテーブル。Markdownは明示import・復元時だけ読み込む。
//! 接続規律(busy_timeout 必須・WAL)は docs/poc-report.md の PoC ③ 由来。

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::frontmatter::Note;
use crate::tokenize::wakati;
use crate::vault::Vault;

const SCHEMA_VERSION: &str = "6";

pub fn open_db(vault: &Vault) -> Result<Connection> {
    let conn = open_db_recovery(vault)?;
    restore_missing_documents(vault, &conn)?;
    if !runtime_store_is_db(&conn) {
        let report = import_markdown_snapshot(vault, &conn)?;
        if !report.degraded.is_empty() {
            bail!(
                "MarkdownバックアップからDBを復元できない: {}",
                report
                    .degraded
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" / ")
            );
        }
    }
    Ok(conn)
}

/// Markdown import/exportが衝突して通常起動できない場合の限定的な復旧接続。
/// schema準備だけを行い、Markdown restore/importやoutbox flushは呼ばない。
pub fn open_db_recovery(vault: &Vault) -> Result<Connection> {
    let path = vault.index_db_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(&path).context("index.db open")?;
    conn.busy_timeout(Duration::from_secs(5))?; // 全接続で必須(PoC ③)
    let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    debug_assert_eq!(mode.to_lowercase(), "wal");
    init_schema(&conn)?;
    Ok(conn)
}

/// plannerなど「観測だけ」の操作向け。schema作成・migration・Markdown復元・WAL設定を
/// 行わず、既存DBをSQLiteのread-only + query_onlyで開く。
pub fn open_db_read_only(vault: &Vault) -> Result<Connection> {
    let path = vault.index_db_path();
    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("read-only index.dbを開けない: {}", path.display()))?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA query_only=ON")?;
    let schema: String = conn
        .query_row("SELECT value FROM meta WHERE key='schema'", [], |row| {
            row.get(0)
        })
        .context("read-only plannerが必要とするDB schemaを確認できない")?;
    if schema != SCHEMA_VERSION {
        bail!("read-only plannerはschema {SCHEMA_VERSION}の準備済みDBを必要とする(現在: {schema})")
    }
    Ok(conn)
}

/// 2026-08-20に、既存v3 DBへ`document`列を追加しただけでは空文字の行が残り、
/// FTS検索には出るのに詳細取得だけ失敗する事故が起きた。通常のMarkdown importは
/// parse失敗をfail-openで続行するため、この復旧だけは対象を先に全件読み切り、
/// 1 transactionで全件または0件に固定する。
fn restore_missing_documents(vault: &Vault, conn: &Connection) -> Result<usize> {
    let transaction = conn.unchecked_transaction()?;
    let missing = {
        let mut statement =
            transaction.prepare("SELECT id FROM notes WHERE document = '' ORDER BY id")?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    if missing.is_empty() {
        transaction.commit()?;
        return Ok(0);
    }
    if crate::note_store::pending_count(&transaction)? != 0 {
        bail!("未出力のDB更新があるため空のDB本文を復元できない");
    }

    let paths: HashMap<String, std::path::PathBuf> = vault.list_note_files()?.into_iter().collect();
    let mut absent = BTreeSet::new();
    let mut recovered = Vec::with_capacity(missing.len());
    for id in &missing {
        let Some(path) = paths.get(id) else {
            absent.insert(id.clone());
            continue;
        };
        let modified = fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .and_then(|modified| {
                modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(std::io::Error::other)
            })
            .with_context(|| format!("DB本文の復元元metadataを読めない: {id}"))?;
        let mtime = i64::try_from(modified.as_nanos())
            .with_context(|| format!("DB本文の復元元mtimeがSQLite INTEGERに収まらない: {id}"))?;
        let document = fs::read_to_string(path)
            .with_context(|| format!("DB本文の復元元Markdownを読めない: {id}"))?;
        let note = Note::parse(&document)
            .with_context(|| format!("DB本文の復元元Markdownをparseできない: {id}"))?;
        recovered.push((id.clone(), mtime, note));
    }
    if !absent.is_empty() {
        bail!(
            "DB本文の復元元Markdownが見つからない: {}",
            absent.into_iter().collect::<Vec<_>>().join(", ")
        );
    }

    for (id, mtime, note) in &recovered {
        upsert(&transaction, vault, id, *mtime, note)?;
    }
    let remaining: i64 = transaction.query_row(
        "SELECT count(*) FROM notes WHERE document = ''",
        [],
        |row| row.get(0),
    )?;
    if remaining != 0 {
        bail!("空のDB本文が復元後も残っている: {remaining}件");
    }
    transaction.commit()?;
    Ok(recovered.len())
}

fn init_schema(conn: &Connection) -> Result<()> {
    let ver: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key='schema'", [], |r| {
            r.get(0)
        })
        .ok();
    if matches!(ver.as_deref(), Some("3" | "4" | "5" | SCHEMA_VERSION)) {
        // 追加カラムの後方互換マイグレーション(破壊的な作り直しをしない —
        // 全テーブル再作成は埋め込みの再計算嵐を起こすため)
        for (col, ddl) in [
            ("tags", "ALTER TABLE notes ADD COLUMN tags TEXT DEFAULT ''"),
            ("created", "ALTER TABLE notes ADD COLUMN created TEXT"),
            (
                "document",
                "ALTER TABLE notes ADD COLUMN document TEXT NOT NULL DEFAULT ''",
            ),
            ("note_uid", "ALTER TABLE notes ADD COLUMN note_uid TEXT"),
            ("namespace", "ALTER TABLE notes ADD COLUMN namespace TEXT"),
            (
                "authority_role",
                "ALTER TABLE notes ADD COLUMN authority_role TEXT",
            ),
            (
                "authority_status",
                "ALTER TABLE notes ADD COLUMN authority_status TEXT",
            ),
            (
                "authority_scope",
                "ALTER TABLE notes ADD COLUMN authority_scope TEXT",
            ),
        ] {
            if conn
                .prepare(&format!("SELECT {col} FROM notes LIMIT 0"))
                .is_err()
            {
                // 破壊的な作り直しをしない(埋め込み再計算の嵐を避ける)
                conn.execute_batch(&format!("{ddl}; UPDATE notes SET mtime = -1;"))?;
            }
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS links_dst ON links(dst);
             CREATE UNIQUE INDEX IF NOT EXISTS notes_note_uid ON notes(note_uid)
                 WHERE note_uid IS NOT NULL;
             CREATE UNIQUE INDEX IF NOT EXISTS notes_active_canonical_scope
                 ON notes(namespace, authority_scope)
                 WHERE authority_role = 'canonical' AND authority_status = 'active';
             CREATE TABLE IF NOT EXISTS note_relations(
                 src_uid TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 target_uid TEXT NOT NULL,
                 PRIMARY KEY(src_uid, kind, target_uid)
             );
             CREATE INDEX IF NOT EXISTS note_relations_target ON note_relations(target_uid);
             CREATE TABLE IF NOT EXISTS note_exports(
                 seq INTEGER PRIMARY KEY AUTOINCREMENT,
                 op_id TEXT NOT NULL UNIQUE,
                 note_id TEXT NOT NULL,
                 operation TEXT NOT NULL,
                 base_document TEXT,
                 document TEXT,
                 log_entry TEXT NOT NULL,
                 commit_message TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS distillation_runs(
                 execution_id TEXT PRIMARY KEY,
                 plan_id TEXT NOT NULL,
                 before_snapshot_digest TEXT NOT NULL,
                 after_snapshot_digest TEXT NOT NULL,
                 request_json TEXT NOT NULL,
                 before_documents TEXT NOT NULL,
                 after_documents TEXT NOT NULL,
                 client TEXT NOT NULL,
                 applied_at TEXT NOT NULL,
                 status TEXT NOT NULL CHECK(status IN ('applied', 'rolled_back')),
                 rollback_id TEXT,
                 rolled_back_at TEXT
             );
             CREATE TABLE IF NOT EXISTS action_receipts(
                 receipt_id TEXT PRIMARY KEY,
                 workspace TEXT NOT NULL,
                 request_hash TEXT NOT NULL UNIQUE,
                 idempotency_key TEXT NOT NULL UNIQUE,
                 capability_id TEXT,
                 request_json TEXT NOT NULL,
                 decision_json TEXT NOT NULL,
                 status TEXT NOT NULL CHECK(status IN ('pending', 'succeeded', 'failed')),
                 reserved_at INTEGER NOT NULL,
                 execution_started_at INTEGER,
                 completed_at INTEGER,
                 external_reference TEXT
             );
             CREATE INDEX IF NOT EXISTS action_receipts_status
                 ON action_receipts(status, reserved_at);
             CREATE TABLE IF NOT EXISTS action_capability_uses(
                 capability_id TEXT PRIMARY KEY,
                 issuer TEXT NOT NULL,
                 receipt_id TEXT NOT NULL UNIQUE REFERENCES action_receipts(receipt_id)
             );
             INSERT OR REPLACE INTO meta(key, value) VALUES('schema', '6');",
        )?;
        return Ok(());
    }
    conn.execute_batch(&format!(
        "
        DROP TABLE IF EXISTS notes; DROP TABLE IF EXISTS links;
        DROP TABLE IF EXISTS fts_main; DROP TABLE IF EXISTS fts_tri;
        CREATE TABLE meta_new(key TEXT PRIMARY KEY, value TEXT);
        DROP TABLE IF EXISTS meta;
        ALTER TABLE meta_new RENAME TO meta;
        INSERT INTO meta(key, value) VALUES('schema', '{SCHEMA_VERSION}');
        CREATE TABLE notes(
            id TEXT PRIMARY KEY, title TEXT, description TEXT, status TEXT,
            origin TEXT, generated_by TEXT, generated_at TEXT,
            mtime INTEGER, body TEXT, tags TEXT DEFAULT '', created TEXT,
            document TEXT NOT NULL DEFAULT '', note_uid TEXT, namespace TEXT,
            authority_role TEXT, authority_status TEXT, authority_scope TEXT
        );
        CREATE UNIQUE INDEX notes_note_uid ON notes(note_uid) WHERE note_uid IS NOT NULL;
        CREATE UNIQUE INDEX notes_active_canonical_scope ON notes(namespace, authority_scope)
            WHERE authority_role = 'canonical' AND authority_status = 'active';
        CREATE TABLE links(src TEXT, dst TEXT, PRIMARY KEY(src, dst));
        CREATE INDEX links_dst ON links(dst);
        CREATE TABLE note_relations(
            src_uid TEXT NOT NULL, kind TEXT NOT NULL, target_uid TEXT NOT NULL,
            PRIMARY KEY(src_uid, kind, target_uid)
        );
        CREATE INDEX note_relations_target ON note_relations(target_uid);
        DROP TABLE IF EXISTS note_vecs;
        CREATE TABLE note_vecs(id TEXT PRIMARY KEY, stamp TEXT, embedding BLOB);
        CREATE VIRTUAL TABLE fts_main USING fts5(id UNINDEXED, text, tokenize='unicode61');
        CREATE VIRTUAL TABLE fts_tri  USING fts5(id UNINDEXED, text, tokenize='trigram');
        CREATE TABLE note_exports(
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            op_id TEXT NOT NULL UNIQUE,
            note_id TEXT NOT NULL,
            operation TEXT NOT NULL,
            base_document TEXT,
            document TEXT,
            log_entry TEXT NOT NULL,
            commit_message TEXT NOT NULL
        );
        CREATE TABLE distillation_runs(
            execution_id TEXT PRIMARY KEY,
            plan_id TEXT NOT NULL,
            before_snapshot_digest TEXT NOT NULL,
            after_snapshot_digest TEXT NOT NULL,
            request_json TEXT NOT NULL,
            before_documents TEXT NOT NULL,
            after_documents TEXT NOT NULL,
            client TEXT NOT NULL,
            applied_at TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN ('applied', 'rolled_back')),
            rollback_id TEXT,
            rolled_back_at TEXT
        );
        CREATE TABLE action_receipts(
            receipt_id TEXT PRIMARY KEY,
            workspace TEXT NOT NULL,
            request_hash TEXT NOT NULL UNIQUE,
            idempotency_key TEXT NOT NULL UNIQUE,
            capability_id TEXT,
            request_json TEXT NOT NULL,
            decision_json TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN ('pending', 'succeeded', 'failed')),
            reserved_at INTEGER NOT NULL,
            execution_started_at INTEGER,
            completed_at INTEGER,
            external_reference TEXT
        );
        CREATE INDEX action_receipts_status ON action_receipts(status, reserved_at);
        CREATE TABLE action_capability_uses(
            capability_id TEXT PRIMARY KEY,
            issuer TEXT NOT NULL,
            receipt_id TEXT NOT NULL UNIQUE REFERENCES action_receipts(receipt_id)
        );
        "
    ))?;
    Ok(())
}

/// GUI/MCPが本体データと一緒に返す、増分syncの結果。
#[derive(Debug, Default)]
#[must_use = "degradedを捨てると索引失敗を正常に見せるため、必ず処理する"]
pub struct SyncReport {
    pub updated: usize,
    pub degraded: Vec<crate::degradation::Degradation>,
}

/// 部分失敗を続行できない呼び出し元向け。失敗を黙殺せず従来の件数を返す。
pub fn sync(vault: &Vault, conn: &Connection) -> Result<usize> {
    let report = sync_with_degradations(vault, conn)?;
    if !report.degraded.is_empty() {
        let detail = report
            .degraded
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" / ");
        bail!("索引へ取り込めないノートがある: {detail}");
    }
    Ok(report.updated)
}

/// 増分syncのfail-open入口。読めないノートは既存rowを残し、正常なノートだけを
/// 同じtransactionで反映する。呼び出し元は`degraded`をデータと一緒に返すこと。
pub fn sync_with_degradations(vault: &Vault, conn: &Connection) -> Result<SyncReport> {
    if runtime_store_is_db(conn) {
        let degraded = match vault.flush_note_exports(conn) {
            Ok(_) => Vec::new(),
            Err(error) => vec![crate::degradation::Degradation::MarkdownExport {
                detail: error.to_string(),
            }],
        };
        return Ok(SyncReport {
            updated: 0,
            degraded,
        });
    }
    import_markdown_snapshot(vault, conn)
}

/// fresh clone・明示import・Git pullだけがMarkdownからDBへ入る入口。
pub fn import_markdown_snapshot(vault: &Vault, conn: &Connection) -> Result<SyncReport> {
    if crate::note_store::pending_count(conn)? != 0 {
        bail!("未出力のDB更新があるためMarkdownをimportできない");
    }
    let files = vault.list_note_files()?;
    let report = sync_files(vault, conn, files)?;
    if report.degraded.is_empty() {
        conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES('runtime_store', 'db-v1')",
            [],
        )?;
    }
    Ok(report)
}

fn runtime_store_is_db(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT value FROM meta WHERE key = 'runtime_store'",
        [],
        |row| row.get::<_, String>(0),
    )
    .is_ok_and(|value| value == "db-v1")
}

pub(crate) fn mark_runtime_store_db(conn: &Connection) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES('runtime_store', 'db-v1')",
        [],
    )?;
    Ok(())
}

fn sync_files(
    vault: &Vault,
    conn: &Connection,
    files: Vec<(String, std::path::PathBuf)>,
) -> Result<SyncReport> {
    let mut updated = 0usize;
    let mut degraded = Vec::new();

    let mut known: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT id, mtime FROM notes")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (id, mtime) = row?;
            known.insert(id, mtime);
        }
    }

    // 2026-08-16の10k fixtureでは1件ごとのautocommitが再構築20秒の大半を占めた。
    // 全件を同じ派生索引versionとして反映し、途中失敗も半端な索引を残さない。
    let transaction = conn.unchecked_transaction()?;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (id, path) in files {
        seen.insert(id.clone());
        if let Err(error) = crate::note_id::NoteId::parse(&id) {
            degraded.push(crate::degradation::Degradation::IndexParse {
                note: id,
                detail: error.to_string(),
            });
            continue;
        }
        // ナノ秒精度 — 秒精度だと同一秒内の連続保存が再索引されない(実測で露呈)
        let mtime = match fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .and_then(|modified| {
                modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(std::io::Error::other)
            }) {
            Ok(modified) => modified.as_nanos() as i64,
            Err(error) => {
                degraded.push(crate::degradation::Degradation::IndexMetadata {
                    note: id,
                    detail: error.to_string(),
                });
                continue;
            }
        };
        if known.get(&id) == Some(&mtime) {
            continue;
        }
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) => {
                degraded.push(crate::degradation::Degradation::IndexRead {
                    note: id,
                    detail: error.to_string(),
                });
                continue;
            }
        };
        let note = match Note::parse(&content) {
            Ok(note) => note,
            Err(error) => {
                degraded.push(crate::degradation::Degradation::IndexParse {
                    note: id,
                    detail: error.to_string(),
                });
                continue;
            }
        };
        upsert(&transaction, vault, &id, mtime, &note)?;
        updated += 1;
    }

    for gone in known.keys().filter(|k| !seen.contains(*k)) {
        let referenced: Option<String> = transaction
            .query_row(
                "SELECT source.id FROM notes target
                 JOIN note_relations relation ON relation.target_uid = target.note_uid
                 JOIN notes source ON source.note_uid = relation.src_uid
                 WHERE target.id=?1 LIMIT 1",
                [gone],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(source) = referenced {
            bail!("typed relationの参照先は削除できない: {source} -> {gone}");
        }
        transaction.execute(
            "DELETE FROM note_relations
             WHERE src_uid = (SELECT note_uid FROM notes WHERE id=?1)",
            [gone],
        )?;
        transaction.execute("DELETE FROM notes WHERE id=?1", [gone])?;
        transaction.execute("DELETE FROM links WHERE src=?1", [gone])?;
        transaction.execute("DELETE FROM fts_main WHERE id=?1", [gone])?;
        transaction.execute("DELETE FROM fts_tri WHERE id=?1", [gone])?;
        transaction.execute("DELETE FROM note_vecs WHERE id=?1", [gone])?;
        updated += 1;
    }
    validate_authority_index(&transaction)?;
    transaction.commit()?;
    Ok(SyncReport { updated, degraded })
}

/// Markdown snapshotを明示importするとき、全noteを同じtransactionへ入れた後で
/// relationの大域条件を検査する。note単位のupsert順には依存させない。
pub(crate) fn validate_authority_index(conn: &Connection) -> Result<()> {
    let dangling: Option<(String, String, String)> = conn
        .query_row(
            "SELECT source.id, relation.kind, relation.target_uid
             FROM note_relations relation
             JOIN notes source ON source.note_uid = relation.src_uid
             LEFT JOIN notes target ON target.note_uid = relation.target_uid
             WHERE target.id IS NULL LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((source, kind, target)) = dangling {
        bail!("typed relationの参照先がない: {source} {kind} -> {target}");
    }

    let invalid_supersedes: Option<(String, String)> = conn
        .query_row(
            "SELECT source.id, target.id
             FROM note_relations relation
             JOIN notes source ON source.note_uid = relation.src_uid
             JOIN notes target ON target.note_uid = relation.target_uid
             WHERE relation.kind = 'supersedes'
               AND NOT (
                 source.authority_role = 'canonical'
                 AND source.authority_status = 'active'
                 AND target.authority_role = 'canonical'
                 AND target.authority_status = 'superseded'
                 AND source.namespace = target.namespace
                 AND source.authority_scope = target.authority_scope
               )
             LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((source, target)) = invalid_supersedes {
        bail!(
            "supersedesは同じnamespace/scopeのactive canonicalからsuperseded canonicalへ結ぶ: {source} -> {target}"
        );
    }

    let orphaned: Option<String> = conn
        .query_row(
            "SELECT target.id FROM notes target
             WHERE target.authority_role = 'canonical'
               AND target.authority_status = 'superseded'
               AND NOT EXISTS (
                 SELECT 1 FROM note_relations relation
                 JOIN notes source ON source.note_uid = relation.src_uid
                 WHERE relation.target_uid = target.note_uid
                   AND relation.kind = 'supersedes'
                   AND source.authority_role = 'canonical'
                   AND source.authority_status = 'active'
                   AND source.namespace = target.namespace
                   AND source.authority_scope = target.authority_scope
               )
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(target) = orphaned {
        bail!("superseded canonicalに後継のsupersedes relationがない: {target}");
    }
    Ok(())
}

/// sync の後段: 未埋め込みノートの追い付き(1回あたり少数に制限し、残は劣化情報で見せる)。
/// モデル未導入なら None(段0 の正常形 — 劣化ではない)。
pub fn embed_step(conn: &Connection) -> Option<crate::degradation::Degradation> {
    if !crate::embed::model_installed() {
        return None;
    }
    match crate::embed::embed_pending(conn, 5) {
        Ok(0) => None,
        Ok(remaining) => Some(crate::degradation::Degradation::EmbeddingIndexPending { remaining }),
        Err(error) => Some(crate::degradation::Degradation::EmbeddingIndex {
            detail: error.to_string(),
        }),
    }
}

pub(crate) fn upsert(
    conn: &Connection,
    vault: &Vault,
    id: &str,
    mtime: i64,
    note: &Note,
) -> Result<()> {
    let f = &note.front;
    // 添付ファイル名も検索対象に(「あの PDF どこだっけ」を引けるように。FR-C8)
    let attach_names: String = vault
        .list_attachments(id)?
        .iter()
        .map(|(n, _)| n.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let search_text = format!(
        "{} {} {} {} {} {} {}",
        f.title.as_deref().unwrap_or(""),
        f.description.as_deref().unwrap_or(""),
        f.tags.join(" "),
        f.authority
            .as_ref()
            .map(|authority| authority.namespace.as_str())
            .unwrap_or(""),
        f.authority
            .as_ref()
            .map(|authority| authority.scope.as_str())
            .unwrap_or(""),
        attach_names,
        note.body
    );
    // 旧本文は notes を上書きする前に取っておく(埋め込み保持判定に使う)
    let old_body: Option<String> = conn
        .query_row("SELECT body FROM notes WHERE id=?1", [id], |r| r.get(0))
        .ok();
    let old_uid: Option<Option<String>> = conn
        .query_row("SELECT note_uid FROM notes WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?;
    if let Some(Some(old_uid)) = &old_uid
        && f.note_uid.as_ref().map(|uid| uid.as_str()) != Some(old_uid.as_str())
    {
        bail!("note_uidは作成後に変更・削除できない: {id}");
    }
    if let Some(Some(old_uid)) = old_uid {
        conn.execute("DELETE FROM note_relations WHERE src_uid=?1", [&old_uid])?;
    }
    conn.execute(
        "INSERT INTO notes(
            id, title, description, status, origin, generated_by, generated_at, mtime, body,
            tags, created, document, note_uid, namespace, authority_role, authority_status,
            authority_scope
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
         ON CONFLICT(id) DO UPDATE SET
            title=excluded.title, description=excluded.description, status=excluded.status,
            origin=excluded.origin, generated_by=excluded.generated_by,
            generated_at=excluded.generated_at, mtime=excluded.mtime, body=excluded.body,
            tags=excluded.tags, created=excluded.created, document=excluded.document,
            note_uid=excluded.note_uid, namespace=excluded.namespace,
            authority_role=excluded.authority_role, authority_status=excluded.authority_status,
            authority_scope=excluded.authority_scope",
        rusqlite::params![
            id,
            f.title,
            f.description,
            f.effective_status(),
            f.origin,
            f.generated.as_ref().map(|g| g.by.clone()),
            f.generated.as_ref().map(|g| g.at.clone()),
            mtime,
            note.body,
            f.tags.join(" "),
            f.created_at(),
            note.to_file_string()?,
            f.note_uid.as_ref().map(|uid| uid.as_str()),
            f.authority
                .as_ref()
                .map(|authority| authority.namespace.as_str()),
            f.authority
                .as_ref()
                .map(|authority| authority.role.as_str()),
            f.authority
                .as_ref()
                .map(|authority| authority.status.as_str()),
            f.authority
                .as_ref()
                .map(|authority| authority.scope.as_str()),
        ],
    )?;
    if let Some(uid) = &f.note_uid {
        for relation in &f.relations {
            conn.execute(
                "INSERT OR IGNORE INTO note_relations(src_uid, kind, target_uid) VALUES(?1, ?2, ?3)",
                rusqlite::params![
                    uid.as_str(),
                    relation.kind.as_str(),
                    relation.target.as_str()
                ],
            )?;
        }
    }
    conn.execute("DELETE FROM fts_main WHERE id=?1", [id])?;
    conn.execute("DELETE FROM fts_tri WHERE id=?1", [id])?;
    // 本文が実際に変わったときだけ埋め込みを捨てる(メタ変更や mtime 精度移行で
    // 全ノート再埋め込みの嵐を起こさない)
    if old_body.as_deref() != Some(note.body.as_str()) {
        conn.execute("DELETE FROM note_vecs WHERE id=?1", [id])?;
    }
    conn.execute(
        "INSERT INTO fts_main(id, text) VALUES(?1, ?2)",
        rusqlite::params![id, wakati(&search_text)],
    )?;
    conn.execute(
        "INSERT INTO fts_tri(id, text) VALUES(?1, ?2)",
        rusqlite::params![id, search_text],
    )?;
    conn.execute("DELETE FROM links WHERE src=?1", [id])?;
    for dst in extract_links(id, &note.body, vault) {
        conn.execute(
            "INSERT OR IGNORE INTO links(src, dst) VALUES(?1, ?2)",
            rusqlite::params![id, dst],
        )?;
    }
    Ok(())
}

/// 標準 markdown リンクから .md 宛先をノート ID へ解決(OKF §6.1)。
/// バンドル相対(/x.md)と相対(./x.md, ../x.md)の両形を受ける。
fn extract_links(src_id: &str, body: &str, _vault: &Vault) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(pos) = rest.find("](") {
        rest = &rest[pos + 2..];
        let Some(end) = rest.find(')') else { break };
        let target = &rest[..end];
        rest = &rest[end..];
        if !target.ends_with(".md") || target.starts_with("http") {
            continue;
        }
        let resolved = if let Some(abs) = target.strip_prefix('/') {
            abs.to_string()
        } else {
            // 相対: src の親ディレクトリから解決
            let base = std::path::Path::new(src_id)
                .parent()
                .unwrap_or(std::path::Path::new(""));
            let mut parts: Vec<&str> = base.iter().filter_map(|c| c.to_str()).collect();
            for comp in target.split('/') {
                match comp {
                    "." | "" => {}
                    ".." => {
                        parts.pop();
                    }
                    c => parts.push(c),
                }
            }
            parts.join("/")
        };
        out.push(resolved.trim_end_matches(".md").to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{
        Authority, AuthorityRole, AuthorityStatus, NoteNamespace, NoteRelation, NoteUid,
        RelationKind,
    };
    use crate::frontmatter::Frontmatter;

    #[test]
    fn read_only_open_never_creates_or_mutates_the_runtime_database() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let path = vault.index_db_path();
        assert!(!path.exists());
        assert!(open_db_read_only(&vault).is_err());
        assert!(!path.exists(), "read-only openが空DBを作ってはならない");

        drop(open_db(&vault).unwrap());
        let conn = open_db_read_only(&vault).unwrap();
        let query_only: i64 = conn
            .query_row("PRAGMA query_only", [], |row| row.get(0))
            .unwrap();
        assert_eq!(query_only, 1);
        assert!(
            conn.execute("INSERT INTO meta(key, value) VALUES('probe', 'write')", [])
                .is_err()
        );
        let count: i64 = conn
            .query_row("SELECT count(*) FROM meta WHERE key='probe'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn sync_and_incremental() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        vault
            .propose_for_test(
                "認証 メモ",
                "認証フローの見直し。[設計](/notes/設計.md) 参照。",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let conn = open_db(&vault).unwrap();
        // propose時点でDB transactionへ反映済み。通常syncはMarkdownを再走査しない。
        assert_eq!(sync(&vault, &conn).unwrap(), 0);
        assert_eq!(sync(&vault, &conn).unwrap(), 0); // 変更なしなら 0
        let n: i64 = conn
            .query_row("SELECT count(*) FROM links", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    /// 2026-08-20、`runtime_store=db-v1`を持つ既存v3 DBへ`document`列だけを
    /// 追加すると、検索行は残る一方で詳細取得が空本文として失敗した。
    #[test]
    fn existing_v3_index_migrates_to_the_db_runtime_store() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let note = Note {
            front: {
                let mut front = Frontmatter::new_note("移行");
                front.origin = Some("agent".into());
                front.tags = vec!["test".into()];
                front
            },
            body: "失わない本文".into(),
        };
        vault.write_note_fixture("notes/migrate", &note).unwrap();
        std::fs::create_dir_all(vault.index_db_path().parent().unwrap()).unwrap();
        let legacy = Connection::open(vault.index_db_path()).unwrap();
        legacy
            .execute_batch(
                "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
                 INSERT INTO meta(key, value) VALUES('schema', '3');
                 INSERT INTO meta(key, value) VALUES('runtime_store', 'db-v1');
                 CREATE TABLE notes(
                    id TEXT PRIMARY KEY, title TEXT, description TEXT, status TEXT,
                    origin TEXT, generated_by TEXT, generated_at TEXT,
                    mtime INTEGER, body TEXT, tags TEXT DEFAULT '', created TEXT
                 );
                 CREATE TABLE links(src TEXT, dst TEXT, PRIMARY KEY(src, dst));
                 CREATE TABLE note_vecs(id TEXT PRIMARY KEY, stamp TEXT, embedding BLOB);
                 CREATE VIRTUAL TABLE fts_main USING fts5(id UNINDEXED, text, tokenize='unicode61');
                 CREATE VIRTUAL TABLE fts_tri USING fts5(id UNINDEXED, text, tokenize='trigram');",
            )
            .unwrap();
        legacy
            .execute(
                "INSERT INTO notes(id, title, status, origin, mtime, body, tags)
                 VALUES(?1, ?2, 'stable', 'agent', 0, ?3, 'test')",
                rusqlite::params!["notes/migrate", "移行", "検索に残る旧本文"],
            )
            .unwrap();
        drop(legacy);

        let migrated = open_db(&vault).unwrap();
        let schema: String = migrated
            .query_row("SELECT value FROM meta WHERE key='schema'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(schema, "6");
        assert!(migrated.prepare("SELECT note_uid, namespace, authority_role, authority_status, authority_scope FROM notes LIMIT 0").is_ok());
        assert!(
            migrated
                .prepare("SELECT src_uid, kind, target_uid FROM note_relations LIMIT 0")
                .is_ok()
        );
        assert_eq!(
            crate::note_store::read(&migrated, "notes/migrate")
                .unwrap()
                .body,
            "失わない本文\n"
        );
        assert_eq!(crate::note_store::pending_count(&migrated).unwrap(), 0);
        let first_document: String = migrated
            .query_row(
                "SELECT document FROM notes WHERE id='notes/migrate'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(migrated);

        let reopened = open_db(&vault).unwrap();
        let second_document: String = reopened
            .query_row(
                "SELECT document FROM notes WHERE id='notes/migrate'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(second_document, first_document, "再起動で再復元しない");
    }

    /// Action Governanceの実行証跡は既存v5 runtime DBにも非破壊で追加し、
    /// note本文とsemantic execution履歴を作り直さない。
    #[test]
    fn existing_v5_index_adds_action_receipts_without_rebuilding_notes() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let id = vault
            .propose_for_test(
                "v5から保持するノート",
                "失わない本文",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let conn = open_db(&vault).unwrap();
        let document: String = conn
            .query_row("SELECT document FROM notes WHERE id=?1", [&id], |row| {
                row.get(0)
            })
            .unwrap();
        conn.execute_batch(
            "DROP TABLE action_capability_uses;
             DROP TABLE action_receipts;
             UPDATE meta SET value='5' WHERE key='schema';",
        )
        .unwrap();
        drop(conn);

        let migrated = open_db(&vault).unwrap();
        let schema: String = migrated
            .query_row("SELECT value FROM meta WHERE key='schema'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(schema, "6");
        assert!(
            migrated
                .prepare(
                    "SELECT receipt_id, workspace, request_hash, idempotency_key, status,
                            reserved_at, execution_started_at, completed_at
                     FROM action_receipts LIMIT 0"
                )
                .is_ok()
        );
        assert!(
            migrated
                .prepare(
                    "SELECT capability_id, issuer, receipt_id
                     FROM action_capability_uses LIMIT 0"
                )
                .is_ok()
        );
        let preserved: String = migrated
            .query_row("SELECT document FROM notes WHERE id=?1", [&id], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(preserved, document);
    }

    /// 2026-08-20、semantic executorのruntime監査表は既存v4 DBにも非破壊で
    /// 追加される必要があり、fresh schemaだけの作成では本番端末に届かない。
    #[test]
    fn existing_v4_index_adds_distillation_runs_without_rebuilding_notes() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let id = vault
            .propose_for_test(
                "v4から保持するノート",
                "失わない本文",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let conn = open_db(&vault).unwrap();
        let document: String = conn
            .query_row("SELECT document FROM notes WHERE id=?1", [&id], |row| {
                row.get(0)
            })
            .unwrap();
        conn.execute_batch(
            "DROP TABLE distillation_runs;
             UPDATE meta SET value='4' WHERE key='schema';",
        )
        .unwrap();
        drop(conn);

        let migrated = open_db(&vault).unwrap();
        let schema: String = migrated
            .query_row("SELECT value FROM meta WHERE key='schema'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(schema, "6");
        assert!(
            migrated
                .prepare(
                    "SELECT execution_id, plan_id, before_documents, after_documents,
                            status, rollback_id FROM distillation_runs LIMIT 0"
                )
                .is_ok()
        );
        let preserved: String = migrated
            .query_row("SELECT document FROM notes WHERE id=?1", [&id], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(preserved, document);
    }

    /// 2026-08-20、復元元が1件でも壊れていると正常行だけ直す半端な移行を残すため、
    /// 全Markdownを読めた後にだけ同じtransactionでDBへ反映する。
    #[test]
    fn missing_document_recovery_rolls_back_every_note_when_one_export_is_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let first = vault
            .propose_for_test("復元一", "本文一", None, &["test".into()], "test/client")
            .unwrap();
        let second = vault
            .propose_for_test("復元二", "本文二", None, &["test".into()], "test/client")
            .unwrap();
        let conn = open_db(&vault).unwrap();
        conn.execute(
            "UPDATE notes SET document='' WHERE id IN (?1, ?2)",
            rusqlite::params![first, second],
        )
        .unwrap();
        fs::write(vault.note_path(&second).unwrap(), "frontmatterではない").unwrap();
        drop(conn);

        assert!(open_db(&vault).is_err());
        let unchanged = Connection::open(vault.index_db_path()).unwrap();
        let empty: i64 = unchanged
            .query_row("SELECT count(*) FROM notes WHERE document=''", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(empty, 2);
    }

    /// 2026-08-20、全復元元を読めた後のDB書き込みで失敗しても、先にupsertした
    /// ノートだけが復元済みになる状態を残さない。
    #[test]
    fn missing_document_recovery_rolls_back_when_one_upsert_fails() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let first = vault
            .propose_for_test("書込一", "本文一", None, &["test".into()], "test/client")
            .unwrap();
        let second = vault
            .propose_for_test("書込二", "本文二", None, &["test".into()], "test/client")
            .unwrap();
        let conn = open_db(&vault).unwrap();
        conn.execute(
            "UPDATE notes SET document='' WHERE id IN (?1, ?2)",
            rusqlite::params![first, second],
        )
        .unwrap();
        conn.execute_batch(&format!(
            "CREATE TRIGGER fail_recovery BEFORE INSERT ON notes
             WHEN NEW.id = '{second}'
             BEGIN SELECT RAISE(FAIL, 'fixture failure'); END;"
        ))
        .unwrap();
        drop(conn);

        assert!(open_db(&vault).is_err());
        let unchanged = Connection::open(vault.index_db_path()).unwrap();
        let empty: i64 = unchanged
            .query_row("SELECT count(*) FROM notes WHERE document=''", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(empty, 2);
    }

    /// 2026-08-20、DB正本がMarkdownより新しい可能性のあるoutbox滞留中は、
    /// 古いexportで空本文を埋めてDB更新を巻き戻さない。
    #[test]
    fn missing_document_recovery_stops_when_an_export_is_pending() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let id = vault
            .propose_for_test("復元待機", "本文", None, &["test".into()], "test/client")
            .unwrap();
        let conn = open_db(&vault).unwrap();
        conn.execute("UPDATE notes SET document='' WHERE id=?1", [&id])
            .unwrap();
        conn.execute(
            "INSERT INTO note_exports(
                op_id, note_id, operation, base_document, document, log_entry, commit_message
             ) VALUES('pending', ?1, 'upsert', NULL, NULL, 'pending', 'pending')",
            [&id],
        )
        .unwrap();
        drop(conn);

        assert!(open_db(&vault).is_err());
        let unchanged = Connection::open(vault.index_db_path()).unwrap();
        let document: String = unchanged
            .query_row("SELECT document FROM notes WHERE id=?1", [&id], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(document.is_empty());
    }

    /// 2026-08-16の10k高速化で追加したtransactionを外す退行と、半端な索引を防ぐ。
    #[test]
    fn sync_rolls_back_every_note_when_one_upsert_fails() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        for id in ["notes/a", "notes/b"] {
            let mut front = Frontmatter::new_note(id);
            front.tags = vec!["test".into()];
            vault
                .write_note_fixture(
                    id,
                    &Note {
                        front,
                        body: "本文".into(),
                    },
                )
                .unwrap();
        }
        conn.execute_batch(
            "CREATE TRIGGER fail_second_note BEFORE INSERT ON notes
             WHEN NEW.id = 'notes/b'
             BEGIN SELECT RAISE(FAIL, 'fixture failure'); END;",
        )
        .unwrap();

        assert!(import_markdown_snapshot(&vault, &conn).is_err());
        let indexed: i64 = conn
            .query_row("SELECT count(*) FROM notes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(indexed, 0);
    }

    #[test]
    fn explicit_import_rejects_a_dangling_typed_relation_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        let mut front = Frontmatter::new_note("参照切れ");
        front.origin = Some("agent".into());
        front.tags = vec!["test".into()];
        front.note_uid = Some(NoteUid::at(1));
        front.authority = Some(Authority {
            namespace: NoteNamespace::Knowledge,
            role: AuthorityRole::Canonical,
            status: AuthorityStatus::Active,
            scope: "test/import-relation".into(),
        });
        front.relations.push(NoteRelation {
            kind: RelationKind::Supports,
            target: NoteUid::at(2),
        });
        vault
            .write_note_fixture(
                "notes/dangling",
                &Note {
                    front,
                    body: "本文".into(),
                },
            )
            .unwrap();

        assert!(import_markdown_snapshot(&vault, &conn).is_err());
        let indexed: i64 = conn
            .query_row("SELECT count(*) FROM notes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(indexed, 0);
    }

    #[test]
    fn explicit_import_collapses_duplicate_typed_relations() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();

        let target_uid = NoteUid::at(1);
        let mut target = Frontmatter::new_note("対象");
        target.origin = Some("agent".into());
        target.tags = vec!["test".into()];
        target.note_uid = Some(target_uid.clone());
        target.authority = Some(Authority {
            namespace: NoteNamespace::Knowledge,
            role: AuthorityRole::Canonical,
            status: AuthorityStatus::Active,
            scope: "test/target".into(),
        });
        vault
            .write_note_fixture(
                "notes/a-target",
                &Note {
                    front: target,
                    body: "対象本文".into(),
                },
            )
            .unwrap();

        let relation = NoteRelation {
            kind: RelationKind::Supports,
            target: target_uid,
        };
        let mut source = Frontmatter::new_note("参照元");
        source.origin = Some("agent".into());
        source.tags = vec!["test".into()];
        source.note_uid = Some(NoteUid::at(2));
        source.authority = Some(Authority {
            namespace: NoteNamespace::Knowledge,
            role: AuthorityRole::Canonical,
            status: AuthorityStatus::Active,
            scope: "test/source".into(),
        });
        source.relations = vec![relation];
        let source_note = Note {
            front: source,
            body: "参照本文".into(),
        };
        vault
            .write_note_fixture("notes/b-source", &source_note)
            .unwrap();

        let report = import_markdown_snapshot(&vault, &conn).unwrap();
        assert!(report.degraded.is_empty());
        // 旧migrationでnotes側のUIDだけが欠け、relation ledgerにはedgeが残った
        // 不整合を再現する。同じMarkdownを再importしても同一edgeは増やさない。
        conn.execute(
            "UPDATE notes SET note_uid=NULL WHERE id='notes/b-source'",
            [],
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        vault
            .write_note_fixture("notes/b-source", &source_note)
            .unwrap();
        let report = import_markdown_snapshot(&vault, &conn).unwrap();
        assert!(report.degraded.is_empty());
        let count: i64 = conn
            .query_row("SELECT count(*) FROM note_relations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn malformed_note_keeps_the_stale_row_and_reports_the_degradation() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        vault
            .propose_for_test(
                "壊れる前",
                "残す本文",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let conn = open_db(&vault).unwrap();
        sync(&vault, &conn).unwrap();
        let (id, path) = vault.list_note_files().unwrap().remove(0);
        fs::write(&path, "frontmatterではない").unwrap();

        let report = import_markdown_snapshot(&vault, &conn).unwrap();
        assert_eq!(report.updated, 0);
        assert!(report.degraded.iter().any(|item| matches!(
            item,
            crate::degradation::Degradation::IndexParse { note, .. } if note == &id
        )));
        let indexed_body: String = conn
            .query_row("SELECT body FROM notes WHERE id=?1", [&id], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(indexed_body, "残す本文");
        assert!(
            import_markdown_snapshot(&vault, &conn)
                .unwrap()
                .degraded
                .iter()
                .any(|item| matches!(item, crate::degradation::Degradation::IndexParse { .. })),
            "明示importは壊れたMarkdownを正常扱いしない"
        );
    }

    #[test]
    fn metadata_and_read_failures_are_separately_typed() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        let missing = dir.path().join("missing.md");
        let directory = dir.path().join("directory.md");
        fs::create_dir(&directory).unwrap();

        let report = sync_files(
            &vault,
            &conn,
            vec![
                ("notes/missing".into(), missing),
                ("notes/directory".into(), directory),
                ("notes/a.files/inside".into(), dir.path().join("unused")),
            ],
        )
        .unwrap();
        assert!(report.degraded.iter().any(|item| matches!(
            item,
            crate::degradation::Degradation::IndexMetadata { note, .. }
                if note == "notes/missing"
        )));
        assert!(report.degraded.iter().any(|item| matches!(
            item,
            crate::degradation::Degradation::IndexRead { note, .. }
                if note == "notes/directory"
        )));
        assert!(report.degraded.iter().any(|item| matches!(
            item,
            crate::degradation::Degradation::IndexParse { note, .. }
                if note == "notes/a.files/inside"
        )));
    }
}
