//! 派生artifactのformat/generation framework(R4 I-4、統合core C-4)。
//!
//! schema versionを動かさずに派生artifactを追加するための互換性機構。durable
//! schema versionは「durable/runtime構造の互換性」だけを宣言し、派生objectの
//! 互換性はartifactごとのformat key・generation・retirement状態で管理する。
//!
//! **coreの既存6 artifact(`derived_index::DerivedArtifact`)へは適用しない。**
//! ここにあるのは機構とテストだけで、最初の利用者は評価用ブランチの
//! `fts_entry_v1` / `note_context_v1` / `link_anchors_v1` になる(R4 I-5)。
//! 既存registry名・durable table名の使用は`validate_artifact_name`が拒否する。
//!
//! 3つの必須規約(R4 I-4のamend条件):
//!
//! 1. **format key**(`meta` の `artifact_format:<name>`): readerは
//!    `check_availability` で宣言formatと期待formatの一致を確認してからだけ
//!    queryする。非互換format変更は同名objectを上書きせず、versioned object名
//!    (例 `fts_entry_v1` → `fts_entry_v2`)で行う(docs/derived-artifact-lifecycle.md)。
//! 2. **dirty generation barrier**: registryが所有するSQLite trigger群が
//!    `notes` のINSERT/UPDATE/DELETEでartifactをdirty化する。triggerはDB内に
//!    住むため**旧バイナリのwriteでも発火する**。dirty artifactは検索利用不可
//!    (NotReady)で、readerはbaselineへfallbackし劣化(`artifact_not_ready`)を
//!    明示する。rebuild完了時は同一transactionで`publish_ready_generation`を
//!    呼び、dirty解除とready generationのpublishを不可分にする。
//! 3. **二段階retirement**: 廃止は Retired(新readerは無視・objectは残置)→
//!    GC-eligible(明示maintenanceの`gc_drop`でのみDROP)の二段階。registryは
//!    未知objectを絶対にDROPしない(`derived_index`のテストで既存経路も固定)。
//!
//! generation値は共有sequence(`artifact_generation_seq`)から採番する。複数
//! artifactを同じ`publish_ready_generation`で発行すると同一generationになり、
//! composite capability(例: ContextCard = note_context && link_anchors &&
//! same_source_generation)の「別generationを混ぜない」要求(R4 §2)を
//! `composite_readiness` が機械的に判定できる。

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension};

use crate::degradation::Degradation;

const FORMAT_KEY_PREFIX: &str = "artifact_format:";
const GENERATION_KEY_PREFIX: &str = "artifact_generation:";
const DIRTY_KEY_PREFIX: &str = "artifact_dirty:";
const RETIREMENT_KEY_PREFIX: &str = "artifact_retirement:";
/// 共有のgeneration採番sequence。同時publishしたartifact群へ同じ値を配る。
const GENERATION_SEQ_KEY: &str = "artifact_generation_seq";

/// dirty barrier triggerの命名。`<prefix><artifact>_<insert|update|delete>`。
const BARRIER_TRIGGER_PREFIX: &str = "artifact_barrier_";
const BARRIER_EVENTS: [&str; 3] = ["insert", "update", "delete"];

// ---------------------------------------------------------------- 命名規律

/// framework配下のartifact名を検証する。SQL識別子・meta keyへ安全に埋め込める
/// 形だけを許し、durable table・coreの既存registry objectの名前は拒否する
/// (= coreの既存6 artifactへこの機構を適用できないことの機械的強制)。
pub fn validate_artifact_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let head_ok = chars.next().is_some_and(|c| c.is_ascii_lowercase());
    if !head_ok
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        bail!("artifact名が不正(英小文字始まりの [a-z0-9_] のみ): {name:?}");
    }
    if name.len() > 64 {
        bail!("artifact名が長すぎる(64文字まで): {name}");
    }
    if crate::derived_index::DURABLE_STATE_TABLES.contains(&name) {
        bail!("durable table {name} はartifact frameworkの対象にできない");
    }
    for artifact in crate::derived_index::DerivedArtifact::ALL {
        for object in artifact.spec().objects {
            if object.name == name {
                bail!("coreの既存registry object {name} へartifact frameworkは適用しない(R4 I-4)");
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- meta鍵

fn meta_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key=?1", [key], |row| {
            row.get(0)
        })
        .optional()?)
}

