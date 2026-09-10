//! 発話ごとの全件plannerを避ける派生キャッシュ。正本変更はDB triggerで失効させる。
//! checkpointの受入やexecutorの再照合には使わない。期限・受入state・Artifact変更印は毎回読み直す。

use anyhow::{Result, ensure};
use rusqlite::Connection;
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::derived_index::{
    ArtifactHealth, ArtifactSpec, Criticality, Lane, ObjectKind, RebuildOutcome, SqliteObjectSpec,
};
use crate::distillation::{DistillationPlan, DistillationSignal};
use crate::distillation_cadence::DistillationCadenceStatus;
use crate::vault::Vault;

const FORMAT: &str = "kb-app.cadence-cache/v1";

pub(crate) static SPEC: ArtifactSpec = ArtifactSpec {
    lane: Lane::Routine,
    criticality: Criticality::RetrievalOptional,
    objects: OBJECTS,
    health,
    rebuild: |_, _| Ok(RebuildOutcome::Ready),
    // import・GUI・同期・executor・旧binaryのSQLもDB常駐triggerが拾う。
    apply_change: |_, _, _, _| Ok(()),
};

const OBJECTS: &[SqliteObjectSpec] = &[
    SqliteObjectSpec {
        name: "cadence_cache",
        kind: ObjectKind::Table,
        create_sql: "CREATE TABLE cadence_cache (singleton INTEGER PRIMARY KEY CHECK(singleton=1), generation TEXT NOT NULL DEFAULT (lower(hex(randomblob(16)))), notes_revision INTEGER NOT NULL, cached_revision INTEGER, format TEXT, planner_profile TEXT, checkpoint_id TEXT, lineage_count INTEGER); INSERT INTO cadence_cache(singleton, notes_revision) VALUES(1, 0);",
    },
    SqliteObjectSpec {
        name: "notes_revision_insert",
        kind: ObjectKind::Trigger,
        create_sql: "CREATE TRIGGER notes_revision_insert AFTER INSERT ON notes BEGIN UPDATE cadence_cache SET notes_revision=notes_revision+1 WHERE singleton=1; END",
    },
    SqliteObjectSpec {
        name: "notes_revision_update",
        kind: ObjectKind::Trigger,
        create_sql: "CREATE TRIGGER notes_revision_update AFTER UPDATE ON notes BEGIN UPDATE cadence_cache SET notes_revision=notes_revision+1 WHERE singleton=1; END",
    },
    SqliteObjectSpec {
        name: "notes_revision_delete",
        kind: ObjectKind::Trigger,
        create_sql: "CREATE TRIGGER notes_revision_delete AFTER DELETE ON notes BEGIN UPDATE cadence_cache SET notes_revision=notes_revision+1 WHERE singleton=1; END",
    },
    SqliteObjectSpec {
        name: "note_relations_revision_insert",
        kind: ObjectKind::Trigger,
        create_sql: "CREATE TRIGGER note_relations_revision_insert AFTER INSERT ON note_relations BEGIN UPDATE cadence_cache SET notes_revision=notes_revision+1 WHERE singleton=1; END",
    },
    SqliteObjectSpec {
        name: "note_relations_revision_update",
        kind: ObjectKind::Trigger,
        create_sql: "CREATE TRIGGER note_relations_revision_update AFTER UPDATE ON note_relations BEGIN UPDATE cadence_cache SET notes_revision=notes_revision+1 WHERE singleton=1; END",
    },
    SqliteObjectSpec {
        name: "note_relations_revision_delete",
        kind: ObjectKind::Trigger,
        create_sql: "CREATE TRIGGER note_relations_revision_delete AFTER DELETE ON note_relations BEGIN UPDATE cadence_cache SET notes_revision=notes_revision+1 WHERE singleton=1; END",
    },
];

fn health(vault: &Vault, conn: &Connection) -> Result<ArtifactHealth> {
    let _ = vault;
    let objects = crate::derived_index::objects_health(conn, OBJECTS)?;
    if matches!(objects, ArtifactHealth::Broken { .. }) {
        return Ok(objects);
    }
    conn.query_row("SELECT notes_revision, cached_revision, format, planner_profile, checkpoint_id, lineage_count FROM cadence_cache WHERE singleton=1", [], |_| Ok(()))?;
    Ok(ArtifactHealth::Ready)
}

