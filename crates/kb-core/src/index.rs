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

const SCHEMA_VERSION: &str = "8";
/// 現行schema versionの数値形。migration state machineの比較はこちらを使う。
const CURRENT_SCHEMA: u32 = 8;
/// 加算migrationを持つ最古のversion。これより古い宣言versionはfail-closed
/// (unknown versionを破壊的rebuildの合図にしない)。
const OLDEST_SUPPORTED_SCHEMA: u32 = 3;

pub fn open_db(vault: &Vault) -> Result<Connection> {
    Ok(open_db_with_outcome(vault)?.conn)
}

/// open時のhealth check・自己修復の結果を含むopen。修復・劣化・write停止は
/// openを失敗させずここへ載せる(呼び出し面が劣化情報としてユーザーへ見せる)。
/// note writeのfail-closed自体は接続に依存せず、書込側の
/// `derived_index::require_governance_ready` が毎回強制する。
#[must_use = "degraded/write_blockersを捨てると修復失敗を正常に見せるため、必ず処理する"]
pub struct OpenDbOutcome {
    pub conn: Connection,
    /// 今回のopenで再構築に成功した派生索引(一回限りのrecovery notice)。
    pub recovered: Vec<crate::derived_index::DerivedArtifact>,
    pub degraded: Vec<crate::degradation::Degradation>,
    /// 修復できなかったgovernance-critical索引。空でなければnote writeは拒否される。
    pub write_blockers: Vec<crate::derived_index::DerivedArtifact>,
}

pub fn open_db_with_outcome(vault: &Vault) -> Result<OpenDbOutcome> {
    let conn = open_db_recovery(vault)?;
    // 派生索引の修復はMarkdown復元・importより前 — 復元経由のupsertも
    // 修復済みのobjectへ書けるようにする。health OKなら書込は発生しない。
    let repair = crate::derived_index::check_and_repair(vault, &conn)?;
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
    Ok(OpenDbOutcome {
        conn,
        recovered: repair.recovered,
        degraded: repair.degraded,
        write_blockers: repair.write_blockers,
    })
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
    // fail-closed判定(corrupt / future / unsupported / durable table欠落)は
    // `PRAGMA journal_mode=WAL` より前に行う。journal mode変換もDBファイルへの
    // 書込であり、受け入れないDBには1 byteも書かない。
    let state = classify_schema(&conn)?;
    if let SchemaState::Supported(version) = state {
        verify_durable_tables(&conn, version)?;
    }
    let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    debug_assert_eq!(mode.to_lowercase(), "wal");
    match state {
        SchemaState::Fresh => create_fresh_schema(&conn)?,
        SchemaState::Supported(version) => migrate_schema(&conn, version)?,
    }
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

/// open時のschema分類。freshは「`sqlite_schema` が空」の場合だけ。
/// それ以外の受け入れないDB(corrupt / future / unsupported)は分類時点で
/// fail-closedエラーになり、Connectionは呼び出し元へ渡らない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchemaState {
    /// DBオブジェクトが1つもない空DB。registry生成DDLの唯一の対象。
    Fresh,
    /// 加算migrationで現行へ到達できる宣言version({3..=8})。
    Supported(u32),
}

/// 非空DBのmeta/schemaを読み、fail-closedで分類する。書込は一切しない。
/// 旧実装はmetaクエリ失敗を`.ok()`でfresh扱いに潰し、corrupt DBを
/// 破壊的rebuildへ流していた — その経路をここで塞ぐ。
fn classify_schema(conn: &Connection) -> Result<SchemaState> {
    let objects: i64 = conn
        .query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))
        .context("index.dbのschema一覧を読めない(SQLiteファイルとして壊れている可能性)")?;
    if objects == 0 {
        return Ok(SchemaState::Fresh);
    }
    let has_meta: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='meta'",
        [],
        |row| row.get(0),
    )?;
    if has_meta == 0 {
        bail!(
            "index.dbが非空なのにmetaテーブルがない。壊れたDBとして扱い、\
             自動再作成はしない(必要ならindex.dbを退避してから再起動する)"
        );
    }
    let declared: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key='schema'", [], |row| {
            row.get(0)
        })
        .optional()
        .context("metaテーブルからschema versionを読めない")?;
    let Some(declared) = declared else {
        bail!(
            "index.dbのmetaにschema versionがない。壊れたDBとして扱い、\
             自動再作成はしない(必要ならindex.dbを退避してから再起動する)"
        );
    };
    let version: u32 = declared.trim().parse().map_err(|_| {
        anyhow::anyhow!(
            "index.dbのschema versionが数値でない: {declared:?}。壊れたDBとして扱い、\
             自動再作成はしない"
        )
    })?;
    if version > CURRENT_SCHEMA {
        bail!(
            "index.dbのschema versionが新しすぎる: {version}(このバイナリの上限: {CURRENT_SCHEMA})。\
             新しいkbで作られたDBを古いバイナリで開いている。DBには書き込まない"
        );
    }
    if version < OLDEST_SUPPORTED_SCHEMA {
        bail!(
            "index.dbのschema versionが古すぎる: {version}(migration対応は {OLDEST_SUPPORTED_SCHEMA} 以上)。\
             未知の旧versionを破壊的rebuildの合図にはしない"
        );
    }
    Ok(SchemaState::Supported(version))
}