fn meta_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES(?1, ?2)",
        [key, value],
    )?;
    Ok(())
}

fn meta_delete(conn: &Connection, key: &str) -> Result<()> {
    conn.execute("DELETE FROM meta WHERE key=?1", [key])?;
    Ok(())
}

/// 宣言済みformat(format keyの生読み)。`check_availability` の部品だが、
/// format keyだけを使う軽量guard(query前のformat照合)としても公開する。
pub fn declared_format(conn: &Connection, artifact: &str) -> Result<Option<String>> {
    validate_artifact_name(artifact)?;
    meta_get(conn, &format!("{FORMAT_KEY_PREFIX}{artifact}"))
}

// ---------------------------------------------------------------- barrier

fn barrier_trigger_name(artifact: &str, event: &str) -> String {
    format!("{BARRIER_TRIGGER_PREFIX}{artifact}_{event}")
}

/// dirty generation barrierをinstallする。`notes` のINSERT/UPDATE/DELETEが
/// meta の dirty flag を立てるtrigger 3本。DB内に住むため、この機構を知らない
/// 旧バイナリのwriteでも発火する(それが要点)。冪等。
pub fn install_dirty_barrier(conn: &Connection, artifact: &str) -> Result<()> {
    validate_artifact_name(artifact)?;
    let dirty_key = format!("{DIRTY_KEY_PREFIX}{artifact}");
    for event in BARRIER_EVENTS {
        let trigger = barrier_trigger_name(artifact, event);
        conn.execute_batch(&format!(
            "CREATE TRIGGER IF NOT EXISTS {trigger}
             AFTER {} ON notes
             BEGIN
                 INSERT OR REPLACE INTO meta(key, value) VALUES('{dirty_key}', '1');
             END;",
            event.to_uppercase()
        ))
        .with_context(|| format!("dirty barrier trigger {trigger} を作成できない"))?;
    }
    Ok(())
}

/// barrier triggerを取り外す(GC時のみ想定)。冪等。dirty flag等のmeta鍵は
/// 消さない — 取り外しただけのartifactがReadyに見えてはいけない
/// (`check_availability` はbarrier欠落をNotReadyにする)。
pub fn uninstall_dirty_barrier(conn: &Connection, artifact: &str) -> Result<()> {
    validate_artifact_name(artifact)?;
    for event in BARRIER_EVENTS {
        conn.execute_batch(&format!(
            "DROP TRIGGER IF EXISTS {}",
            barrier_trigger_name(artifact, event)
        ))?;
    }
    Ok(())
}

/// barrier trigger 3本が全て存在するか。
pub fn dirty_barrier_installed(conn: &Connection, artifact: &str) -> Result<bool> {
    validate_artifact_name(artifact)?;
    for event in BARRIER_EVENTS {
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM sqlite_schema WHERE type='trigger' AND name=?1",
                [barrier_trigger_name(artifact, event)],
                |row| row.get(0),
            )
            .optional()?;
        if found.is_none() {
            return Ok(false);
        }
    }
    Ok(true)
}

// ---------------------------------------------------------------- publish

