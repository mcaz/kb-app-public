//! 派生索引のregistry(単一の台帳)と、open時の自己修復。
//!
//! 「派生 = notes正本(+添付名)から決定的に再構築できるSQLite object」。
//! durable state(notes / meta / note_exports / distillation_runs / action_*)は
//! `DerivedArtifact` に列挙されない — **型の上で登録不能**にすることで、修復・
//! rebuildの対象に正本が紛れ込む事故クラスを消す(spec S-2)。
//!
//! 増分維持(ノート書込のたび)と一括rebuild(修復時)は同じ registry の
//! `apply_change` / `rebuild` を通り、同値性はテストで固定する。

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension};

use crate::degradation::Degradation;
use crate::frontmatter::Note;
use crate::tokenize::wakati;
use crate::vault::Vault;

/// 派生索引の全量。durable tableはvariantが存在しないため登録できない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum DerivedArtifact {
    FtsMain,
    FtsTri,
    Links,
    FtsAnchor,
    NoteRelations,
    NoteVecs,
}

impl DerivedArtifact {
    pub const ALL: [DerivedArtifact; 6] = [
        DerivedArtifact::FtsMain,
        DerivedArtifact::FtsTri,
        DerivedArtifact::Links,
        DerivedArtifact::FtsAnchor,
        DerivedArtifact::NoteRelations,
        DerivedArtifact::NoteVecs,
    ];

    pub fn name(&self) -> &'static str {
        match self {
            Self::FtsMain => "fts_main",
            Self::FtsTri => "fts_tri",
            Self::Links => "links",
            Self::FtsAnchor => "fts_anchor",
            Self::NoteRelations => "note_relations",
            Self::NoteVecs => "note_vecs",
        }
    }

    pub(crate) fn spec(&self) -> &'static ArtifactSpec {
        match self {
            Self::FtsMain => &FTS_MAIN,
            Self::FtsTri => &FTS_TRI,
            Self::Links => &LINKS,
            Self::FtsAnchor => &FTS_ANCHOR,
            Self::NoteRelations => &NOTE_RELATIONS,
            Self::NoteVecs => &NOTE_VECS,
        }
    }
}

impl std::fmt::Display for DerivedArtifact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// 維持経路。Machine = ノート書込と同じtransactionで即時維持。
/// Routine = 背景の追い付き(embed_step等)が埋める。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "laneは登録の宣言情報。B案(retrieval_entries)統合で参照が増える"
)]
pub(crate) enum Lane {
    Machine,
    Routine,
}

/// 壊れたときの影響面。修復契約(check_and_repair)がこの値で分岐する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Criticality {
    /// 欠損・不整合中はnote writeをfail-closedにする(note_relations)。
    GovernanceCritical,
    /// 主検索経路。修復失敗でもopenは成功し、検索はfallback+劣化表示。
    RetrievalPrimary,
    /// 補助経路。欠落は劣化として見せるが、無くても主検索は成立する。
    RetrievalOptional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObjectKind {
    Table,
    Index,
    VirtualTable,
}

/// 存在検証とDROP/CREATEの単位。`create_sql` はfresh DDLと修復の共通正本。
pub(crate) struct SqliteObjectSpec {
    pub(crate) name: &'static str,
    pub(crate) kind: ObjectKind,
    pub(crate) create_sql: &'static str,
}

#[derive(Debug)]
pub(crate) enum ArtifactHealth {
    Ready,
    Broken { detail: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RebuildOutcome {
    /// rebuild完了、即利用可能。
    Ready,
    /// objectは復元したが、行は背景処理の追い付き待ち(残件数)。
    Pending(usize),
    /// 能力が未導入(埋め込みモデルなし)。段0の正常形 — 劣化ではない。
    CapabilityUnavailable,
}

/// ノート1件の変化。増分維持の全情報をここへ寄せ、artifactごとの
/// 手書きDELETE群(旧 index.rs / note_store.rs の2箇所)を registry 走査へ統一する。
#[derive(Clone, Copy)]
pub(crate) enum NoteChange<'a> {
    Upsert {
        note_id: &'a str,
        /// 旧行のnote_uid(uidを持たないlegacy行は None)。note_relationsの旧uid削除用。
        previous_uid: Option<&'a str>,
        /// 旧行が存在したか。previous_uidだけではlegacy行(uidなし)の既存を
        /// 表現できず、fts_anchorの旧行掃除を取りこぼすため別に持つ。
        previously_indexed: bool,
        note: &'a Note,
    },
    Remove {
        note_id: &'a str,
        note_uid: Option<&'a str>,
    },
}

/// 1回の`apply_note_change`内でartifact間に共有する導出のlazyキャッシュ。
/// fts_main/fts_triの検索テキスト(添付名のFS列挙を含む)とlinks/fts_anchorの
/// リンク抽出を1回だけ計算する。単一note更新100回gate(S-5 予算)の実測で、
/// artifactごとの重複計算がupdate経路の主要な加算コストだったため導入。
/// 意味は不変 — 各artifactが個別に計算した場合と同じ値を返す。
#[derive(Default)]
pub(crate) struct ChangeCache {
    search_text: std::cell::OnceCell<String>,
    links: std::cell::OnceCell<Vec<crate::index::LinkEntry>>,
}

impl ChangeCache {
    fn search_text(&self, vault: &Vault, note_id: &str, note: &Note) -> Result<&str> {
        if let Some(text) = self.search_text.get() {
            return Ok(text);
        }
        let computed = search_text_for_note(vault, note_id, note)?;
        Ok(self.search_text.get_or_init(|| computed))
    }

    fn links(&self, note_id: &str, note: &Note) -> &[crate::index::LinkEntry] {
        self.links
            .get_or_init(|| crate::index::extract_link_entries(note_id, &note.body))
    }
}

pub(crate) struct ArtifactSpec {
    #[allow(dead_code, reason = "宣言情報(spec S-2)。統合ステージで参照される")]
    pub(crate) lane: Lane,
    pub(crate) criticality: Criticality,
    pub(crate) objects: &'static [SqliteObjectSpec],
    pub(crate) health: fn(&Vault, &Connection) -> Result<ArtifactHealth>,
    pub(crate) rebuild: fn(&Vault, &Connection) -> Result<RebuildOutcome>,
    pub(crate) apply_change: fn(&Vault, &Connection, NoteChange<'_>, &ChangeCache) -> Result<()>,
}

/// durable state(正本・復元不能な台帳)。registryのobject名がここへ触れることは
/// テストで禁止する(型上はそもそも `DerivedArtifact` に列挙できない)。
#[allow(
    dead_code,
    reason = "allowlistの正本。テスト(registry_cannot_own_durable_state_tables)が参照する"
)]
pub(crate) const DURABLE_STATE_TABLES: [&str; 6] = [
    "meta",
    "notes",
    "note_exports",
    "distillation_runs",
    "action_receipts",
    "action_capability_uses",
];

// ---------------------------------------------------------------- registry

static FTS_MAIN: ArtifactSpec = ArtifactSpec {
    lane: Lane::Machine,
    criticality: Criticality::RetrievalPrimary,
    objects: &[SqliteObjectSpec {
        name: "fts_main",
        kind: ObjectKind::VirtualTable,
        create_sql: "CREATE VIRTUAL TABLE fts_main USING fts5(id UNINDEXED, text, tokenize='unicode61');",
    }],
    health: fts_main_health,
    rebuild: fts_main_rebuild,
    apply_change: fts_main_apply,
};