/// 宣言versionの時点で存在しなければならないdurable table(再構築不能な正本)。
/// metaはclassify_schemaで検証済みなので含めない。
fn required_durable_tables(version: u32) -> Vec<&'static str> {
    let mut required = vec!["notes"];
    if version >= 4 {
        required.push("note_exports");
    }
    if version >= 5 {
        required.push("distillation_runs");
    }
    if version >= 6 {
        required.push("action_receipts");
        required.push("action_capability_uses");
    }
    required
}

/// durable tableの欠落は空表作成で隠さずfail-closed。distillation_runsや
/// action_receiptsの喪失は復元不能で、空表を作ると喪失自体が見えなくなる。
fn verify_durable_tables(conn: &Connection, version: u32) -> Result<()> {
    let mut missing = Vec::new();
    for table in required_durable_tables(version) {
        let found: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name=?1",
            [table],
            |row| row.get(0),
        )?;
        if found == 0 {
            missing.push(table);
        }
    }
    if !missing.is_empty() {
        bail!(
            "schema {version} のindex.dbに復元不能なdurable tableがない: {}。\
             空表を作って隠さずopenを失敗させる(必要ならバックアップから復旧する)",
            missing.join(", ")
        );
    }
    Ok(())
}

/// fresh DB(sqlite_schemaが空)にだけ全オブジェクトを生成する。単一transactionで、
/// meta schema書込は最後 — 途中失敗はfreshのまま残り、次回openが再試行する。
/// 派生索引のDDLはregistry(derived_index)の`create_sql`が正本 — fresh作成と
/// 自己修復のDROP/CREATEが同じ文字列を使い、形の分岐を作らない。
fn create_fresh_schema(conn: &Connection) -> Result<()> {
    let transaction = conn.unchecked_transaction()?;
    transaction.execute_batch(
        "
        CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
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
            external_reference TEXT,
            external_target TEXT,
            compensation_deadline INTEGER,
            compensated_at INTEGER
        );
        CREATE INDEX action_receipts_status ON action_receipts(status, reserved_at);
        CREATE TABLE action_capability_uses(
            capability_id TEXT PRIMARY KEY,
            issuer TEXT NOT NULL,
            receipt_id TEXT NOT NULL UNIQUE REFERENCES action_receipts(receipt_id)
        );
        ",
    )?;
    for artifact in crate::derived_index::DerivedArtifact::ALL {
        for object in artifact.spec().objects {
            transaction.execute_batch(object.create_sql)?;
        }
    }
    transaction.execute(
        "INSERT INTO meta(key, value) VALUES('schema', ?1)",
        [SCHEMA_VERSION],
    )?;
    transaction.commit()?;
    Ok(())
}

/// supported versionからの加算migration。明示step列(v3→v4→…→v8)を単一transactionで
/// 適用し、meta version書込は最後。途中失敗は全stepをrollbackして旧versionのまま残す。
fn migrate_schema(conn: &Connection, from: u32) -> Result<()> {
    if from == CURRENT_SCHEMA {
        return Ok(());
    }
    type MigrationStep = fn(&Connection) -> Result<()>;
    const STEPS: [(u32, &str, MigrationStep); 5] = [
        (3, "v3→v4", migrate_v3_to_v4),
        (4, "v4→v5", migrate_v4_to_v5),
        (5, "v5→v6", migrate_v5_to_v6),
        (6, "v6→v7", migrate_v6_to_v7),
        (7, "v7→v8", migrate_v7_to_v8),
    ];
    let transaction = conn.unchecked_transaction()?;
    for (source, label, step) in STEPS {
        if from <= source {
            step(&transaction)
                .with_context(|| format!("schema migration {label} を適用できない"))?;
        }
    }
    transaction.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES('schema', ?1)",
        [SCHEMA_VERSION],
    )?;
    transaction.commit()?;
    Ok(())
}

/// v4(337ead1): 正本authorityと安定UID。notesへの追加カラム、UID/scope索引、
/// note_relations、note_exports(DB正本のoutbox)、links_dst索引。
/// tags/createdはv3期内の後追い列なので、欠けたv3 DBもここで揃える。
fn migrate_v3_to_v4(conn: &Connection) -> Result<()> {
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
            // 破壊的な作り直しをしない(埋め込み再計算の嵐を避ける)。
            // mtime=-1で次回syncに再索引だけを促す。
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
         );",
    )?;
    Ok(())
}