/// rebuild完了時のready generation publish。**rebuild本体と同一transactionの
/// 中で呼ぶ**(autocommit接続では拒否)。列挙した全artifactへ共有sequenceの
/// 同じ新generationを配り、formatを宣言し、dirtyを解除する。
///
/// - barrier未installのartifactは拒否する(dirty barrierなしのversionless
///   machine artifactを作らせない — R4 §4の危険リスト5)
/// - Retired / GC-eligibleのartifactは拒否する(retirement後の再生成
///   ping-pongを作らせない — 同リスト7)
/// - rebuild transactionはimmediateで始めること(deferredはread→write昇格が
///   並行writerとSQLITE_BUSYで衝突し得る。失敗してもdirtyのままなので安全側)
pub fn publish_ready_generation(conn: &Connection, artifacts: &[(&str, &str)]) -> Result<i64> {
    if artifacts.is_empty() {
        bail!("publish対象のartifactが空");
    }
    if conn.is_autocommit() {
        bail!(
            "ready generationのpublishはrebuildと同一transactionの中で行う\
             (autocommit接続では呼べない)"
        );
    }
    for (artifact, _) in artifacts {
        validate_artifact_name(artifact)?;
        if let Some(state) = retirement_state(conn, artifact)? {
            bail!("{artifact} は{state}のためpublishできない(retirement後の再生成禁止)");
        }
        if !dirty_barrier_installed(conn, artifact)? {
            bail!(
                "{artifact} のdirty barrierが未installのためpublishできない\
                 (barrierなしのmachine artifactは許可しない)"
            );
        }
    }
    let next = meta_get(conn, GENERATION_SEQ_KEY)?
        .map(|value| {
            value
                .parse::<i64>()
                .context("generation sequenceが数値でない")
        })
        .transpose()?
        .unwrap_or(0)
        .checked_add(1)
        .context("generation sequenceが上限")?;
    meta_set(conn, GENERATION_SEQ_KEY, &next.to_string())?;
    for (artifact, format) in artifacts {
        meta_set(conn, &format!("{FORMAT_KEY_PREFIX}{artifact}"), format)?;
        meta_set(
            conn,
            &format!("{GENERATION_KEY_PREFIX}{artifact}"),
            &next.to_string(),
        )?;
        meta_set(conn, &format!("{DIRTY_KEY_PREFIX}{artifact}"), "0")?;
    }
    Ok(next)
}

// ---------------------------------------------------------------- reader guard

/// readerがqueryを拒否した理由。`describe()` が劣化detailの正本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotReadyReason {
    /// ready generationが一度もpublishされていない。
    NeverPublished,
    /// dirty barrierが存在しない — staleを検出できないため利用不可。
    BarrierMissing,
    /// 宣言formatが期待formatと一致しない(旧format残置・別producer)。
    FormatMismatch {
        expected: String,
        found: Option<String>,
    },
    /// 最後のpublish以後にnotesが変化した(旧バイナリのwrite含む)。
    Dirty,
    /// Retired — 新readerは無視する(objectは残置)。
    Retired,
    /// GC-eligible — 明示maintenanceの削除待ち。
    GcEligible,
    /// composite: memberのready generationが揃っていない。
    GenerationMismatch,
}

impl NotReadyReason {
    pub fn describe(&self) -> String {
        match self {
            Self::NeverPublished => "ready generationが未publish".into(),
            Self::BarrierMissing => "dirty barrierが未install".into(),
            Self::FormatMismatch { expected, found } => format!(
                "format不一致(期待 {expected} / 宣言 {})",
                found.as_deref().unwrap_or("なし")
            ),
            Self::Dirty => "notes変化後のrebuild待ち(dirty)".into(),
            Self::Retired => "retired(新readerは利用しない)".into(),
            Self::GcEligible => "GC-eligible(削除待ち)".into(),
            Self::GenerationMismatch => "member間でready generationが不一致".into(),
        }
    }
}

/// reader側guardの判定結果。NotReadyのartifactは**queryしない** — baselineへ
/// fallbackし、`degradation_for` の劣化を応答へ載せる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactAvailability {
    Ready { generation: i64 },
    NotReady { reason: NotReadyReason },
}