static FTS_TRI: ArtifactSpec = ArtifactSpec {
    lane: Lane::Machine,
    criticality: Criticality::RetrievalPrimary,
    objects: &[SqliteObjectSpec {
        name: "fts_tri",
        kind: ObjectKind::VirtualTable,
        create_sql: "CREATE VIRTUAL TABLE fts_tri USING fts5(id UNINDEXED, text, tokenize='trigram');",
    }],
    health: fts_tri_health,
    rebuild: fts_tri_rebuild,
    apply_change: fts_tri_apply,
};

static LINKS: ArtifactSpec = ArtifactSpec {
    lane: Lane::Machine,
    criticality: Criticality::RetrievalPrimary,
    objects: &[
        SqliteObjectSpec {
            name: "links",
            kind: ObjectKind::Table,
            create_sql: "CREATE TABLE links(src TEXT, dst TEXT, PRIMARY KEY(src, dst));",
        },
        SqliteObjectSpec {
            name: "links_dst",
            kind: ObjectKind::Index,
            create_sql: "CREATE INDEX links_dst ON links(dst);",
        },
    ],
    health: links_health,
    rebuild: links_rebuild,
    apply_change: links_apply,
};

static FTS_ANCHOR: ArtifactSpec = ArtifactSpec {
    lane: Lane::Machine,
    // schema上は常に存在すべきobjectなので、欠落は(検索を止めない)劣化として扱う。
    criticality: Criticality::RetrievalOptional,
    objects: &[SqliteObjectSpec {
        name: "fts_anchor",
        kind: ObjectKind::VirtualTable,
        create_sql: "CREATE VIRTUAL TABLE fts_anchor USING fts5(
            src UNINDEXED, dst UNINDEXED, text, tokenize='unicode61'
        );",
    }],
    health: fts_anchor_health,
    rebuild: fts_anchor_rebuild,
    apply_change: fts_anchor_apply,
};

static NOTE_RELATIONS: ArtifactSpec = ArtifactSpec {
    lane: Lane::Machine,
    criticality: Criticality::GovernanceCritical,
    objects: &[
        SqliteObjectSpec {
            name: "note_relations",
            kind: ObjectKind::Table,
            create_sql: "CREATE TABLE note_relations(
                src_uid TEXT NOT NULL, kind TEXT NOT NULL, target_uid TEXT NOT NULL,
                PRIMARY KEY(src_uid, kind, target_uid)
            );",
        },
        SqliteObjectSpec {
            name: "note_relations_target",
            kind: ObjectKind::Index,
            create_sql: "CREATE INDEX note_relations_target ON note_relations(target_uid);",
        },
    ],
    health: note_relations_health,
    rebuild: note_relations_rebuild,
    apply_change: note_relations_apply,
};

static NOTE_VECS: ArtifactSpec = ArtifactSpec {
    lane: Lane::Routine,
    // モデル未導入は正常(段0)。導入済みの欠損・pendingは embed_step が劣化として見せる。
    criticality: Criticality::RetrievalOptional,
    objects: &[SqliteObjectSpec {
        name: "note_vecs",
        kind: ObjectKind::Table,
        create_sql: "CREATE TABLE note_vecs(id TEXT PRIMARY KEY, stamp TEXT, embedding BLOB);",
    }],
    health: note_vecs_health,
    rebuild: note_vecs_rebuild,
    apply_change: note_vecs_apply,
};

// ---------------------------------------------------------------- health

/// sqlite_schema上の存在と種別(通常table / index / fts5仮想table)を確認する。
fn object_health(conn: &Connection, object: &SqliteObjectSpec) -> Result<ArtifactHealth> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT type, coalesce(sql, '') FROM sqlite_schema WHERE name = ?1",
            [object.name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((kind, sql)) = row else {
        return Ok(ArtifactHealth::Broken {
            detail: format!("{} が存在しない", object.name),
        });
    };
    let normalized = sql.to_uppercase();
    let matches_kind = match object.kind {
        ObjectKind::Table => kind == "table" && !normalized.contains("VIRTUAL TABLE"),
        ObjectKind::VirtualTable => kind == "table" && normalized.contains("VIRTUAL TABLE"),
        ObjectKind::Index => kind == "index",
    };
    if !matches_kind {
        return Ok(ArtifactHealth::Broken {
            detail: format!("{} の種別が不正: {kind}", object.name),
        });
    }
    Ok(ArtifactHealth::Ready)
}

fn objects_health(conn: &Connection, objects: &[SqliteObjectSpec]) -> Result<ArtifactHealth> {
    for object in objects {
        if let ArtifactHealth::Broken { detail } = object_health(conn, object)? {
            return Ok(ArtifactHealth::Broken { detail });
        }
    }
    Ok(ArtifactHealth::Ready)
}

fn links_health(_vault: &Vault, conn: &Connection) -> Result<ArtifactHealth> {
    objects_health(conn, LINKS.objects)
}

fn fts_anchor_health(_vault: &Vault, conn: &Connection) -> Result<ArtifactHealth> {
    objects_health(conn, FTS_ANCHOR.objects)
}

fn note_vecs_health(_vault: &Vault, conn: &Connection) -> Result<ArtifactHealth> {
    if let ArtifactHealth::Broken { detail } = objects_health(conn, NOTE_VECS.objects)? {
        return Ok(ArtifactHealth::Broken { detail });
    }
    // stamp形式検査(spec S-3の安価health列挙)。現行複合形
    // (`CURRENT_STAMP_PREFIX`)でない行は旧形式・他producer・破損のいずれかで、
    // knn対象外の死蔵行。Brokenとしてrebuild(drop→再作成)へ回し、行は
    // embed_stepのpendingが追い付く。行の欠落(未埋め込み)は正常なので数えない。
    let malformed: i64 = conn.query_row(
        "SELECT count(*) FROM note_vecs WHERE stamp IS NULL OR substr(stamp, 1, ?1) <> ?2",
        rusqlite::params![
            crate::embed::CURRENT_STAMP_PREFIX.len() as i64,
            crate::embed::CURRENT_STAMP_PREFIX
        ],
        |row| row.get(0),
    )?;
    if malformed > 0 {
        return Ok(ArtifactHealth::Broken {
            detail: format!("note_vecs のstamp形式が現行と不一致({malformed}件)"),
        });
    }
    Ok(ArtifactHealth::Ready)
}

/// note IDカバレッジ(安価なhealth check)。notes⊆ftsとfts⊆notesの両方向を数える。
/// 全文の再parse・再tokenize監査はopenごとには行わない(修復後とテストのみ)。
/// NOT INはid NULL行が片側に1行あるだけで全体がNULLに評価され、実在する欠落・
/// 迷子を0件に見せる(レビュー指摘)— 内側からNULLを除外し、fts側のNULL行
/// 自体も迷子として数える。
fn fts_coverage_health(conn: &Connection, table: &str) -> Result<ArtifactHealth> {
    let missing: i64 = conn.query_row(
        &format!(
            "SELECT count(*) FROM notes
             WHERE id NOT IN (SELECT id FROM {table} WHERE id IS NOT NULL)"
        ),
        [],
        |row| row.get(0),
    )?;
    let stray: i64 = conn.query_row(
        &format!(
            "SELECT count(*) FROM {table}
             WHERE id IS NULL OR id NOT IN (SELECT id FROM notes WHERE id IS NOT NULL)"
        ),
        [],
        |row| row.get(0),
    )?;
    if missing > 0 || stray > 0 {
        return Ok(ArtifactHealth::Broken {
            detail: format!("{table} のnote IDカバレッジ不整合(欠落{missing}件 / 迷子{stray}件)"),
        });
    }
    Ok(ArtifactHealth::Ready)
}