/// v5(7277f8f): semantic蒸留waveのatomic実行・rollback監査表。
fn migrate_v4_to_v5(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS distillation_runs(
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
         );",
    )?;
    Ok(())
}

/// v6(2e32042): action governanceの実行証跡。新規作成は最初から現行(v7相当)の
/// 全列で作る — 直後のv6→v7 stepの列追加が既存列としてno-opになるだけ。
fn migrate_v5_to_v6(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS action_receipts(
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
             external_reference TEXT,
             external_target TEXT,
             compensation_deadline INTEGER,
             compensated_at INTEGER
         );
         CREATE INDEX IF NOT EXISTS action_receipts_status
             ON action_receipts(status, reserved_at);
         CREATE TABLE IF NOT EXISTS action_capability_uses(
             capability_id TEXT PRIMARY KEY,
             issuer TEXT NOT NULL,
             receipt_id TEXT NOT NULL UNIQUE REFERENCES action_receipts(receipt_id)
         );",
    )?;
    Ok(())
}

/// v7(6f1286d): receipt schemaのreconcile対応(external_target /
/// compensation_deadline / compensated_at)。既存行は保持したまま列だけ足す。
fn migrate_v6_to_v7(conn: &Connection) -> Result<()> {
    for (column, ddl) in [
        (
            "external_target",
            "ALTER TABLE action_receipts ADD COLUMN external_target TEXT",
        ),
        (
            "compensation_deadline",
            "ALTER TABLE action_receipts ADD COLUMN compensation_deadline INTEGER",
        ),
        (
            "compensated_at",
            "ALTER TABLE action_receipts ADD COLUMN compensated_at INTEGER",
        ),
    ] {
        if conn
            .prepare(&format!("SELECT {column} FROM action_receipts LIMIT 0"))
            .is_err()
        {
            conn.execute_batch(ddl)?;
        }
    }
    Ok(())
}

/// v8(5b81073): anchor text索引。派生表なのでDB内のnotes本文から一括再構築する。
fn migrate_v7_to_v8(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS fts_anchor USING fts5(
             src UNINDEXED, dst UNINDEXED, text, tokenize='unicode61'
         );",
    )?;
    rebuild_anchor_index_from_notes(conn)?;
    Ok(())
}

pub(crate) fn rebuild_anchor_index_from_notes(conn: &Connection) -> Result<()> {
    let notes = {
        let mut statement = conn.prepare("SELECT id, body FROM notes")?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    conn.execute("DELETE FROM fts_anchor", [])?;
    let mut insert = conn.prepare("INSERT INTO fts_anchor(src, dst, text) VALUES(?1, ?2, ?3)")?;
    for (src, body) in notes {
        for link in extract_link_entries(&src, &body) {
            if !link.anchor.is_empty() {
                insert.execute(rusqlite::params![src, link.dst, wakati(&link.anchor)])?;
            }
        }
    }
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
        // 派生行の削除(inbound relation存在時の拒否を含む)はregistry走査へ統一。
        // durableのnotes行だけをここで消す。
        let note_uid: Option<String> = transaction
            .query_row("SELECT note_uid FROM notes WHERE id=?1", [gone], |row| {
                row.get(0)
            })
            .optional()?
            .flatten();
        crate::derived_index::apply_note_change(
            vault,
            &transaction,
            crate::derived_index::NoteChange::Remove {
                note_id: gone,
                note_uid: note_uid.as_deref(),
            },
        )?;
        transaction.execute("DELETE FROM notes WHERE id=?1", [gone])?;
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
    // 旧行の有無とuidは notes を上書きする前に取っておく(uid不変検査と、
    // registry走査へ渡すNoteChangeの材料)。
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
    // 派生索引(fts_main/fts_tri/links/fts_anchor/note_relations/note_vecs)は
    // registryのapply_change走査が同じtransaction内で維持する。
    crate::derived_index::apply_note_change(
        vault,
        conn,
        crate::derived_index::NoteChange::Upsert {
            note_id: id,
            previous_uid: old_uid.as_ref().and_then(|uid| uid.as_deref()),
            previously_indexed: old_uid.is_some(),
            note,
        },
    )?;
    Ok(())
}

/// 標準 markdown リンクから .md 宛先をノート ID へ解決(OKF §6.1)。
/// バンドル相対(/x.md)と相対(./x.md, ../x.md)の両形を受ける。
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct LinkEntry {
    pub(crate) dst: String,
    pub(crate) anchor: String,
}

pub(crate) fn extract_link_entries(src_id: &str, body: &str) -> Vec<LinkEntry> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(pos) = rest.find("](") {
        let anchor = rest[..pos]
            .rfind('[')
            .map(|start| rest[start + 1..pos].trim())
            .unwrap_or_default();
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
        let entry = LinkEntry {
            dst: resolved.trim_end_matches(".md").to_string(),
            anchor: anchor.to_string(),
        };
        if !out.contains(&entry) {
            out.push(entry);
        }
    }
    out
}