pub(crate) fn revision(conn: &Connection) -> Result<i64> {
    require_revision_structure(conn)?;
    Ok(conn.query_row(
        "SELECT notes_revision FROM cadence_cache WHERE singleton=1",
        [],
        |r| r.get(0),
    )?)
}

fn require_revision_structure(conn: &Connection) -> Result<()> {
    // 欠けたtriggerの間に変更された可能性があるcacheは再利用しない。
    ensure!(
        matches!(
            crate::derived_index::objects_health(conn, OBJECTS)?,
            ArtifactHealth::Ready
        ),
        "cadence cacheの構造が不正"
    );
    Ok(())
}

/// ノート・関連のcommitを識別する、大小比較しない変更トークン。
/// 修復時のrevision巻き戻りとJavaScriptの整数精度に依存しないよう文字列で返す。
pub fn note_revision(conn: &Connection) -> Result<String> {
    let snapshot = conn.unchecked_transaction()?;
    require_revision_structure(&snapshot)?;
    let token = snapshot.query_row(
        "SELECT generation, notes_revision FROM cadence_cache WHERE singleton=1",
        [],
        |row| {
            let generation: String = row.get(0)?;
            let revision: i64 = row.get(1)?;
            Ok(format!("{generation}:{revision}"))
        },
    )?;
    snapshot.rollback()?;
    Ok(token)
}

#[derive(Debug)]
pub(crate) struct PreparedCache {
    revision: i64,
    generation: String,
    checkpoint: String,
    lineage_count: i64,
}

pub(crate) fn prepare(conn: &Connection, plan: &DistillationPlan) -> Result<PreparedCache> {
    Ok(PreparedCache {
        revision: revision(conn)?,
        generation: conn.query_row(
            "SELECT generation FROM cadence_cache WHERE singleton=1",
            [],
            |r| r.get(0),
        )?,
        checkpoint: crate::distillation_audit::DistillationCheckpoint::from_plan(plan)
            .checkpoint_id,
        lineage_count: i64::try_from(
            plan.entries
                .iter()
                .filter(|entry| {
                    entry
                        .signals
                        .contains(&DistillationSignal::RecordWithoutCanonicalLineage)
                })
                .count(),
        )?,
    })
}

pub(crate) fn publish(conn: &Connection, cache: &PreparedCache) -> Result<()> {
    // read snapshot終了後の別process更新を取り込んだrevisionへ、古い計算を貼らない。
    conn.execute("UPDATE cadence_cache SET cached_revision=?1, format=?2, planner_profile=?3, checkpoint_id=?4, lineage_count=?5 WHERE singleton=1 AND notes_revision=?1 AND generation=?6", rusqlite::params![cache.revision, FORMAT, crate::distillation::PLANNER_PROFILE, cache.checkpoint, cache.lineage_count, cache.generation])?;
    Ok(())
}