/// reader側guard。queryの前に必ず呼び、`Ready` のときだけartifact objectへ
/// SQLを発行する。判定順: retirement → barrier → publish済み → format → dirty。
pub fn check_availability(
    conn: &Connection,
    artifact: &str,
    expected_format: &str,
) -> Result<ArtifactAvailability> {
    validate_artifact_name(artifact)?;
    let not_ready = |reason| Ok(ArtifactAvailability::NotReady { reason });
    match retirement_state(conn, artifact)? {
        Some(RetirementState::Retired) => return not_ready(NotReadyReason::Retired),
        Some(RetirementState::GcEligible) => return not_ready(NotReadyReason::GcEligible),
        None => {}
    }
    if !dirty_barrier_installed(conn, artifact)? {
        return not_ready(NotReadyReason::BarrierMissing);
    }
    let Some(generation) = meta_get(conn, &format!("{GENERATION_KEY_PREFIX}{artifact}"))? else {
        return not_ready(NotReadyReason::NeverPublished);
    };
    let generation: i64 = generation
        .parse()
        .with_context(|| format!("{artifact} のgenerationが数値でない"))?;
    let found = meta_get(conn, &format!("{FORMAT_KEY_PREFIX}{artifact}"))?;
    if found.as_deref() != Some(expected_format) {
        return not_ready(NotReadyReason::FormatMismatch {
            expected: expected_format.into(),
            found,
        });
    }
    if meta_get(conn, &format!("{DIRTY_KEY_PREFIX}{artifact}"))?.as_deref() == Some("1") {
        return not_ready(NotReadyReason::Dirty);
    }
    Ok(ArtifactAvailability::Ready { generation })
}

/// NotReadyをbaseline fallbackの劣化として応答へ載せるための変換。
pub fn degradation_for(artifact: &str, reason: &NotReadyReason) -> Degradation {
    Degradation::ArtifactNotReady {
        artifact: artifact.to_string(),
        detail: reason.describe(),
    }
}

// ---------------------------------------------------------------- composite

/// 複数artifactを合成したcapabilityのreadiness(R4 §2)。coreでは未使用 —
/// 評価用ブランチの ContextCardCapability = NoteContext.ready &&
/// LinkAnchors.ready && same_source_generation がこの型で判定する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompositeReadiness {
    /// 全memberがReadyで、同一のready generationを共有している。
    Ready { generation: i64 },
    /// いずれかのmemberがNotReady — capability全体を使わずbaselineへ。
    MemberNotReady {
        member: String,
        reason: NotReadyReason,
    },
    /// memberは個々にReadyだがgenerationが揃っていない — 別generationの
    /// 産物を混ぜず、capability全体をfallbackにする。
    GenerationMismatch { members: Vec<(String, i64)> },
}

/// composite capabilityのreadiness判定。members = (artifact名, 期待format)。
pub fn composite_readiness(
    conn: &Connection,
    members: &[(&str, &str)],
) -> Result<CompositeReadiness> {
    if members.is_empty() {
        bail!("composite capabilityのmemberが空");
    }
    let mut generations: Vec<(String, i64)> = Vec::with_capacity(members.len());
    for (artifact, expected_format) in members {
        match check_availability(conn, artifact, expected_format)? {
            ArtifactAvailability::Ready { generation } => {
                generations.push(((*artifact).to_string(), generation));
            }
            ArtifactAvailability::NotReady { reason } => {
                return Ok(CompositeReadiness::MemberNotReady {
                    member: (*artifact).to_string(),
                    reason,
                });
            }
        }
    }
    let first = generations[0].1;
    if generations
        .iter()
        .any(|(_, generation)| *generation != first)
    {
        return Ok(CompositeReadiness::GenerationMismatch {
            members: generations,
        });
    }
    Ok(CompositeReadiness::Ready { generation: first })
}

// ---------------------------------------------------------------- retirement

/// 二段階retirement(R4 I-4 必須条件3)。mixed-version環境の再生成・再削除
/// ping-pongを避けるため、「registryから外れた = DROP」にはしない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetirementState {
    /// 新readerは無視する。objectとmeta鍵は残置(旧バイナリの再生成を許容)。
    Retired,
    /// compatibility window経過後。明示maintenance(`gc_drop`)でのみDROP可。
    GcEligible,
}

impl std::fmt::Display for RetirementState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Retired => "retired",
            Self::GcEligible => "gc_eligible",
        })
    }
}