/// 論理snapshot(durable不変の検証部品)。index.rsとderived_index.rsのテストが共用する。
#[cfg(test)]
pub(crate) mod test_support {
    use rusqlite::{Connection, OpenFlags};

    /// spec S-1のdurable table集合(metaはclassifyで検証されるがdumpには含める)。
    pub(crate) const DURABLE_TABLES: [&str; 6] = [
        "meta",
        "notes",
        "note_exports",
        "distillation_runs",
        "action_receipts",
        "action_capability_uses",
    ];

    /// `sqlite_schema` と全durable tableの論理dump。ファイルbyteではなく
    /// 論理内容を比較する(WAL変換などSQLite都合のbyte変化は保証対象外)。
    pub(crate) fn logical_snapshot(path: &std::path::Path) -> Vec<String> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut snapshot = Vec::new();
        {
            let mut statement = conn
                .prepare(
                    "SELECT type, name, tbl_name, coalesce(sql, '')
                     FROM sqlite_schema ORDER BY type, name",
                )
                .unwrap();
            let rows = statement
                .query_map([], |row| {
                    Ok(format!(
                        "schema|{}|{}|{}|{}",
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?
                    ))
                })
                .unwrap();
            for row in rows {
                snapshot.push(row.unwrap());
            }
        }
        snapshot.extend(durable_rows_snapshot(&conn));
        snapshot
    }

    /// durable tableの行だけの論理dump(schema抜き)。派生索引の修復・rollback検証で
    /// 「durable stateはどのrebuildでも変更禁止」を固定するのに使う。
    pub(crate) fn durable_rows_snapshot(conn: &Connection) -> Vec<String> {
        let mut snapshot = Vec::new();
        for table in DURABLE_TABLES {
            let mut statement = match conn.prepare(&format!("SELECT * FROM {table}")) {
                Ok(statement) => statement,
                Err(_) => {
                    snapshot.push(format!("{table}|<absent>"));
                    continue;
                }
            };
            snapshot.append(&mut table_rows_via(&mut statement, table));
        }
        snapshot
    }

    /// 任意tableの論理行dump(rebuild同値性の比較部品)。
    pub(crate) fn table_rows(conn: &Connection, table: &str, columns: &str) -> Vec<String> {
        let mut statement = conn
            .prepare(&format!("SELECT {columns} FROM {table}"))
            .unwrap();
        table_rows_via(&mut statement, table)
    }

    fn table_rows_via(statement: &mut rusqlite::Statement<'_>, label: &str) -> Vec<String> {
        let columns = statement.column_count();
        let mut rows_out = Vec::new();
        let mut rows = statement.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            let mut cells = Vec::with_capacity(columns);
            for index in 0..columns {
                use rusqlite::types::ValueRef;
                cells.push(match row.get_ref(index).unwrap() {
                    ValueRef::Null => "NULL".to_owned(),
                    ValueRef::Integer(value) => value.to_string(),
                    ValueRef::Real(value) => value.to_string(),
                    ValueRef::Text(value) => String::from_utf8_lossy(value).into_owned(),
                    ValueRef::Blob(value) => format!("blob:{}", value.len()),
                });
            }
            rows_out.push(format!("{label}|{}", cells.join("|")));
        }
        rows_out.sort();
        rows_out
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{DURABLE_TABLES, logical_snapshot};
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

    #[test]
    fn link_entries_keep_anchor_text_and_resolve_relative_targets() {
        assert_eq!(
            extract_link_entries(
                "notes/maps/storage",
                "See [blue comet policy](../kepler.md) and [external](https://example.com/x.md)."
            ),
            vec![LinkEntry {
                dst: "notes/kepler".into(),
                anchor: "blue comet policy".into(),
            }]
        );
        assert_eq!(
            extract_link_entries("notes/source", "[](/notes/target.md)"),
            vec![LinkEntry {
                dst: "notes/target".into(),
                anchor: String::new(),
            }]
        );
    }

    #[test]
    fn existing_v7_index_backfills_anchor_text_from_db_bodies() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        vault
            .propose_for_test(
                "Storage Map",
                "See [blue comet policy](/notes/kepler.md).",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let conn = open_db(&vault).unwrap();
        conn.execute("UPDATE meta SET value='7' WHERE key='schema'", [])
            .unwrap();
        conn.execute("DROP TABLE fts_anchor", []).unwrap();
        drop(conn);

        let migrated = open_db(&vault).unwrap();
        let schema: String = migrated
            .query_row("SELECT value FROM meta WHERE key='schema'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let dst: String = migrated
            .query_row(
                "SELECT dst FROM fts_anchor WHERE fts_anchor MATCH ?1",
                [crate::tokenize::match_expr("blue comet policy")],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(schema, "8");
        assert_eq!(dst, "notes/kepler");
    }

    #[test]
    fn updating_a_note_replaces_its_anchor_rows() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let id = vault
            .propose_for_test(
                "Storage Map",
                "See [old blue alias](/notes/kepler.md).",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let conn = open_db(&vault).unwrap();

        vault
            .agent_update_note(
                &conn,
                crate::vault::NoteUpdate {
                    id: &id,
                    title: None,
                    body: Some("See [new green alias](/notes/kepler.md)."),
                    description: None,
                    tags: None,
                    authority: None,
                    relations: None,
                    allow_new_tags: false,
                    client: "test/client",
                },
            )
            .unwrap();

        let old_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM fts_anchor WHERE fts_anchor MATCH ?1",
                [crate::tokenize::match_expr("old blue alias")],
                |row| row.get(0),
            )
            .unwrap();
        let new_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM fts_anchor WHERE fts_anchor MATCH ?1",
                [crate::tokenize::match_expr("new green alias")],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_count, 0);
        assert_eq!(new_count, 1);
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
        assert_eq!(schema, "8");
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
        assert_eq!(schema, "8");
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

    #[test]
    fn existing_v6_receipts_gain_reconcile_fields_without_losing_rows() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        conn.execute_batch(
            "DROP TABLE action_capability_uses;
             DROP TABLE action_receipts;
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
             CREATE TABLE action_capability_uses(
                 capability_id TEXT PRIMARY KEY,
                 issuer TEXT NOT NULL,
                 receipt_id TEXT NOT NULL UNIQUE REFERENCES action_receipts(receipt_id)
             );
             INSERT INTO action_receipts(
                 receipt_id, workspace, request_hash, idempotency_key,
                 request_json, decision_json, status, reserved_at
             ) VALUES(
                 'receipt:v6', 'workspace:test', 'hash:v6', 'key:v6',
                 '{}', '{}', 'pending', 1
             );
             UPDATE meta SET value='6' WHERE key='schema';",
        )
        .unwrap();
        drop(conn);

        let migrated = open_db_recovery(&vault).unwrap();
        let schema: String = migrated
            .query_row("SELECT value FROM meta WHERE key='schema'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(schema, "8");
        let preserved: (String, Option<String>, Option<i64>, Option<i64>) = migrated
            .query_row(
                "SELECT receipt_id, external_target, compensation_deadline, compensated_at
                 FROM action_receipts WHERE receipt_id='receipt:v6'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(preserved, ("receipt:v6".to_owned(), None, None, None));
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
        assert_eq!(schema, "8");
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

    // ------------------------------------------------------------------
    // migration state machine(S-1)のfixtureとテスト
    // ------------------------------------------------------------------

    /// 歴史上のfresh DDLを版順に再現し、各版に存在したdurable tableへ実データ行
    /// (note本文 / pending export / 蒸留履歴 / action receipt)を実装した
    /// fixture DBを作る。戻り値は保存したnote document文字列。
    fn build_versioned_fixture(vault: &Vault, version: u32) -> String {
        assert!((3..=8).contains(&version));
        let mut front = Frontmatter::new_note("移行");
        front.origin = Some("agent".into());
        front.tags = vec!["test".into()];
        let note = Note {
            front,
            body: "See [blue comet policy](/notes/kepler.md).".into(),
        };
        vault.write_note_fixture("notes/migrate", &note).unwrap();
        let document = fs::read_to_string(vault.note_path("notes/migrate").unwrap()).unwrap();
        std::fs::create_dir_all(vault.index_db_path().parent().unwrap()).unwrap();
        let conn = Connection::open(vault.index_db_path()).unwrap();
        // v3(fe2cc42)のfresh DDL
        conn.execute_batch(
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
        conn.execute(
            "INSERT INTO notes(id, title, status, origin, mtime, body, tags)
             VALUES('notes/migrate', '移行', 'stable', 'agent', 0, ?1, 'test')",
            [&note.body],
        )
        .unwrap();
        // 実機のv3同様、索引済みnoteはfts行も持つ(open時のカバレッジhealth checkが
        // 「行の無いfts」を正しく欠損と見なすため、健全fixtureには行を入れる)。
        conn.execute(
            "INSERT INTO fts_main(id, text) VALUES('notes/migrate', ?1)",
            [crate::tokenize::wakati(&format!("移行 test {}", note.body))],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO fts_tri(id, text) VALUES('notes/migrate', ?1)",
            [format!("移行 test {}", note.body)],
        )
        .unwrap();
        if version >= 4 {
            // v4(337ead1): authority列・UID索引・note_relations・note_exports
            conn.execute_batch(
                "ALTER TABLE notes ADD COLUMN document TEXT NOT NULL DEFAULT '';
                 ALTER TABLE notes ADD COLUMN note_uid TEXT;
                 ALTER TABLE notes ADD COLUMN namespace TEXT;
                 ALTER TABLE notes ADD COLUMN authority_role TEXT;
                 ALTER TABLE notes ADD COLUMN authority_status TEXT;
                 ALTER TABLE notes ADD COLUMN authority_scope TEXT;
                 CREATE UNIQUE INDEX notes_note_uid ON notes(note_uid)
                     WHERE note_uid IS NOT NULL;
                 CREATE UNIQUE INDEX notes_active_canonical_scope
                     ON notes(namespace, authority_scope)
                     WHERE authority_role = 'canonical' AND authority_status = 'active';
                 CREATE INDEX links_dst ON links(dst);
                 CREATE TABLE note_relations(
                     src_uid TEXT NOT NULL, kind TEXT NOT NULL, target_uid TEXT NOT NULL,
                     PRIMARY KEY(src_uid, kind, target_uid)
                 );
                 CREATE INDEX note_relations_target ON note_relations(target_uid);
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
                 UPDATE meta SET value='4' WHERE key='schema';",
            )
            .unwrap();
            conn.execute(
                "UPDATE notes SET document=?1 WHERE id='notes/migrate'",
                [&document],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO note_exports(
                     op_id, note_id, operation, base_document, document,
                     log_entry, commit_message
                 ) VALUES('op:matrix-pending', 'notes/migrate', 'upsert', NULL, ?1,
                          'log entry', 'commit message')",
                [&document],
            )
            .unwrap();
        }
        if version >= 5 {
            // v5(7277f8f): 蒸留waveの監査表
            conn.execute_batch(
                "CREATE TABLE distillation_runs(
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
                 INSERT INTO distillation_runs VALUES(
                     'exec:matrix', 'plan:matrix', 'digest:before', 'digest:after',
                     '{}', '{}', '{}', 'test/client', '2026-08-01T00:00:00Z',
                     'applied', NULL, NULL
                 );
                 UPDATE meta SET value='5' WHERE key='schema';",
            )
            .unwrap();
        }
        if version >= 6 {
            // v6(2e32042): action governance証跡(reconcile列はまだない)
            conn.execute_batch(
                "CREATE TABLE action_receipts(
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
                 INSERT INTO action_receipts(
                     receipt_id, workspace, request_hash, idempotency_key,
                     request_json, decision_json, status, reserved_at
                 ) VALUES('receipt:matrix', 'workspace:test', 'hash:matrix', 'key:matrix',
                          '{}', '{}', 'pending', 1);
                 INSERT INTO action_capability_uses VALUES(
                     'cap:matrix', 'issuer:test', 'receipt:matrix'
                 );
                 UPDATE meta SET value='6' WHERE key='schema';",
            )
            .unwrap();
        }
        if version >= 7 {
            // v7(6f1286d): receiptのreconcile列
            conn.execute_batch(
                "ALTER TABLE action_receipts ADD COLUMN external_target TEXT;
                 ALTER TABLE action_receipts ADD COLUMN compensation_deadline INTEGER;
                 ALTER TABLE action_receipts ADD COLUMN compensated_at INTEGER;
                 UPDATE meta SET value='7' WHERE key='schema';",
            )
            .unwrap();
        }
        if version >= 8 {
            // v8(5b81073): anchor text索引
            conn.execute_batch(
                "CREATE VIRTUAL TABLE fts_anchor USING fts5(
                     src UNINDEXED, dst UNINDEXED, text, tokenize='unicode61'
                 );
                 UPDATE meta SET value='8' WHERE key='schema';",
            )
            .unwrap();
        }
        document
    }

    /// v3..v8の各fixture(durable行入り)が現行schemaへ到達し、durable行を
    /// 1行も失わないことのmatrix検証。
    #[test]
    fn migration_matrix_reaches_current_schema_and_preserves_durable_rows() {
        for version in 3..=8u32 {
            let dir = tempfile::tempdir().unwrap();
            let vault = Vault::create(dir.path().join("v")).unwrap();
            let document = build_versioned_fixture(&vault, version);

            let migrated = open_db(&vault)
                .unwrap_or_else(|error| panic!("v{version} fixtureを開けない: {error:#}"));
            let schema: String = migrated
                .query_row("SELECT value FROM meta WHERE key='schema'", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(schema, SCHEMA_VERSION, "v{version}");
            for table in DURABLE_TABLES {
                assert!(
                    migrated
                        .prepare(&format!("SELECT * FROM {table} LIMIT 0"))
                        .is_ok(),
                    "v{version}: {table} が現行schemaに存在しない"
                );
            }
            // note本文(v3は復元経由、v4+はdocument列保持)
            let (title, body, stored): (String, String, String) = migrated
                .query_row(
                    "SELECT title, body, document FROM notes WHERE id='notes/migrate'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(title, "移行", "v{version}");
            assert!(body.contains("blue comet policy"), "v{version}");
            assert_eq!(stored, document, "v{version}: note documentが失われた");
            if version >= 4 {
                let export: (String, String, Option<String>, String, String) = migrated
                    .query_row(
                        "SELECT op_id, operation, base_document, document, log_entry
                         FROM note_exports WHERE op_id='op:matrix-pending'",
                        [],
                        |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                            ))
                        },
                    )
                    .unwrap();
                assert_eq!(
                    export,
                    (
                        "op:matrix-pending".to_owned(),
                        "upsert".to_owned(),
                        None,
                        document.clone(),
                        "log entry".to_owned()
                    ),
                    "v{version}: pending exportが失われた"
                );
            }
            if version >= 5 {
                let run: (String, String, String) = migrated
                    .query_row(
                        "SELECT plan_id, applied_at, status
                         FROM distillation_runs WHERE execution_id='exec:matrix'",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .unwrap();
                assert_eq!(
                    run,
                    (
                        "plan:matrix".to_owned(),
                        "2026-08-01T00:00:00Z".to_owned(),
                        "applied".to_owned()
                    ),
                    "v{version}: 蒸留履歴が失われた"
                );
            }
            if version >= 6 {
                let receipt: (String, String, Option<String>, Option<i64>, Option<i64>) = migrated
                    .query_row(
                        "SELECT workspace, status, external_target,
                                    compensation_deadline, compensated_at
                             FROM action_receipts WHERE receipt_id='receipt:matrix'",
                        [],
                        |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                            ))
                        },
                    )
                    .unwrap();
                assert_eq!(
                    receipt,
                    (
                        "workspace:test".to_owned(),
                        "pending".to_owned(),
                        None,
                        None,
                        None
                    ),
                    "v{version}: action receiptが失われた"
                );
                let capability: (String, String) = migrated
                    .query_row(
                        "SELECT issuer, receipt_id FROM action_capability_uses
                         WHERE capability_id='cap:matrix'",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .unwrap();
                assert_eq!(
                    capability,
                    ("issuer:test".to_owned(), "receipt:matrix".to_owned()),
                    "v{version}: capability使用証跡が失われた"
                );
            }
            if version < 8 {
                // v7→v8 stepがDB本文からanchor索引を再構築している
                let dst: String = migrated
                    .query_row(
                        "SELECT dst FROM fts_anchor WHERE fts_anchor MATCH ?1",
                        [crate::tokenize::match_expr("blue comet policy")],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(dst, "notes/kepler", "v{version}");
            }
        }
    }

    /// 現行versionのDBを開いても、DDL・DMLを一切行わない(論理snapshot完全一致)。
    #[test]
    fn opening_a_current_schema_database_performs_no_writes() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        build_versioned_fixture(&vault, 8);
        let before = logical_snapshot(&vault.index_db_path());

        drop(open_db(&vault).unwrap());

        assert_eq!(before, logical_snapshot(&vault.index_db_path()));
    }

    /// future schema(新しいkbで作られたDB)はfail-closed。WAL変換すら行わず、
    /// `sqlite_schema` と全durable tableの論理内容を変えない。
    #[test]
    fn future_schema_fails_closed_before_wal_and_keeps_logical_state() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        build_versioned_fixture(&vault, 8);
        let path = vault.index_db_path();
        Connection::open(&path)
            .unwrap()
            .execute("UPDATE meta SET value='999' WHERE key='schema'", [])
            .unwrap();
        let before = logical_snapshot(&path);

        let error = open_db(&vault).unwrap_err();
        assert!(
            format!("{error:#}").contains("新しすぎる"),
            "未知のfutureを明確に報告する: {error:#}"
        );
        assert_eq!(before, logical_snapshot(&path));
        // fail-closed判定がWAL設定より前 — journal modeは元のまま
        let probe = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mode: String = probe
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "delete");
    }

    /// 非空DBのmeta欠落・schema key欠落・非数値versionはいずれもcorruptとして
    /// fail-closed。旧実装のように黙ってfresh扱い(破壊的rebuild)へ流さない。
    #[test]
    fn corrupt_meta_fails_closed_without_any_ddl() {
        // (a) metaテーブル自体がない非空DB
        {
            let dir = tempfile::tempdir().unwrap();
            let vault = Vault::create(dir.path().join("v")).unwrap();
            std::fs::create_dir_all(vault.index_db_path().parent().unwrap()).unwrap();
            Connection::open(vault.index_db_path())
                .unwrap()
                .execute_batch("CREATE TABLE stray(x); INSERT INTO stray VALUES(1);")
                .unwrap();
            let before = logical_snapshot(&vault.index_db_path());
            let error = open_db(&vault).unwrap_err();
            assert!(format!("{error:#}").contains("meta"), "{error:#}");
            assert_eq!(before, logical_snapshot(&vault.index_db_path()));
        }
        // (b) metaはあるがschema keyがない
        {
            let dir = tempfile::tempdir().unwrap();
            let vault = Vault::create(dir.path().join("v")).unwrap();
            build_versioned_fixture(&vault, 8);
            Connection::open(vault.index_db_path())
                .unwrap()
                .execute("DELETE FROM meta WHERE key='schema'", [])
                .unwrap();
            let before = logical_snapshot(&vault.index_db_path());
            let error = open_db(&vault).unwrap_err();
            assert!(
                format!("{error:#}").contains("schema versionがない"),
                "{error:#}"
            );
            assert_eq!(before, logical_snapshot(&vault.index_db_path()));
        }
        // (c) schema versionが数値でない
        {
            let dir = tempfile::tempdir().unwrap();
            let vault = Vault::create(dir.path().join("v")).unwrap();
            build_versioned_fixture(&vault, 8);
            Connection::open(vault.index_db_path())
                .unwrap()
                .execute("UPDATE meta SET value='eight' WHERE key='schema'", [])
                .unwrap();
            let before = logical_snapshot(&vault.index_db_path());
            let error = open_db(&vault).unwrap_err();
            assert!(format!("{error:#}").contains("数値でない"), "{error:#}");
            assert_eq!(before, logical_snapshot(&vault.index_db_path()));
        }
    }

    /// migration対応より古い宣言version(v1/v2)は破壊的rebuildの合図にしない。
    /// 旧実装はここでnotes等をDROPして作り直していた。
    #[test]
    fn too_old_schema_is_not_a_destructive_rebuild_signal() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        build_versioned_fixture(&vault, 3);
        Connection::open(vault.index_db_path())
            .unwrap()
            .execute("UPDATE meta SET value='2' WHERE key='schema'", [])
            .unwrap();
        let before = logical_snapshot(&vault.index_db_path());

        let error = open_db(&vault).unwrap_err();
        assert!(format!("{error:#}").contains("古すぎる"), "{error:#}");
        assert_eq!(
            before,
            logical_snapshot(&vault.index_db_path()),
            "notes行が破壊されず残っている"
        );
    }

    /// 宣言versionに必要なdurable tableが欠けたDBは、空表作成で隠さずopen失敗。
    #[test]
    fn missing_durable_table_fails_closed_instead_of_recreating_it_empty() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        build_versioned_fixture(&vault, 8);
        Connection::open(vault.index_db_path())
            .unwrap()
            .execute_batch("DROP TABLE distillation_runs;")
            .unwrap();
        let before = logical_snapshot(&vault.index_db_path());

        let error = open_db(&vault).unwrap_err();
        assert!(
            format!("{error:#}").contains("distillation_runs"),
            "{error:#}"
        );
        assert_eq!(before, logical_snapshot(&vault.index_db_path()));
    }

    /// migration最初のstepで失敗しても、単一transactionが旧状態を完全に残す。
    #[test]
    fn migration_failure_rolls_back_to_the_declared_version() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        build_versioned_fixture(&vault, 3);
        Connection::open(vault.index_db_path())
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER fail_migration BEFORE UPDATE ON notes
                 BEGIN SELECT RAISE(FAIL, 'fixture failure'); END;",
            )
            .unwrap();
        let before = logical_snapshot(&vault.index_db_path());

        let error = open_db(&vault).unwrap_err();
        assert!(format!("{error:#}").contains("v3→v4"), "{error:#}");
        assert_eq!(before, logical_snapshot(&vault.index_db_path()));
        let unchanged = Connection::open(vault.index_db_path()).unwrap();
        let schema: String = unchanged
            .query_row("SELECT value FROM meta WHERE key='schema'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(schema, "3", "meta versionは最後まで書かれない");
        assert!(
            unchanged
                .prepare("SELECT document FROM notes LIMIT 0")
                .is_err(),
            "途中まで適用した列追加が残らない"
        );
    }

    /// 後段step(v7→v8)の失敗は、前段step(v5→v6)の適用済みDDLも巻き戻す —
    /// 全stepが単一transactionである検証。
    #[test]
    fn late_step_failure_rolls_back_earlier_steps_too() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        build_versioned_fixture(&vault, 5);
        // fts_anchorの名前を先取りする通常表 — v7→v8のanchor再構築だけを失敗させる
        Connection::open(vault.index_db_path())
            .unwrap()
            .execute_batch("CREATE TABLE fts_anchor(x);")
            .unwrap();
        let before = logical_snapshot(&vault.index_db_path());

        let error = open_db(&vault).unwrap_err();
        assert!(format!("{error:#}").contains("v7→v8"), "{error:#}");
        assert_eq!(before, logical_snapshot(&vault.index_db_path()));
        let unchanged = Connection::open(vault.index_db_path()).unwrap();
        let schema: String = unchanged
            .query_row("SELECT value FROM meta WHERE key='schema'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(schema, "5");
        assert!(
            unchanged
                .prepare("SELECT * FROM action_receipts LIMIT 0")
                .is_err(),
            "v5→v6で作ったaction_receiptsも巻き戻る"
        );
    }
}