fn cached(conn: &Connection) -> Result<Option<(String, i64)>> {
    let current = revision(conn)?;
    use rusqlite::OptionalExtension as _;
    Ok(conn.query_row("SELECT checkpoint_id, lineage_count FROM cadence_cache WHERE singleton=1 AND cached_revision=?1 AND format=?2 AND planner_profile=?3", rusqlite::params![current, FORMAT, crate::distillation::PLANNER_PROFILE], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)
}

/// 通常検索・書込の後で温める。hook子MCPはこの関数を呼ばない。
pub(crate) fn refresh(conn: &Connection) -> Result<()> {
    let snapshot = conn.unchecked_transaction()?;
    if cached(&snapshot)?.is_some() {
        return Ok(());
    }
    let plan = crate::distillation::plan_in_transaction(&snapshot)?;
    let prepared = prepare(&snapshot, &plan)?;
    snapshot.rollback()?;
    publish(conn, &prepared)
}

#[derive(Debug, Serialize, serde::Deserialize)]
pub struct CadenceDigest {
    pub schema: String,
    pub digest: String,
    pub status: DistillationCadenceStatus,
    pub records_without_canonical_lineage: i64,
}

/// cache missは未確認として返し、取得済み本文の配送を全件plannerで遅らせない。
pub(crate) fn read(vault: &Vault, conn: &Connection) -> Result<Option<CadenceDigest>> {
    let Some((checkpoint, lineage)) = cached(conn)? else {
        return Ok(None);
    };
    // Artifactだけの昇格・rollbackでnotes_revisionが不変でも、変更印はcache外で確認する。
    let status = crate::distillation_cadence::status_for_checkpoint(vault, &checkpoint)?;
    Ok(Some(from_status(status, lineage)?))
}

pub(crate) fn from_status(
    status: DistillationCadenceStatus,
    lineage: i64,
) -> Result<CadenceDigest> {
    let mut material = serde_json::to_value(&status)?;
    // 毎回変わる観測時刻はdigestから除く。期限到来・受入・失敗の変化は含める。
    material
        .as_object_mut()
        .expect("status object")
        .remove("checked_at");
    let bytes = serde_json::to_vec(&(material, lineage))?;
    Ok(CadenceDigest {
        schema: "kb-app.cadence-digest/v1".into(),
        digest: format!("{:x}", Sha256::digest(bytes)),
        status,
        records_without_canonical_lineage: lineage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (tempfile::TempDir, Vault, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        (dir, vault, conn)
    }

    /// 2026-09-08: aliasだけの変更はplannerのcacheを失効させず、hookへ要監査を返す。
    #[test]
    fn metadata_changes_make_cached_status_due_without_invalidating_note_revision() {
        let (_dir, vault, conn) = setup();
        let accepted = crate::distillation_cadence::run(
            &vault,
            &conn,
            crate::distillation_cadence::DistillationCadenceRunArguments::default(),
        )
        .unwrap();
        assert!(accepted.accepted);
        refresh(&conn).unwrap();
        let revision_before = note_revision(&conn).unwrap();
        let cache_before = cached(&conn).unwrap();
        let before = read(&vault, &conn).unwrap().unwrap();
        assert!(before.status.due_lanes().is_empty());
        let root = vault.root.join(crate::ledger::DIR);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("aliases.json"), "{}").unwrap();
        let changed = read(&vault, &conn).unwrap().unwrap();
        assert_eq!(
            changed.status.due_lanes(),
            vec![crate::distillation_cadence::DistillationCadenceLane::AfterWrite]
        );
        assert_eq!(
            changed.status.current_checkpoint_id,
            before.status.current_checkpoint_id
        );
        assert_ne!(changed.digest, before.digest);
        assert_eq!(note_revision(&conn).unwrap(), revision_before);
        assert_eq!(cached(&conn).unwrap(), cache_before);
        std::fs::remove_file(root.join("aliases.json")).unwrap();
        assert!(
            read(&vault, &conn)
                .unwrap()
                .unwrap()
                .status
                .due_lanes()
                .is_empty()
        );
        let reference = crate::artifact::ArtifactRef::new(
            &crate::workspace::stored_workspace_id(&vault).unwrap(),
            "fixture".parse().unwrap(),
            crate::artifact::ArtifactId::new(1),
        );
        std::fs::create_dir_all(root.join("refs")).unwrap();
        std::fs::write(
            root.join("refs/fixture.json"),
            serde_json::to_vec(&reference).unwrap(),
        )
        .unwrap();
        let ref_changed = read(&vault, &conn).unwrap().unwrap();
        assert_eq!(
            ref_changed.status.due_lanes(),
            vec![crate::distillation_cadence::DistillationCadenceLane::AfterWrite]
        );
        assert_eq!(
            ref_changed.status.current_checkpoint_id,
            before.status.current_checkpoint_id
        );
        assert_eq!(note_revision(&conn).unwrap(), revision_before);
        assert_eq!(cached(&conn).unwrap(), cache_before);
        std::fs::write(root.join("aliases.json"), "{broken").unwrap();
        assert!(read(&vault, &conn).is_err());
        assert_eq!(cached(&conn).unwrap(), cache_before);
    }

    /// 2026-09-08: 起動済みGUIは別MCP接続の保存を、接続を開き直さず検知する。
    #[test]
    fn note_revision_observes_other_connection_commits_and_keeps_rollbacks_invisible() {
        let (_dir, vault, writer) = setup();
        let reader = crate::index::open_db_read_only(&vault).unwrap();
        let count = || crate::search::browsable_note_count(&reader).unwrap();
        let initial = note_revision(&reader).unwrap();
        assert_eq!(count(), 0);

        let tx = writer.unchecked_transaction().unwrap();
        tx.execute("INSERT INTO notes(id,title,status,normal_reference_allowed) VALUES('notes/a','保存前','stable',1)", [])
            .unwrap();
        assert_eq!(note_revision(&reader).unwrap(), initial);
        assert_eq!(count(), 0);
        tx.commit().unwrap();

        let created = note_revision(&reader).unwrap();
        assert_ne!(created, initial);
        assert_eq!(count(), 1);
        writer
            .execute("UPDATE notes SET title='保存後' WHERE id='notes/a'", [])
            .unwrap();
        let updated = note_revision(&reader).unwrap();
        assert_ne!(updated, created, "件数不変の本文・metadata更新も拾う");

        writer
            .execute(
                "UPDATE notes SET normal_reference_allowed=0 WHERE id='notes/a'",
                [],
            )
            .unwrap();
        let hidden = note_revision(&reader).unwrap();
        assert_ne!(hidden, updated);
        assert_eq!(count(), 0);

        let tx = writer.unchecked_transaction().unwrap();
        tx.execute(
            "UPDATE notes SET normal_reference_allowed=1 WHERE id='notes/a'",
            [],
        )
        .unwrap();
        assert_eq!(note_revision(&reader).unwrap(), hidden);
        tx.rollback().unwrap();
        assert_eq!(note_revision(&reader).unwrap(), hidden);
        assert_eq!(count(), 0);

        writer
            .execute(
                "UPDATE notes SET normal_reference_allowed=1 WHERE id='notes/a'",
                [],
            )
            .unwrap();
        let restored = note_revision(&reader).unwrap();
        assert_ne!(restored, hidden);
        assert_eq!(count(), 1);
        writer
            .execute("DELETE FROM notes WHERE id='notes/a'", [])
            .unwrap();
        assert_ne!(note_revision(&reader).unwrap(), restored);
        assert_eq!(count(), 0);
    }

    /// 2026-09-08: 修復でrevisionが0へ戻っても同じ版に見せず、欠損中は変更なしと偽らない。
    #[test]
    fn note_revision_rejects_missing_triggers_and_changes_after_repair() {
        let (_dir, vault, writer) = setup();
        let reader = crate::index::open_db_read_only(&vault).unwrap();
        let before = note_revision(&reader).unwrap();
        writer
            .execute_batch("DROP TRIGGER notes_revision_insert")
            .unwrap();
        assert!(note_revision(&reader).is_err());
        crate::derived_index::force_rebuild(
            &vault,
            &writer,
            crate::derived_index::DerivedArtifact::CadenceCache,
        )
        .unwrap();
        assert_ne!(note_revision(&reader).unwrap(), before);
        assert_eq!(revision(&writer).unwrap(), 0);
    }

    /// 2026-09-08: 軽量な変更検知でplannerを実行せず、キャッシュの温め直しを変更と数えない。
    #[test]
    fn note_revision_is_read_only_and_ignores_cache_metadata_writes() {
        let (_dir, vault, writer) = setup();
        writer
            .execute_batch("INSERT INTO notes(id,document) VALUES('notes/a','unparseable fixture')")
            .unwrap();
        let reader = crate::index::open_db_read_only(&vault).unwrap();
        let before = note_revision(&reader).unwrap();
        writer
            .execute(
                "UPDATE cadence_cache SET cached_revision=notes_revision, checkpoint_id='fixture'",
                [],
            )
            .unwrap();
        assert_eq!(note_revision(&reader).unwrap(), before);
        writer
            .execute(
                "UPDATE cadence_cache SET notes_revision=9007199254740993",
                [],
            )
            .unwrap();
        assert!(
            note_revision(&reader)
                .unwrap()
                .ends_with(":9007199254740993")
        );
        assert_eq!(reader.total_changes(), 0);
    }

    /// 2026-09-05: APIを経由しないimport・同期のSQLとtransaction rollbackも失効対象。
    #[test]
    fn triggers_cover_notes_and_relations_and_rollback_is_atomic() {
        let (_dir, _vault, conn) = setup();
        let start = revision(&conn).unwrap();
        conn.execute_batch("INSERT INTO notes(id, note_uid) VALUES('notes/a','uid-a'); UPDATE notes SET title='changed'; INSERT INTO note_relations(src_uid, kind, target_uid) VALUES('uid-a','mentions','uid-b'); UPDATE note_relations SET target_uid='uid-c'; DELETE FROM note_relations; DELETE FROM notes;").unwrap();
        assert_eq!(revision(&conn).unwrap(), start + 6);
        let tx = conn.unchecked_transaction().unwrap();
        tx.execute_batch("INSERT INTO notes(id) VALUES('notes/rollback')")
            .unwrap();
        assert_eq!(revision(&tx).unwrap(), start + 7);
        tx.rollback().unwrap();
        assert_eq!(revision(&conn).unwrap(), start + 6);
    }

    /// 2026-09-05: hookのcache missは本文parseや全件plannerを走らせない。
    #[test]
    fn cold_cache_and_stale_cache_never_parse_notes() {
        let (_dir, _vault, conn) = setup();
        let start = revision(&conn).unwrap();
        assert!(cached(&conn).unwrap().is_none());
        publish(
            &conn,
            &PreparedCache {
                revision: start,
                generation: conn
                    .query_row("SELECT generation FROM cadence_cache", [], |r| r.get(0))
                    .unwrap(),
                checkpoint: "fixture".into(),
                lineage_count: 2,
            },
        )
        .unwrap();
        assert_eq!(cached(&conn).unwrap(), Some(("fixture".into(), 2)));
        conn.execute_batch("INSERT INTO notes(id,document) VALUES('notes/broken','unparseable')")
            .unwrap();
        assert!(cached(&conn).unwrap().is_none());
        // 古いsnapshotの計算完了が後から来ても新しいrevisionのcacheにはならない。
        publish(
            &conn,
            &PreparedCache {
                revision: start,
                generation: conn
                    .query_row("SELECT generation FROM cadence_cache", [], |r| r.get(0))
                    .unwrap(),
                checkpoint: "stale".into(),
                lineage_count: 0,
            },
        )
        .unwrap();
        assert!(cached(&conn).unwrap().is_none());
    }

    #[test]
    fn missing_trigger_invalidates_cache_and_registry_repair_keeps_notes() {
        let (_dir, vault, conn) = setup();
        refresh(&conn).unwrap();
        assert!(cached(&conn).unwrap().is_some());
        conn.execute_batch("DROP TRIGGER notes_revision_insert")
            .unwrap();
        assert!(cached(&conn).is_err());
        crate::derived_index::force_rebuild(
            &vault,
            &conn,
            crate::derived_index::DerivedArtifact::CadenceCache,
        )
        .unwrap();
        assert!(cached(&conn).unwrap().is_none());
        refresh(&conn).unwrap();
        assert!(cached(&conn).unwrap().is_some());
    }
    #[test]
    fn cache_repair_generation_rejects_inflight_old_snapshot() {
        let (_dir, vault, conn) = setup();
        let plan = crate::distillation::plan(&conn).unwrap();
        let prepared = prepare(&conn, &plan).unwrap();
        crate::derived_index::force_rebuild(
            &vault,
            &conn,
            crate::derived_index::DerivedArtifact::CadenceCache,
        )
        .unwrap();
        publish(&conn, &prepared).unwrap();
        assert!(cached(&conn).unwrap().is_none());
    }

    #[test]
    fn ten_thousand_notes_do_not_expand_cache_read_work() {
        let (_dir, _vault, conn) = setup();
        conn.execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000) INSERT INTO notes(id, document) SELECT 'notes/'||x, 'malformed fixture' FROM n").unwrap();
        let prepared = PreparedCache {
            revision: revision(&conn).unwrap(),
            generation: conn
                .query_row("SELECT generation FROM cadence_cache", [], |r| r.get(0))
                .unwrap(),
            checkpoint: "fixture".into(),
            lineage_count: 10000,
        };
        publish(&conn, &prepared).unwrap();
        let started = std::time::Instant::now();
        for _ in 0..100 {
            assert_eq!(cached(&conn).unwrap(), Some(("fixture".into(), 10000)));
        }
        eprintln!("10k notes / 100 cached reads: {:?}", started.elapsed());
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
    }
}