pub fn retirement_state(conn: &Connection, artifact: &str) -> Result<Option<RetirementState>> {
    validate_artifact_name(artifact)?;
    match meta_get(conn, &format!("{RETIREMENT_KEY_PREFIX}{artifact}"))?.as_deref() {
        None => Ok(None),
        Some("retired") => Ok(Some(RetirementState::Retired)),
        Some("gc_eligible") => Ok(Some(RetirementState::GcEligible)),
        Some(other) => bail!("{artifact} のretirement状態が不明: {other:?}(fail-closed)"),
    }
}

/// 第一段階: Retired。新readerは以後無視するが、objectは残置する。冪等。
pub fn retire_artifact(conn: &Connection, artifact: &str) -> Result<()> {
    validate_artifact_name(artifact)?;
    if retirement_state(conn, artifact)? == Some(RetirementState::GcEligible) {
        bail!("{artifact} は既にGC-eligible(retiredへは戻せない)");
    }
    meta_set(
        conn,
        &format!("{RETIREMENT_KEY_PREFIX}{artifact}"),
        "retired",
    )
}

/// 第二段階: GC-eligible。Retiredを経ていないartifactには適用できない
/// (compatibility windowを飛ばした即時DROPを禁止する)。
pub fn mark_gc_eligible(conn: &Connection, artifact: &str) -> Result<()> {
    validate_artifact_name(artifact)?;
    match retirement_state(conn, artifact)? {
        Some(RetirementState::Retired) => meta_set(
            conn,
            &format!("{RETIREMENT_KEY_PREFIX}{artifact}"),
            "gc_eligible",
        ),
        Some(RetirementState::GcEligible) => Ok(()),
        None => {
            bail!("{artifact} はretiredを経ていないためGC-eligibleにできない(二段階retirement)")
        }
    }
}

/// GC対象objectの種別(DROP文の種別分け)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetiredObjectKind {
    Table,
    Index,
}

/// 明示maintenanceでのDROP。GC-eligibleのartifactに限り、**そのartifactが
/// 所有すると機械的に判別できる名前**(artifact名そのもの、または
/// `<artifact>_` prefix)のobjectだけを落とす。それ以外の名前は未知object
/// としてDROPを拒否する。barrier triggerとmeta鍵も同一transactionで片付ける。
pub fn gc_drop(
    conn: &Connection,
    artifact: &str,
    objects: &[(&str, RetiredObjectKind)],
) -> Result<()> {
    validate_artifact_name(artifact)?;
    if retirement_state(conn, artifact)? != Some(RetirementState::GcEligible) {
        bail!("{artifact} はGC-eligibleではないためDROPできない(二段階retirement)");
    }
    if conn.is_autocommit() {
        let transaction = conn.unchecked_transaction()?;
        gc_drop_locked(&transaction, artifact, objects)?;
        transaction.commit()?;
        return Ok(());
    }
    gc_drop_locked(conn, artifact, objects)
}