fn fts_main_health(_vault: &Vault, conn: &Connection) -> Result<ArtifactHealth> {
    if let ArtifactHealth::Broken { detail } = objects_health(conn, FTS_MAIN.objects)? {
        return Ok(ArtifactHealth::Broken { detail });
    }
    fts_coverage_health(conn, "fts_main")
}

fn fts_tri_health(_vault: &Vault, conn: &Connection) -> Result<ArtifactHealth> {
    if let ArtifactHealth::Broken { detail } = objects_health(conn, FTS_TRI.objects)? {
        return Ok(ArtifactHealth::Broken { detail });
    }
    fts_coverage_health(conn, "fts_tri")
}

fn note_relations_health(_vault: &Vault, conn: &Connection) -> Result<ArtifactHealth> {
    if let ArtifactHealth::Broken { detail } = objects_health(conn, NOTE_RELATIONS.objects)? {
        return Ok(ArtifactHealth::Broken { detail });
    }
    if let Err(error) = crate::index::validate_authority_index(conn) {
        return Ok(ArtifactHealth::Broken {
            detail: format!("authority整合検証に失敗: {error:#}"),
        });
    }
    Ok(ArtifactHealth::Ready)
}

// ---------------------------------------------------------------- rebuild

/// notes行(+添付名)から検索本文を組み立てる。upsert増分と一括rebuildの共通正本。
/// 欄の並び・区切りを変えるときはrebuild同値性テストが差を検出する。
pub(crate) fn note_search_text(
    title: Option<&str>,
    description: Option<&str>,
    tags_joined: &str,
    namespace: Option<&str>,
    scope: Option<&str>,
    attach_names: &str,
    body: &str,
) -> String {
    format!(
        "{} {} {} {} {} {} {}",
        title.unwrap_or(""),
        description.unwrap_or(""),
        tags_joined,
        namespace.unwrap_or(""),
        scope.unwrap_or(""),
        attach_names,
        body
    )
}

fn search_text_for_note(vault: &Vault, note_id: &str, note: &Note) -> Result<String> {
    let front = &note.front;
    // 添付ファイル名も検索対象に(「あの PDF どこだっけ」を引けるように。FR-C8)
    let attach_names: String = vault
        .list_attachments(note_id)?
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    Ok(note_search_text(
        front.title.as_deref(),
        front.description.as_deref(),
        &front.tags.join(" "),
        front
            .authority
            .as_ref()
            .map(|authority| authority.namespace.as_str()),
        front
            .authority
            .as_ref()
            .map(|authority| authority.scope.as_str()),
        &attach_names,
        &note.body,
    ))
}

struct NoteRow {
    id: String,
    title: Option<String>,
    description: Option<String>,
    tags: String,
    namespace: Option<String>,
    scope: Option<String>,
    body: String,
}