fn gc_drop_locked(
    conn: &Connection,
    artifact: &str,
    objects: &[(&str, RetiredObjectKind)],
) -> Result<()> {
    for (name, kind) in objects {
        if *name != artifact && !name.starts_with(&format!("{artifact}_")) {
            bail!(
                "{name} は {artifact} の所有と判別できないためDROPしない\
                 (registryは未知objectを絶対にDROPしない)"
            );
        }
        // 所有prefix判定を通っても、durable・既存registry名は最終防壁で拒否する。
        validate_artifact_name(name).with_context(|| format!("{name} はGC対象にできない"))?;
        let drop_sql = match kind {
            RetiredObjectKind::Table => format!("DROP TABLE IF EXISTS {name}"),
            RetiredObjectKind::Index => format!("DROP INDEX IF EXISTS {name}"),
        };
        conn.execute_batch(&drop_sql)?;
    }
    uninstall_dirty_barrier(conn, artifact)?;
    for prefix in [
        FORMAT_KEY_PREFIX,
        GENERATION_KEY_PREFIX,
        DIRTY_KEY_PREFIX,
        RETIREMENT_KEY_PREFIX,
    ] {
        meta_delete(conn, &format!("{prefix}{artifact}"))?;
    }
    Ok(())
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::open_db;
    use crate::vault::Vault;

    const ENTRY: &str = "fts_entry_v1";
    const ENTRY_FORMAT: &str = "1";

    fn vault_with_note(dir: &std::path::Path) -> (Vault, String) {
        let vault = Vault::create(dir.join("v")).unwrap();
        let id = vault
            .propose_for_test(
                "barrier対象",
                "quiet ember signal の記録。",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        (vault, id)
    }

    /// 実験artifactのtableを作り、barrier install→rebuild→publishまで進める。
    fn build_and_publish(conn: &Connection) -> i64 {
        conn.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS {ENTRY}(id TEXT PRIMARY KEY, entry TEXT)"
        ))
        .unwrap();
        install_dirty_barrier(conn, ENTRY).unwrap();
        let transaction = conn.unchecked_transaction().unwrap();
        transaction
            .execute_batch(&format!("DELETE FROM {ENTRY}"))
            .unwrap();
        transaction
            .execute(
                &format!("INSERT INTO {ENTRY}(id, entry) SELECT id, body FROM notes"),
                [],
            )
            .unwrap();
        let generation = publish_ready_generation(&transaction, &[(ENTRY, ENTRY_FORMAT)]).unwrap();
        transaction.commit().unwrap();
        generation
    }

    fn availability(conn: &Connection) -> ArtifactAvailability {
        check_availability(conn, ENTRY, ENTRY_FORMAT).unwrap()
    }

    /// C-4-2の要点: barrier triggerはDB内に住むため、この機構を知らない
    /// 旧バイナリ相当(素のSQL)のINSERT/UPDATE/DELETEでもdirty化する。
    /// rebuild+publishで同一transactionのready復帰、generationは単調増加。
    #[test]
    fn plain_sql_note_writes_from_another_connection_mark_the_artifact_dirty() {
        let dir = tempfile::tempdir().unwrap();
        let (vault, id) = vault_with_note(dir.path());
        let conn = open_db(&vault).unwrap();
        let first = build_and_publish(&conn);
        assert_eq!(
            availability(&conn),
            ArtifactAvailability::Ready { generation: first }
        );

        // 旧バイナリ相当: registryを一切通らない生接続の素のSQL UPDATE
        let raw = Connection::open(vault.index_db_path()).unwrap();
        raw.execute(
            "UPDATE notes SET body='旧バイナリが書いた' WHERE id=?1",
            [&id],
        )
        .unwrap();
        assert_eq!(
            availability(&conn),
            ArtifactAvailability::NotReady {
                reason: NotReadyReason::Dirty
            }
        );

        // rebuild+publishでready復帰、generationは進む
        let second = build_and_publish(&conn);
        assert!(second > first);
        assert_eq!(
            availability(&conn),
            ArtifactAvailability::Ready { generation: second }
        );

        // INSERT / DELETE でも発火する
        raw.execute(
            "INSERT INTO notes(id, body, document) VALUES('notes/raw-insert', 'b', '')",
            [],
        )
        .unwrap();
        assert_eq!(
            availability(&conn),
            ArtifactAvailability::NotReady {
                reason: NotReadyReason::Dirty
            }
        );
        build_and_publish(&conn);
        raw.execute("DELETE FROM notes WHERE id='notes/raw-insert'", [])
            .unwrap();
        assert_eq!(
            availability(&conn),
            ArtifactAvailability::NotReady {
                reason: NotReadyReason::Dirty
            }
        );
    }

    /// publishの規律: rebuildと同一transaction必須・barrier必須。
    /// barrierを外しただけのartifactはReadyに見えない(fail-closed)。
    #[test]
    fn publish_requires_a_transaction_and_an_installed_barrier() {
        let dir = tempfile::tempdir().unwrap();
        let (vault, _id) = vault_with_note(dir.path());
        let conn = open_db(&vault).unwrap();

        // autocommit接続では拒否
        let error = publish_ready_generation(&conn, &[(ENTRY, ENTRY_FORMAT)]).unwrap_err();
        assert!(
            format!("{error:#}").contains("同一transaction"),
            "{error:#}"
        );

        // barrier未installでは拒否
        let transaction = conn.unchecked_transaction().unwrap();
        let error = publish_ready_generation(&transaction, &[(ENTRY, ENTRY_FORMAT)]).unwrap_err();
        assert!(format!("{error:#}").contains("dirty barrier"), "{error:#}");
        drop(transaction);

        // publish済みでもbarrierを外すとNotReady(staleを検出できないため)
        build_and_publish(&conn);
        uninstall_dirty_barrier(&conn, ENTRY).unwrap();
        assert_eq!(
            availability(&conn),
            ArtifactAvailability::NotReady {
                reason: NotReadyReason::BarrierMissing
            }
        );
    }

    /// C-4-1: reader guardはformat不一致・未publishのobjectをqueryさせない。
    /// 劣化はartifact_not_readyの安定codeで応答へ載る。
    #[test]
    fn reader_guard_blocks_format_mismatch_and_unpublished_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let (vault, _id) = vault_with_note(dir.path());
        let conn = open_db(&vault).unwrap();

        install_dirty_barrier(&conn, ENTRY).unwrap();
        assert_eq!(
            availability(&conn),
            ArtifactAvailability::NotReady {
                reason: NotReadyReason::NeverPublished
            }
        );

        build_and_publish(&conn);
        assert_eq!(declared_format(&conn, ENTRY).unwrap().as_deref(), Some("1"));
        let mismatch = check_availability(&conn, ENTRY, "2").unwrap();
        let ArtifactAvailability::NotReady { reason } = &mismatch else {
            panic!("format不一致がReadyに見えている: {mismatch:?}");
        };
        assert_eq!(
            *reason,
            NotReadyReason::FormatMismatch {
                expected: "2".into(),
                found: Some("1".into()),
            }
        );
        let degradation = degradation_for(ENTRY, reason);
        assert_eq!(degradation.code(), "artifact_not_ready");
        assert!(degradation.to_string().contains(ENTRY), "{degradation}");
    }

    /// C-4-3: retirementは二段階。Retiredは新readerが無視するがobjectは残置され、
    /// open時の自己修復も触らない。DROPはGC-eligible+明示maintenanceのみ。
    #[test]
    fn retirement_is_two_stage_and_gc_only_drops_owned_objects() {
        let dir = tempfile::tempdir().unwrap();
        let (vault, _id) = vault_with_note(dir.path());
        let conn = open_db(&vault).unwrap();
        build_and_publish(&conn);

        // retiredを経ないGC-eligible化は拒否
        let error = mark_gc_eligible(&conn, ENTRY).unwrap_err();
        assert!(format!("{error:#}").contains("retired"), "{error:#}");

        retire_artifact(&conn, ENTRY).unwrap();
        assert_eq!(
            retirement_state(&conn, ENTRY).unwrap(),
            Some(RetirementState::Retired)
        );
        assert_eq!(
            availability(&conn),
            ArtifactAvailability::NotReady {
                reason: NotReadyReason::Retired
            }
        );
        // retired中のpublish(再生成)は拒否
        let transaction = conn.unchecked_transaction().unwrap();
        let error = publish_ready_generation(&transaction, &[(ENTRY, ENTRY_FORMAT)]).unwrap_err();
        assert!(format!("{error:#}").contains("retired"), "{error:#}");
        drop(transaction);
        // GC-eligible前のDROPは拒否
        let error = gc_drop(&conn, ENTRY, &[(ENTRY, RetiredObjectKind::Table)]).unwrap_err();
        assert!(format!("{error:#}").contains("GC-eligible"), "{error:#}");
        drop(conn);

        // retired objectは通常open(自己修復込み)でも残置される
        let outcome = crate::index::open_db_with_outcome(&vault).unwrap();
        let exists: i64 = outcome
            .conn
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name=?1",
                [ENTRY],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1, "retired objectが通常openで消えた");
        let conn = outcome.conn;

        mark_gc_eligible(&conn, ENTRY).unwrap();
        assert_eq!(
            availability(&conn),
            ArtifactAvailability::NotReady {
                reason: NotReadyReason::GcEligible
            }
        );

        // 所有と判別できない名前はDROP拒否(未知objectを絶対にDROPしない)
        for name in ["notes", "fts_main", "other_table"] {
            let error = gc_drop(&conn, ENTRY, &[(name, RetiredObjectKind::Table)]).unwrap_err();
            assert!(
                format!("{error:#}").contains("DROPしない")
                    || format!("{error:#}").contains("GC対象にできない"),
                "{name}: {error:#}"
            );
        }
        let notes_rows: i64 = conn
            .query_row("SELECT count(*) FROM notes", [], |row| row.get(0))
            .unwrap();
        assert!(notes_rows >= 1, "拒否経路でdurableが消えた");

        gc_drop(&conn, ENTRY, &[(ENTRY, RetiredObjectKind::Table)]).unwrap();
        let leftovers: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name LIKE ?1 OR name = ?2",
                [format!("{BARRIER_TRIGGER_PREFIX}{ENTRY}%"), ENTRY.into()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(leftovers, 0, "GC後にobject/triggerが残っている");
        let keys: i64 = conn
            .query_row(
                "SELECT count(*) FROM meta WHERE key LIKE '%' || ?1",
                [format!(":{ENTRY}")],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(keys, 0, "GC後にmeta鍵が残っている");
        assert_eq!(retirement_state(&conn, ENTRY).unwrap(), None);
    }

    /// R4 §2: composite capabilityは全member Ready かつ同一generationのときだけ
    /// Ready。片方だけ再publishしたらcapability全体がfallbackになる。
    #[test]
    fn composite_capability_requires_members_to_share_a_generation() {
        let dir = tempfile::tempdir().unwrap();
        let (vault, _id) = vault_with_note(dir.path());
        let conn = open_db(&vault).unwrap();
        const CONTEXT: &str = "note_context_v1";
        const ANCHORS: &str = "link_anchors_v1";
        let members: [(&str, &str); 2] = [(CONTEXT, "1"), (ANCHORS, "1")];
        install_dirty_barrier(&conn, CONTEXT).unwrap();
        install_dirty_barrier(&conn, ANCHORS).unwrap();

        // 同時publish → 同一generationでReady
        let transaction = conn.unchecked_transaction().unwrap();
        let generation = publish_ready_generation(&transaction, &members).unwrap();
        transaction.commit().unwrap();
        assert_eq!(
            composite_readiness(&conn, &members).unwrap(),
            CompositeReadiness::Ready { generation }
        );

        // 片方だけ再publish → generation不一致でcapability全体がfallback
        let transaction = conn.unchecked_transaction().unwrap();
        let newer = publish_ready_generation(&transaction, &[(ANCHORS, "1")]).unwrap();
        transaction.commit().unwrap();
        assert_eq!(
            composite_readiness(&conn, &members).unwrap(),
            CompositeReadiness::GenerationMismatch {
                members: vec![(CONTEXT.into(), generation), (ANCHORS.into(), newer)],
            }
        );

        // memberが1つでもNotReadyなら合成もNotReady
        retire_artifact(&conn, CONTEXT).unwrap();
        assert_eq!(
            composite_readiness(&conn, &members).unwrap(),
            CompositeReadiness::MemberNotReady {
                member: CONTEXT.into(),
                reason: NotReadyReason::Retired,
            }
        );
    }

    /// 命名規律: coreのregistry artifactとdurable tableはframeworkの対象外
    /// (「coreの既存artifactへは適用しない」の機械的強制)。
    #[test]
    fn framework_rejects_core_registry_objects_and_durable_tables() {
        for name in [
            "fts_main",
            "fts_tri",
            "links",
            "links_dst",
            "fts_anchor",
            "fts_events",
            "note_relations",
            "note_vecs",
            "notes",
            "meta",
            "note_exports",
        ] {
            assert!(validate_artifact_name(name).is_err(), "{name} が通った");
        }
        for name in ["", "1abc", "Bad", "a-b", "a b", "a;drop"] {
            assert!(validate_artifact_name(name).is_err(), "{name:?} が通った");
        }
        assert!(validate_artifact_name("fts_entry_v1").is_ok());
    }
}