fn all_note_rows(conn: &Connection) -> Result<Vec<NoteRow>> {
    let mut statement = conn.prepare(
        "SELECT id, title, description, coalesce(tags, ''), namespace, authority_scope, body
         FROM notes ORDER BY id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(NoteRow {
            id: row.get(0)?,
            title: row.get(1)?,
            description: row.get(2)?,
            tags: row.get(3)?,
            namespace: row.get(4)?,
            scope: row.get(5)?,
            body: row.get(6)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn fts_main_rebuild(vault: &Vault, conn: &Connection) -> Result<RebuildOutcome> {
    conn.execute("DELETE FROM fts_main", [])?;
    let mut insert = conn.prepare("INSERT INTO fts_main(id, text) VALUES(?1, ?2)")?;
    for note in all_note_rows(conn)? {
        let attach_names: String = vault
            .list_attachments(&note.id)?
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let text = note_search_text(
            note.title.as_deref(),
            note.description.as_deref(),
            &note.tags,
            note.namespace.as_deref(),
            note.scope.as_deref(),
            &attach_names,
            &note.body,
        );
        insert.execute(rusqlite::params![note.id, wakati(&text)])?;
    }
    Ok(RebuildOutcome::Ready)
}

fn fts_tri_rebuild(vault: &Vault, conn: &Connection) -> Result<RebuildOutcome> {
    conn.execute("DELETE FROM fts_tri", [])?;
    let mut insert = conn.prepare("INSERT INTO fts_tri(id, text) VALUES(?1, ?2)")?;
    for note in all_note_rows(conn)? {
        let attach_names: String = vault
            .list_attachments(&note.id)?
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let text = note_search_text(
            note.title.as_deref(),
            note.description.as_deref(),
            &note.tags,
            note.namespace.as_deref(),
            note.scope.as_deref(),
            &attach_names,
            &note.body,
        );
        insert.execute(rusqlite::params![note.id, text])?;
    }
    Ok(RebuildOutcome::Ready)
}

fn links_rebuild(_vault: &Vault, conn: &Connection) -> Result<RebuildOutcome> {
    conn.execute("DELETE FROM links", [])?;
    let mut insert = conn.prepare("INSERT OR IGNORE INTO links(src, dst) VALUES(?1, ?2)")?;
    for note in all_note_rows(conn)? {
        for link in crate::index::extract_link_entries(&note.id, &note.body) {
            insert.execute(rusqlite::params![note.id, link.dst])?;
        }
    }
    Ok(RebuildOutcome::Ready)
}

fn fts_anchor_rebuild(_vault: &Vault, conn: &Connection) -> Result<RebuildOutcome> {
    crate::index::rebuild_anchor_index_from_notes(conn)?;
    Ok(RebuildOutcome::Ready)
}

/// typed relationの正本はnote frontmatter(notes.document)。全noteをparseして
/// 台帳を作り直す。documentが空のuid付きnoteは復元不能としてrebuild失敗にする
/// (governanceの穴を黙って空edgeで隠さない)。
fn note_relations_rebuild(_vault: &Vault, conn: &Connection) -> Result<RebuildOutcome> {
    conn.execute("DELETE FROM note_relations", [])?;
    let rows: Vec<(String, Option<String>, String)> = {
        let mut statement = conn.prepare("SELECT id, note_uid, document FROM notes ORDER BY id")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut insert = conn.prepare(
        "INSERT OR IGNORE INTO note_relations(src_uid, kind, target_uid) VALUES(?1, ?2, ?3)",
    )?;
    for (id, note_uid, document) in rows {
        let Some(note_uid) = note_uid else {
            continue; // uidを持たないlegacy noteはrelationの端点になれない
        };
        let note = Note::parse(&document)
            .with_context(|| format!("relation台帳の復元元documentをparseできない: {id}"))?;
        for relation in &note.front.relations {
            insert.execute(rusqlite::params![
                note_uid,
                relation.kind.as_str(),
                relation.target.as_str()
            ])?;
        }
    }
    Ok(RebuildOutcome::Ready)
}

/// 埋め込みは同期再計算できない(モデル未導入があり得る・計算が重い)。
/// objectの復元だけ行い、行は既存のembed_step(cap付き)が追い付く。
fn note_vecs_rebuild(_vault: &Vault, conn: &Connection) -> Result<RebuildOutcome> {
    if !crate::embed::model_installed() {
        return Ok(RebuildOutcome::CapabilityUnavailable);
    }
    let pending: i64 = conn.query_row("SELECT count(*) FROM notes", [], |row| row.get(0))?;
    Ok(RebuildOutcome::Pending(pending.max(0) as usize))
}

// ---------------------------------------------------------------- apply_change

fn fts_main_apply(
    vault: &Vault,
    conn: &Connection,
    change: NoteChange<'_>,
    cache: &ChangeCache,
) -> Result<()> {
    match change {
        NoteChange::Upsert { note_id, note, .. } => {
            conn.execute("DELETE FROM fts_main WHERE id=?1", [note_id])?;
            let text = cache.search_text(vault, note_id, note)?;
            conn.execute(
                "INSERT INTO fts_main(id, text) VALUES(?1, ?2)",
                rusqlite::params![note_id, wakati(text)],
            )?;
        }
        NoteChange::Remove { note_id, .. } => {
            conn.execute("DELETE FROM fts_main WHERE id=?1", [note_id])?;
        }
    }
    Ok(())
}

fn fts_tri_apply(
    vault: &Vault,
    conn: &Connection,
    change: NoteChange<'_>,
    cache: &ChangeCache,
) -> Result<()> {
    match change {
        NoteChange::Upsert { note_id, note, .. } => {
            conn.execute("DELETE FROM fts_tri WHERE id=?1", [note_id])?;
            let text = cache.search_text(vault, note_id, note)?;
            conn.execute(
                "INSERT INTO fts_tri(id, text) VALUES(?1, ?2)",
                rusqlite::params![note_id, text],
            )?;
        }
        NoteChange::Remove { note_id, .. } => {
            conn.execute("DELETE FROM fts_tri WHERE id=?1", [note_id])?;
        }
    }
    Ok(())
}

fn links_apply(
    _vault: &Vault,
    conn: &Connection,
    change: NoteChange<'_>,
    cache: &ChangeCache,
) -> Result<()> {
    match change {
        NoteChange::Upsert { note_id, note, .. } => {
            // upsertはsource行だけを置き換える(dst側は他noteのsource行)。
            conn.execute("DELETE FROM links WHERE src=?1", [note_id])?;
            for link in cache.links(note_id, note) {
                conn.execute(
                    "INSERT OR IGNORE INTO links(src, dst) VALUES(?1, ?2)",
                    rusqlite::params![note_id, link.dst],
                )?;
            }
        }
        NoteChange::Remove { note_id, .. } => {
            conn.execute("DELETE FROM links WHERE src=?1 OR dst=?1", [note_id])?;
        }
    }
    Ok(())
}

fn fts_anchor_apply(
    _vault: &Vault,
    conn: &Connection,
    change: NoteChange<'_>,
    cache: &ChangeCache,
) -> Result<()> {
    match change {
        NoteChange::Upsert {
            note_id,
            previously_indexed,
            note,
            ..
        } => {
            // srcはFTS上でUNINDEXEDなので全走査になる。新規ノートには旧rowが存在しない
            // ため省略し、更新時だけ削除することで10k初回rebuildを二次時間にしない。
            if previously_indexed {
                conn.execute("DELETE FROM fts_anchor WHERE src=?1", [note_id])?;
            }
            for link in cache.links(note_id, note) {
                if !link.anchor.is_empty() {
                    conn.execute(
                        "INSERT INTO fts_anchor(src, dst, text) VALUES(?1, ?2, ?3)",
                        rusqlite::params![note_id, link.dst, wakati(&link.anchor)],
                    )?;
                }
            }
        }
        NoteChange::Remove { note_id, .. } => {
            conn.execute("DELETE FROM fts_anchor WHERE src=?1 OR dst=?1", [note_id])?;
        }
    }
    Ok(())
}

fn note_relations_apply(
    _vault: &Vault,
    conn: &Connection,
    change: NoteChange<'_>,
    _cache: &ChangeCache,
) -> Result<()> {
    match change {
        NoteChange::Upsert {
            previous_uid, note, ..
        } => {
            if let Some(previous_uid) = previous_uid {
                conn.execute(
                    "DELETE FROM note_relations WHERE src_uid=?1",
                    [previous_uid],
                )?;
            }
            if let Some(uid) = &note.front.note_uid {
                for relation in &note.front.relations {
                    conn.execute(
                        "INSERT OR IGNORE INTO note_relations(src_uid, kind, target_uid)
                         VALUES(?1, ?2, ?3)",
                        rusqlite::params![
                            uid.as_str(),
                            relation.kind.as_str(),
                            relation.target.as_str()
                        ],
                    )?;
                }
            }
        }
        NoteChange::Remove { note_id, note_uid } => {
            if let Some(note_uid) = note_uid {
                // inbound typed relationがある間は削除自体を拒否する(参照切れ防止)。
                let source: Option<String> = conn
                    .query_row(
                        "SELECT source.id FROM note_relations relation
                         JOIN notes source ON source.note_uid = relation.src_uid
                         WHERE relation.target_uid = ?1 LIMIT 1",
                        [note_uid],
                        |row| row.get(0),
                    )
                    .optional()?;
                if let Some(source) = source {
                    bail!("typed relationの参照先は削除できない: {source} -> {note_id}");
                }
                conn.execute("DELETE FROM note_relations WHERE src_uid=?1", [note_uid])?;
            }
        }
    }
    Ok(())
}

fn note_vecs_apply(
    _vault: &Vault,
    conn: &Connection,
    change: NoteChange<'_>,
    _cache: &ChangeCache,
) -> Result<()> {
    match change {
        NoteChange::Upsert { note_id, note, .. } => {
            // title/description/bodyのどれかが変われば同transactionで無効化する
            // (旧実装はbodyのみ比較 — R1 A-5のstaleバグ)。旧形式stampも
            // ここで落ち、embed_stepのpendingへ合流する。
            let expected = crate::embed::embedding_stamp(&crate::embed::embedding_input(
                note.front.title.as_deref(),
                note.front.description.as_deref(),
                &note.body,
            ));
            conn.execute(
                "DELETE FROM note_vecs WHERE id=?1 AND (stamp IS NULL OR stamp<>?2)",
                rusqlite::params![note_id, expected],
            )?;
        }
        NoteChange::Remove { note_id, .. } => {
            conn.execute("DELETE FROM note_vecs WHERE id=?1", [note_id])?;
        }
    }
    Ok(())
}

/// ノート1件の変化を全artifactへ反映する。durable(notes行・note_exports)は
/// 呼び出し元が同じtransaction内で処理する。
pub(crate) fn apply_note_change(
    vault: &Vault,
    conn: &Connection,
    change: NoteChange<'_>,
) -> Result<()> {
    let cache = ChangeCache::default();
    for artifact in DerivedArtifact::ALL {
        let spec = artifact.spec();
        (spec.apply_change)(vault, conn, change, &cache)
            .with_context(|| format!("派生索引 {artifact} を更新できない"))?;
    }
    Ok(())
}

// ---------------------------------------------------------------- self-repair

/// open時のhealth check+修復の結果。openを失敗させない劣化はここへ集める。
#[derive(Debug, Default)]
pub struct RepairReport {
    pub recovered: Vec<DerivedArtifact>,
    pub degraded: Vec<Degradation>,
    pub write_blockers: Vec<DerivedArtifact>,
}

/// artifact単位transactionでDROP→CREATE→rebuild(→governanceはvalidate)する。
/// 失敗はtransaction dropで自動rollbackされ、durable stateは変化しない。
pub(crate) fn force_rebuild(
    vault: &Vault,
    conn: &Connection,
    artifact: DerivedArtifact,
) -> Result<RebuildOutcome> {
    let spec = artifact.spec();
    let transaction = conn.unchecked_transaction()?;
    for object in spec.objects {
        let drop_sql = match object.kind {
            ObjectKind::Table | ObjectKind::VirtualTable => {
                format!("DROP TABLE IF EXISTS {}", object.name)
            }
            ObjectKind::Index => format!("DROP INDEX IF EXISTS {}", object.name),
        };
        transaction.execute_batch(&drop_sql)?;
        transaction.execute_batch(object.create_sql)?;
    }
    let outcome = (spec.rebuild)(vault, &transaction)?;
    if spec.criticality == Criticality::GovernanceCritical {
        crate::index::validate_authority_index(&transaction)?;
    }
    transaction.commit()?;
    Ok(outcome)
}

/// open時の安価なhealth checkと自己修復(spec S-3の修復契約)。
///
/// - primary/optional repair成功 → open成功、今回だけrecovery notice、通常検索
/// - primary/optional repair失敗 → repairをrollback、open成功、fallback検索+劣化
/// - governance repair+validate成功 → write可
/// - governance repair/validate失敗 → read/search継続、note writeはfail-closed
///
/// health OKなら書込を一切行わない(no-writes不変はテストで固定)。
pub(crate) fn check_and_repair(vault: &Vault, conn: &Connection) -> Result<RepairReport> {
    let mut report = RepairReport::default();
    for artifact in DerivedArtifact::ALL {
        let spec = artifact.spec();
        let health = match (spec.health)(vault, conn) {
            Ok(health) => health,
            Err(error) => ArtifactHealth::Broken {
                detail: format!("health checkに失敗: {error:#}"),
            },
        };
        let ArtifactHealth::Broken { detail } = health else {
            continue;
        };
        match force_rebuild(vault, conn, artifact) {
            Ok(outcome) => {
                report.recovered.push(artifact);
                report.degraded.push(Degradation::IndexRecovered {
                    artifact: artifact.name().to_string(),
                    detail,
                });
                if let RebuildOutcome::Pending(remaining) = outcome {
                    report
                        .degraded
                        .push(Degradation::EmbeddingIndexPending { remaining });
                }
            }
            Err(error) => {
                report.degraded.push(Degradation::IndexRepair {
                    artifact: artifact.name().to_string(),
                    detail: format!("{detail} / 修復失敗: {error:#}"),
                });
                if spec.criticality == Criticality::GovernanceCritical {
                    report.write_blockers.push(artifact);
                    report.degraded.push(Degradation::GovernanceWriteBlocked {
                        detail: format!("{artifact} を修復できないためnote書込を停止中"),
                    });
                }
            }
        }
    }
    // governance台帳がhealth(=validate_authority_index込み)または修復+validateを
    // 通過した接続にだけmarkerを置き、write時のfull再検証を省く。修復失敗時は
    // markerが無いままなので、write側は毎回full検証してfail-closedになる。
    if !report
        .write_blockers
        .contains(&DerivedArtifact::NoteRelations)
    {
        mark_governance_validated(conn)?;
    }
    Ok(report)
}

/// open時のgovernance検証成功をこの接続に記録するmarker。
///
/// TEMP tableは接続ローカル(temp databaseに置かれ、DBファイルへは何も残らない)
/// なので、spec S-3の「永続の通知済み状態を持たない・毎openで再検査」を保ったまま
/// 接続単位の検証結果を運べる。10k notes規模で `validate_authority_index` は
/// 1 write あたり約2 ms(gov_x100実測208 ms)かかり、毎write実行は単一note更新の
/// median予算(baseline比+15%)を超過したため、full検証はopen時(check_and_repair)
/// と未検証接続のwriteに限定する。
const GOVERNANCE_MARKER: &str = "governance_validated_at_open";

fn mark_governance_validated(conn: &Connection) -> Result<()> {
    conn.execute_batch(&format!(
        "CREATE TEMP TABLE IF NOT EXISTS {GOVERNANCE_MARKER}(ok INTEGER)"
    ))?;
    Ok(())
}

fn governance_validated_at_open(conn: &Connection) -> Result<bool> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_temp_schema WHERE type='table' AND name=?1",
            [GOVERNANCE_MARKER],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

/// note write(queue_put / delete)の先頭で呼ぶfail-closedゲート。
/// governance台帳(note_relations)が欠損・不整合の間、noteの書込を拒否する。
///
/// - object存在・種別は毎write検査する(open後のDROP等をその場で止める)
/// - 内容整合(`validate_authority_index`)はopen時のhealth check/修復で検証済みの
///   接続では省略する(markerはこの接続限り)。open時検証を通っていない接続
///   (生Connection・修復失敗後)は毎writeでfull検証し、fail-closedを維持する
/// - write自体の整合はupsert後の `validate_authority_write` が同transactionで検査する
///
/// 永続の「通知済み」状態は持たない(毎openで再検査)。
pub(crate) fn require_governance_ready(conn: &Connection) -> Result<()> {
    for object in NOTE_RELATIONS.objects {
        if let ArtifactHealth::Broken { detail } = object_health(conn, object)? {
            bail!("governance台帳が壊れているためnoteを書き込めない(fail-closed): {detail}");
        }
    }
    if governance_validated_at_open(conn)? {
        return Ok(());
    }
    crate::index::validate_authority_index(conn)
        .context("governance台帳が不整合のためnoteを書き込めない(fail-closed)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{
        Authority, AuthorityRole, AuthorityStatus, NoteNamespace, NoteRelation, RelationKind,
    };
    use crate::index::test_support::{durable_rows_snapshot, table_rows};
    use crate::index::{open_db, open_db_with_outcome};
    use crate::vault::{NoteProposal, Vault};

    fn open_raw(vault: &Vault) -> Connection {
        Connection::open(vault.index_db_path()).unwrap()
    }

    fn degraded_codes(degraded: &[Degradation]) -> Vec<&'static str> {
        degraded.iter().map(|item| item.code()).collect()
    }

    /// registryは派生索引だけを所有できる。durable state(正本)のtable名は
    /// objectに現れない — 修復・rebuildが正本へ触る経路を型とテストの両方で塞ぐ。
    #[test]
    fn registry_cannot_own_durable_state_tables() {
        let mut seen = std::collections::HashSet::new();
        for artifact in DerivedArtifact::ALL {
            let spec = artifact.spec();
            assert!(!spec.objects.is_empty(), "{artifact}: objectが空");
            for object in spec.objects {
                assert!(
                    !DURABLE_STATE_TABLES.contains(&object.name),
                    "{artifact}: durable table {} を派生registryへ登録している",
                    object.name
                );
                assert!(seen.insert(object.name), "object名が重複: {}", object.name);
            }
        }
    }

    /// 実事故再現(1): meta=8のままfts_main/fts_triが欠落 → open時に自己修復し、
    /// 検索が復活する。2回目のopenは健全なので通知は消える。
    #[test]
    fn missing_primary_fts_tables_are_rebuilt_and_search_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let id = vault
            .propose_for_test(
                "認証設計の決定",
                "quartz falcon ledger を採用する。",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        drop(open_db(&vault).unwrap());
        open_raw(&vault)
            .execute_batch("DROP TABLE fts_main; DROP TABLE fts_tri;")
            .unwrap();

        let outcome = open_db_with_outcome(&vault).unwrap();
        assert!(outcome.recovered.contains(&DerivedArtifact::FtsMain));
        assert!(outcome.recovered.contains(&DerivedArtifact::FtsTri));
        assert!(outcome.write_blockers.is_empty());
        assert!(
            degraded_codes(&outcome.degraded)
                .iter()
                .all(|code| *code == "index_recovered"),
            "{:?}",
            outcome.degraded
        );

        let found = crate::search::search(&outcome.conn, "quartz falcon ledger", 5);
        assert!(found.degraded.is_empty(), "{:?}", found.degraded);
        assert!(found.hits.iter().any(|hit| hit.id == id));
        drop(outcome);

        let second = open_db_with_outcome(&vault).unwrap();
        assert!(
            second.recovered.is_empty(),
            "健全なopenは通知を繰り返さない"
        );
        assert!(second.degraded.is_empty());
    }

    /// 実事故再現(2): fts_anchorだけ欠落 → 修復され、リンク文言検索が戻る。
    #[test]
    fn missing_fts_anchor_alone_is_rebuilt() {
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
        open_raw(&vault)
            .execute_batch("DROP TABLE fts_anchor;")
            .unwrap();

        let outcome = open_db_with_outcome(&vault).unwrap();
        assert_eq!(outcome.recovered, vec![DerivedArtifact::FtsAnchor]);
        assert!(outcome.write_blockers.is_empty());
        let dst: String = outcome
            .conn
            .query_row(
                "SELECT dst FROM fts_anchor WHERE fts_anchor MATCH ?1",
                [crate::tokenize::match_expr("blue comet policy")],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(dst, "notes/kepler");
    }

    /// fts側のid NULL行はNOT IN形カバレッジ検査の盲点(全行NULL化で欠落・迷子
    /// とも0件に見える)だった。NULL行の存在下でも実在する欠落を検出し、
    /// NULL行自体も迷子として修復されることを固定する(レビュー指摘)。
    #[test]
    fn null_id_rows_do_not_blind_the_fts_coverage_check() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let id = vault
            .propose_for_test(
                "盲点検査",
                "amber vole compass の記録。",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        drop(open_db(&vault).unwrap());
        let raw = open_raw(&vault);
        raw.execute("DELETE FROM fts_main WHERE id=?1", [&id])
            .unwrap();
        raw.execute("INSERT INTO fts_main(id, text) VALUES(NULL, 'ghost')", [])
            .unwrap();
        drop(raw);

        let outcome = open_db_with_outcome(&vault).unwrap();
        assert!(
            outcome.recovered.contains(&DerivedArtifact::FtsMain),
            "{:?}",
            outcome.degraded
        );
        let null_rows: i64 = outcome
            .conn
            .query_row(
                "SELECT count(*) FROM fts_main WHERE id IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(null_rows, 0);
        let found = crate::search::search(&outcome.conn, "amber vole compass", 5);
        assert!(found.hits.iter().any(|hit| hit.id == id));
    }

    /// S-3のstamp形式検査: 現行複合形でないstamp行(旧形式・他producer)は
    /// open時のhealth checkで検出され、修復(drop→再作成)でembed pendingへ
    /// 回る。行の欠落(未埋め込み)は正常なので修復を繰り返さない。
    #[test]
    fn malformed_stamp_rows_are_detected_and_cleared_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let id = vault
            .propose_for_test("旧stamp行", "本文", None, &["test".into()], "test/client")
            .unwrap();
        open_raw(&vault)
            .execute(
                "INSERT OR REPLACE INTO note_vecs(id, stamp, embedding) VALUES(?1, ?2, ?3)",
                rusqlite::params![
                    id,
                    crate::embed::EMBED_PRODUCER_STAMP, // 旧: producer単体形式
                    crate::embed::to_blob(&[1.0, 0.0])
                ],
            )
            .unwrap();

        let outcome = open_db_with_outcome(&vault).unwrap();
        assert!(
            outcome.recovered.contains(&DerivedArtifact::NoteVecs),
            "{:?}",
            outcome.degraded
        );
        let rows: i64 = outcome
            .conn
            .query_row("SELECT count(*) FROM note_vecs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 0, "旧形式stamp行が残っている");
        drop(outcome);

        // 空のnote_vecs(未埋め込み)は正常 — 次のopenは修復を繰り返さない
        let second = open_db_with_outcome(&vault).unwrap();
        assert!(second.recovered.is_empty(), "{:?}", second.degraded);
    }

    fn propose_supports_pair(vault: &Vault) -> (String, String, String) {
        let conn = open_db(vault).unwrap();
        let target = vault
            .propose(
                &conn,
                NoteProposal {
                    title: "参照される決定",
                    body: "本文",
                    description: None,
                    tags: &["test".into()],
                    authority: Authority {
                        namespace: NoteNamespace::Knowledge,
                        role: AuthorityRole::Canonical,
                        status: AuthorityStatus::Active,
                        scope: "test/governance-target".into(),
                    },
                    relations: Vec::new(),
                    allow_new_tags: true,
                    client: "test/client",
                },
            )
            .unwrap();
        let target_uid = crate::note_store::read(&conn, &target)
            .unwrap()
            .front
            .note_uid
            .unwrap();
        let source = vault
            .propose(
                &conn,
                NoteProposal {
                    title: "根拠を参照するノート",
                    body: "silver heron ballast の根拠。",
                    description: None,
                    tags: &["test".into()],
                    authority: Authority {
                        namespace: NoteNamespace::Knowledge,
                        role: AuthorityRole::Canonical,
                        status: AuthorityStatus::Active,
                        scope: "test/governance-source".into(),
                    },
                    relations: vec![NoteRelation {
                        kind: RelationKind::Supports,
                        target: target_uid.clone(),
                    }],
                    allow_new_tags: true,
                    client: "test/client",
                },
            )
            .unwrap();
        (target, source, target_uid.to_string())
    }

    /// 実事故再現(3)成功側: note_relations欠落 → frontmatter(document)から
    /// 再構築+validate成功 → write可。
    #[test]
    fn missing_note_relations_is_rebuilt_from_documents_and_writes_resume() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let (_target, source, target_uid) = propose_supports_pair(&vault);
        open_raw(&vault)
            .execute_batch("DROP TABLE note_relations;")
            .unwrap();

        let outcome = open_db_with_outcome(&vault).unwrap();
        assert!(outcome.recovered.contains(&DerivedArtifact::NoteRelations));
        assert!(outcome.write_blockers.is_empty());
        let restored: (String, String) = outcome
            .conn
            .query_row(
                "SELECT kind, target_uid FROM note_relations LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(restored, ("supports".to_owned(), target_uid));

        // governance修復+validate成功後はnote writeが通る
        let mut note = crate::note_store::read(&outcome.conn, &source).unwrap();
        note.body = "silver heron ballast の根拠(追記)。".into();
        crate::note_store::put(
            &vault,
            &outcome.conn,
            &source,
            &note,
            "update",
            "update note",
        )
        .unwrap();
    }

    /// 実事故再現(3)失敗側: 再構築してもvalidateが通らないgovernance台帳は
    /// rollbackされ、read/searchは継続、note writeだけがfail-closedになる。
    #[test]
    fn governance_repair_failure_blocks_note_writes_but_keeps_search() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let (target, source, _target_uid) = propose_supports_pair(&vault);
        // 参照先noteの行だけを直接失わせる(relation edgeとsource documentは残る)
        // — 再構築は同じdangling edgeを作り、validateが必ず失敗する状態。
        let raw = open_raw(&vault);
        raw.execute("DELETE FROM notes WHERE id=?1", [&target])
            .unwrap();
        raw.execute("DELETE FROM fts_main WHERE id=?1", [&target])
            .unwrap();
        raw.execute("DELETE FROM fts_tri WHERE id=?1", [&target])
            .unwrap();
        drop(raw);

        let outcome = open_db_with_outcome(&vault).unwrap();
        assert_eq!(outcome.write_blockers, vec![DerivedArtifact::NoteRelations]);
        let codes = degraded_codes(&outcome.degraded);
        assert!(codes.contains(&"index_repair"), "{codes:?}");
        assert!(codes.contains(&"governance_write_blocked"), "{codes:?}");

        // read/searchは継続する
        let found = crate::search::search(&outcome.conn, "silver heron ballast", 5);
        assert!(found.hits.iter().any(|hit| hit.id == source));

        // note write(upsert・delete両方)はfail-closed
        let mut note = crate::note_store::read(&outcome.conn, &source).unwrap();
        note.body = "更新できないはず".into();
        let error = crate::note_store::put(
            &vault,
            &outcome.conn,
            &source,
            &note,
            "update",
            "update note",
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("fail-closed"), "{error:#}");
        let error =
            crate::note_store::delete(&vault, &outcome.conn, &source, "delete", "delete note")
                .unwrap_err();
        assert!(format!("{error:#}").contains("fail-closed"), "{error:#}");
    }

    /// 実事故再現(5): 修復途中の失敗はそのartifactだけrollbackされ、durable行は
    /// 1 bitも変わらない。先に成功したartifact(fts_main)は維持される。
    #[test]
    fn repair_failure_rolls_back_that_artifact_and_leaves_durable_rows_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let id = vault
            .propose_for_test(
                "修復途中失敗",
                "copper lynx meridian の記録。",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let raw = open_raw(&vault);
        // fts_tri の名前を先取りするindex — repairのCREATE VIRTUAL TABLEだけが失敗する
        raw.execute_batch(
            "DROP TABLE fts_main; DROP TABLE fts_tri;
             CREATE INDEX fts_tri ON notes(id);",
        )
        .unwrap();
        let durable_before = durable_rows_snapshot(&raw);
        drop(raw);

        let outcome = open_db_with_outcome(&vault).unwrap();
        assert_eq!(outcome.recovered, vec![DerivedArtifact::FtsMain]);
        assert!(
            outcome.write_blockers.is_empty(),
            "retrieval失敗はwriteを止めない"
        );
        let codes = degraded_codes(&outcome.degraded);
        assert!(codes.contains(&"index_repair"), "{codes:?}");

        // durable論理digest不変
        assert_eq!(durable_before, durable_rows_snapshot(&outcome.conn));

        // fts_mainは復活済み、fts_tri経路はfallback(検索は劣化付きで続行)
        let found = crate::search::search(&outcome.conn, "copper lynx meridian", 5);
        assert!(found.hits.iter().any(|hit| hit.id == id));
        assert!(
            found
                .degraded
                .iter()
                .any(|item| item.code() == "rescue_search"),
            "{:?}",
            found.degraded
        );
    }

    /// 増分維持(upsert/update/delete)と一括rebuildが全artifactで同じ論理行に
    /// 到達する(spec S-5のrebuild同値性)。
    #[test]
    fn incremental_maintenance_matches_bulk_rebuild_for_every_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let (_target, source, _uid) = propose_supports_pair(&vault);
        let conn = open_db(&vault).unwrap();
        let linked = vault
            .propose_for_test(
                "リンク元",
                "See [blue comet policy](/notes/kepler.md) と [別名](../kepler2.md)。",
                Some("説明文つき"),
                &["test".into()],
                "test/client",
            )
            .unwrap();
        // 添付ファイル名もfts本文に入る — 増分とrebuildの両方が同じ名前列を見るか
        let attach_dir = dir.path().join("v").join(format!("{linked}.files"));
        std::fs::create_dir_all(&attach_dir).unwrap();
        std::fs::write(attach_dir.join("仕様.pdf"), b"pdf").unwrap();
        vault
            .agent_update_note(
                &conn,
                crate::vault::NoteUpdate {
                    id: &linked,
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
        // 削除経路も増分walkを通す
        let doomed = vault
            .propose_for_test(
                "消えるノート",
                "一時的な本文",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        crate::note_store::delete(&vault, &conn, &doomed, "delete", "delete note").unwrap();
        let _ = source;

        let dumps = |conn: &Connection| {
            vec![
                table_rows(conn, "fts_main", "id, text"),
                table_rows(conn, "fts_tri", "id, text"),
                table_rows(conn, "links", "src, dst"),
                table_rows(conn, "fts_anchor", "src, dst, text"),
                table_rows(conn, "note_relations", "src_uid, kind, target_uid"),
                table_rows(conn, "note_vecs", "id, stamp"),
            ]
        };
        let incremental = dumps(&conn);
        for artifact in DerivedArtifact::ALL {
            let outcome = force_rebuild(&vault, &conn, artifact).unwrap();
            if artifact == DerivedArtifact::NoteVecs {
                // テストビルドはモデル未導入 — 行の同値は「空=空」で成立し、
                // rebuildは能力未導入を正しく報告する
                assert_eq!(outcome, RebuildOutcome::CapabilityUnavailable);
            } else {
                assert_eq!(outcome, RebuildOutcome::Ready, "{artifact}");
            }
        }
        assert_eq!(
            incremental,
            dumps(&conn),
            "増分と一括rebuildの論理行が一致しない"
        );
    }

    /// governanceゲートは接続状態に依存しない: openの後で台帳が壊れても、
    /// 次のnote writeがその場でfail-closedになる。
    #[test]
    fn writes_fail_closed_when_the_ledger_breaks_mid_session() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let id = vault
            .propose_for_test("生きた接続", "本文", None, &["test".into()], "test/client")
            .unwrap();
        let conn = open_db(&vault).unwrap();
        conn.execute_batch("DROP TABLE note_relations;").unwrap();

        let mut note = crate::note_store::read(&conn, &id).unwrap();
        note.body = "書けないはず".into();
        let error =
            crate::note_store::put(&vault, &conn, &id, &note, "update", "update note").unwrap_err();
        assert!(format!("{error:#}").contains("fail-closed"), "{error:#}");
        // readは影響を受けない
        assert_eq!(crate::note_store::read(&conn, &id).unwrap().body, "本文\n");
    }

    /// governance内容のfull再検証(`validate_authority_index`)はopen単位。
    /// open時検証を通っていない生接続は毎writeでfull検証しfail-closedのまま、
    /// open済み接続はmarkerで素通りする(単一note更新の性能予算S-5)。壊れた
    /// 内容は次のopenの自己修復がdocument正本から再構築する。
    #[test]
    fn governance_content_revalidation_happens_at_open_not_per_write() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let id = vault
            .propose_for_test(
                "接続単位検証",
                "本文",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();

        // 生接続(open時検証なし)へdangling relationを注入 → write時に検出される
        let raw = open_raw(&vault);
        let src_uid: String = raw
            .query_row("SELECT note_uid FROM notes WHERE id=?1", [&id], |row| {
                row.get(0)
            })
            .unwrap();
        raw.execute(
            "INSERT INTO note_relations(src_uid, kind, target_uid) VALUES(?1, 'supports', ?2)",
            rusqlite::params![src_uid, "u-nowhere"],
        )
        .unwrap();
        let mut note = crate::note_store::read(&raw, &id).unwrap();
        note.body = "生接続では書けないはず".into();
        let error =
            crate::note_store::put(&vault, &raw, &id, &note, "update", "update note").unwrap_err();
        assert!(format!("{error:#}").contains("fail-closed"), "{error:#}");
        drop(raw);

        // 次のopenは自己修復がdocument正本からnote_relationsを再構築し、markerを置く
        let outcome = open_db_with_outcome(&vault).unwrap();
        assert_eq!(outcome.recovered, vec![DerivedArtifact::NoteRelations]);
        assert!(outcome.write_blockers.is_empty());

        // open済み接続のwriteは通る(full再検証はopen時に済んでいる)
        note.body = "open済み接続では書ける".into();
        crate::note_store::put(&vault, &outcome.conn, &id, &note, "update", "update note").unwrap();
        assert_eq!(
            crate::note_store::read(&outcome.conn, &id).unwrap().body,
            "open済み接続では書ける\n"
        );
    }

    /// S-4: title/description/bodyのどの変更でも埋め込み行が同一transactionで
    /// 無効化され、embed_stepのpendingへ落ちる(旧実装はbodyのみ — R1 A-5)。
    #[test]
    fn any_field_change_invalidates_the_embedding_row() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let id = vault
            .propose_for_test(
                "最初の題",
                "変わらない本文",
                Some("最初の説明"),
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let conn = open_db(&vault).unwrap();
        let seed = |conn: &Connection| {
            let (title, description, body): (Option<String>, Option<String>, String) = conn
                .query_row(
                    "SELECT title, description, body FROM notes WHERE id=?1",
                    [&id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            let stamp = crate::embed::embedding_stamp(&crate::embed::embedding_input(
                title.as_deref(),
                description.as_deref(),
                &body,
            ));
            conn.execute(
                "INSERT OR REPLACE INTO note_vecs(id, stamp, embedding) VALUES(?1, ?2, ?3)",
                rusqlite::params![id, stamp, crate::embed::to_blob(&[1.0, 0.0])],
            )
            .unwrap();
        };
        let vec_count = |conn: &Connection| -> i64 {
            conn.query_row("SELECT count(*) FROM note_vecs WHERE id=?1", [&id], |row| {
                row.get(0)
            })
            .unwrap()
        };
        let update = |field: &'static str, value: &'static str| crate::vault::NoteUpdate {
            id: &id,
            title: (field == "title").then_some(value),
            body: (field == "body").then_some(value),
            description: (field == "description").then_some(value),
            tags: None,
            authority: None,
            relations: None,
            allow_new_tags: false,
            client: "test/client",
        };

        // title-only変更 → pending
        seed(&conn);
        assert_eq!(crate::embed::embed_pending(&conn, 0).unwrap(), 0);
        vault
            .agent_update_note(&conn, update("title", "変えた題"))
            .unwrap();
        assert_eq!(vec_count(&conn), 0, "title変更で埋め込みが無効化されない");
        assert_eq!(crate::embed::embed_pending(&conn, 0).unwrap(), 1);

        // description-only変更 → pending
        seed(&conn);
        vault
            .agent_update_note(&conn, update("description", "変えた説明"))
            .unwrap();
        assert_eq!(
            vec_count(&conn),
            0,
            "description変更で埋め込みが無効化されない"
        );

        // body変更 → pending(従来からの動作)
        seed(&conn);
        vault
            .agent_update_note(&conn, update("body", "変えた本文"))
            .unwrap();
        assert_eq!(vec_count(&conn), 0);

        // 内容が変わらない再indexでは保持される(mtime精度移行で再埋め込みの嵐を
        // 起こさない従来動作の維持)。1回目のputでbody表現(末尾改行)を正規化して
        // から測る — 正規化差は旧実装でも「本文変更」扱いだった。
        let note = crate::note_store::read(&conn, &id).unwrap();
        crate::note_store::put(&vault, &conn, &id, &note, "touch", "touch note").unwrap();
        seed(&conn);
        let note = crate::note_store::read(&conn, &id).unwrap();
        crate::note_store::put(&vault, &conn, &id, &note, "touch", "touch note").unwrap();
        assert_eq!(
            vec_count(&conn),
            1,
            "内容不変の再indexが埋め込みを捨てている"
        );
    }

    /// S-4: 旧形式stamp(producer単体)の行は全write経路とは独立に「全pending」
    /// になり、knnからも除外される。再埋め込み後(複合stamp)は対象に戻る。
    #[test]
    fn old_format_stamps_are_pending_and_excluded_from_knn() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let id = vault
            .propose_for_test(
                "旧stampのノート",
                "本文",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let conn = open_db(&vault).unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO note_vecs(id, stamp, embedding) VALUES(?1, ?2, ?3)",
            rusqlite::params![
                id,
                crate::embed::EMBED_PRODUCER_STAMP, // 旧: producer単体形式
                crate::embed::to_blob(&[1.0, 0.0])
            ],
        )
        .unwrap();

        assert_eq!(
            crate::embed::embed_pending(&conn, 0).unwrap(),
            1,
            "旧stampはpendingとして数えられる"
        );
        assert!(
            crate::embed::knn(&conn, &[1.0, 0.0], 5).unwrap().is_empty(),
            "旧stampの行がknnに混ざっている"
        );

        // 再埋め込み(複合stamp)後は対象に戻る
        let (title, description, body): (Option<String>, Option<String>, String) = conn
            .query_row(
                "SELECT title, description, body FROM notes WHERE id=?1",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let stamp = crate::embed::embedding_stamp(&crate::embed::embedding_input(
            title.as_deref(),
            description.as_deref(),
            &body,
        ));
        conn.execute(
            "INSERT OR REPLACE INTO note_vecs(id, stamp, embedding) VALUES(?1, ?2, ?3)",
            rusqlite::params![id, stamp, crate::embed::to_blob(&[1.0, 0.0])],
        )
        .unwrap();
        assert_eq!(crate::embed::embed_pending(&conn, 0).unwrap(), 0);
        let neighbors = crate::embed::knn(&conn, &[1.0, 0.0], 5).unwrap();
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0].0, id);
    }

    /// sync/import経由の外部編集でも(queue_putだけでなく)無効化が効く —
    /// 新バイナリ内の全write経路がapply_change走査を通る検証。
    #[test]
    fn markdown_import_path_also_invalidates_stale_embeddings() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let id = vault
            .propose_for_test(
                "外部編集対象",
                "本文",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let conn = open_db(&vault).unwrap();
        vault.flush_note_exports(&conn).unwrap();
        let (title, description, body): (Option<String>, Option<String>, String) = conn
            .query_row(
                "SELECT title, description, body FROM notes WHERE id=?1",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let stamp = crate::embed::embedding_stamp(&crate::embed::embedding_input(
            title.as_deref(),
            description.as_deref(),
            &body,
        ));
        conn.execute(
            "INSERT OR REPLACE INTO note_vecs(id, stamp, embedding) VALUES(?1, ?2, ?3)",
            rusqlite::params![id, stamp, crate::embed::to_blob(&[1.0, 0.0])],
        )
        .unwrap();

        let mut note = vault.read_note(&id).unwrap();
        note.front.title = Some("外部で変えた題".into());
        std::thread::sleep(std::time::Duration::from_millis(2));
        vault.write_note_fixture(&id, &note).unwrap();
        let report = crate::index::import_markdown_snapshot(&vault, &conn).unwrap();
        assert!(report.degraded.is_empty());
        let count: i64 = conn
            .query_row("SELECT count(*) FROM note_vecs WHERE id=?1", [&id], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "import経路のtitle変更が埋め込みを無効化しない");
    }
}
