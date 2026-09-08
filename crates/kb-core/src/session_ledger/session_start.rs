//! SessionStartの観測を、本文を持たない開始証拠として保存する。
//! MCPの親processを証明できないため、同じworkspaceの別開始は並行会話も失効させる。

use super::*;

const START_APPLICATION_ID: i64 = 0x4b42_5353;
const START_SCHEMA_VERSION: i64 = 1;
const FENCE_SQL: &str = "CREATE TABLE workspace_fences (
    workspace_id TEXT PRIMARY KEY NOT NULL,
    generation INTEGER NOT NULL,
    observed_at_ms INTEGER NOT NULL,
    active_session_hash TEXT,
    active_observed_at_ms INTEGER
)";
const IDS_SQL: &str = "CREATE TABLE session_start_ids (
    workspace_id TEXT NOT NULL,
    session_hash TEXT NOT NULL,
    retained_at_ms INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, session_hash)
)";
const IDS_TIME_SQL: &str =
    "CREATE INDEX session_start_ids_time ON session_start_ids(retained_at_ms)";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionStartEvidence {
    /// hostの会話作成時刻ではなく、開始hookがイベントを観測した時刻。
    pub observed_at_ms: i64,
    /// 書込の前後で変わった場合、開始証拠をその書込へ適用しない。
    pub generation: i64,
}

#[derive(Debug)]
struct Fence {
    generation: i64,
    observed_at_ms: i64,
    active_session_hash: Option<String>,
    active_observed_at_ms: Option<i64>,
}

impl Fence {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.generation > 0 && self.observed_at_ms >= 0,
            "開始観測の世代または時刻が不正"
        );
        match (&self.active_session_hash, self.active_observed_at_ms) {
            (Some(hash), Some(at)) => ensure!(
                valid_hash(hash) && at >= 0 && at <= self.observed_at_ms,
                "開始観測の有効なIDまたは時刻が不正"
            ),
            (None, None) => {}
            _ => anyhow::bail!("開始観測の有効状態が不整合"),
        }
        Ok(())
    }
}

fn start_path() -> Result<PathBuf> {
    Ok(runtime_path()?.with_file_name("session-starts.sqlite3"))
}

fn start_session_hash(workspace: &str, session_id: Option<&str>) -> Result<Option<String>> {
    let Some(id) = session_id.filter(|id| !id.is_empty() && id.len() <= 8_192) else {
        return Ok(None);
    };
    let scope = serde_json::to_string(&(ClientSurface::ClaudeCode, Some(workspace)))?;
    Ok(Some(digest(&["session", &scope, id])))
}

/// 呼出元でKBの有効状態と登録workspaceを確認した後にのみ呼ぶ。
pub fn record_session_start(
    workspace_id: &str,
    session_id: Option<&str>,
    source: Option<&str>,
    observed_at_ms: i64,
) -> Result<()> {
    record_session_start_at(
        &start_path()?,
        workspace_id,
        session_id,
        source,
        observed_at_ms,
        now_ms(),
    )
}

/// 保存先の存在確認を除き読取専用。DBがない場合はdirectoryも作らない。
pub fn read_session_start(
    workspace_id: &str,
    session_id: Option<&str>,
) -> Result<Option<SessionStartEvidence>> {
    read_session_start_at(&start_path()?, workspace_id, session_id, now_ms())
}

pub fn record_session_start_at(
    path: &Path,
    workspace_id: &str,
    session_id: Option<&str>,
    source: Option<&str>,
    observed_at_ms: i64,
    now: i64,
) -> Result<()> {
    ensure!(now >= 0, "開始観測の現在時刻が不正");
    let workspace = validate_workspace_id(workspace_id)?;
    let session_hash = start_session_hash(&workspace, session_id)?;
    let valid_time = observed_at_ms >= 0 && observed_at_ms <= now;
    // 入力時刻が不正なイベントでも既存の許可材料を残さない。
    let event_time = if valid_time { observed_at_ms } else { now };
    let parent = path.parent().context("開始観測保存先の親がない")?;
    std::fs::create_dir_all(parent).context("開始観測directoryを作成できない")?;
    let mut conn = Connection::open(path).context("開始観測DBを開けない")?;
    conn.busy_timeout(Duration::from_millis(100))?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    initialize_start_schema(&tx)?;
    let cutoff = now.saturating_sub(RETENTION_DAYS * DAY_MS).max(0);
    tx.execute(
        "DELETE FROM session_start_ids WHERE retained_at_ms < ?1",
        [cutoff],
    )?;
    // 読取は掃除しない。次の記録時には別workspaceの期限切れIDも残さない。
    tx.execute(
        "UPDATE workspace_fences SET active_session_hash=NULL, active_observed_at_ms=NULL
         WHERE active_observed_at_ms < ?1",
        [cutoff],
    )?;

    let previous = read_fence(&tx, &workspace)?;
    let seen = session_hash
        .as_ref()
        .map(|hash| retained_id(&tx, &workspace, hash))
        .transpose()?
        .flatten()
        .is_some();
    let startup = source == Some("startup") && session_hash.is_some() && valid_time;
    let duplicate = startup
        && previous.as_ref().is_some_and(|fence| {
            fence.active_session_hash == session_hash
                && fence.active_observed_at_ms.is_some_and(|at| at >= cutoff)
                && event_time >= fence.observed_at_ms
                && seen
        });

    let next = if duplicate {
        let mut fence = previous.context("重複した開始観測の状態がない")?;
        fence.observed_at_ms = event_time;
        fence
    } else {
        let generation = previous.as_ref().map_or(Ok(1), |fence| {
            fence
                .generation
                .checked_add(1)
                .context("開始観測の世代が上限")
        })?;
        // 同時刻・遅着の別イベントは前後関係を証明できない。既存証拠を失効させるだけ。
        // 並行する別会話も除外されるが、古いMCPのIDを現在の会話と誤認するより保守的にする。
        let ordered = previous
            .as_ref()
            .is_none_or(|fence| event_time > fence.observed_at_ms);
        let can_activate = startup && ordered && !seen && event_time >= cutoff;
        Fence {
            generation,
            observed_at_ms: previous
                .as_ref()
                .map_or(event_time, |fence| fence.observed_at_ms.max(event_time)),
            active_session_hash: can_activate.then(|| session_hash.clone()).flatten(),
            active_observed_at_ms: can_activate.then_some(event_time),
        }
    };
    next.validate()?;
    tx.execute(
        "INSERT INTO workspace_fences(workspace_id, generation, observed_at_ms, active_session_hash, active_observed_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(workspace_id) DO UPDATE SET generation=excluded.generation,
           observed_at_ms=excluded.observed_at_ms, active_session_hash=excluded.active_session_hash,
           active_observed_at_ms=excluded.active_observed_at_ms",
        params![workspace, next.generation, next.observed_at_ms, next.active_session_hash, next.active_observed_at_ms],
    )?;
    if let Some(hash) = session_hash {
        // 保持期間内に見たIDは、resume/clear後にstartupとして届いても復活させない。
        // 90日を超えて消去したIDの再利用は識別できない。世代・時刻のfenceだけは残す。
        tx.execute(
            "INSERT INTO session_start_ids(workspace_id, session_hash, retained_at_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT(workspace_id, session_hash) DO UPDATE SET retained_at_ms=MAX(retained_at_ms, excluded.retained_at_ms)",
            params![workspace, hash, now],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn read_session_start_at(
    path: &Path,
    workspace_id: &str,
    session_id: Option<&str>,
    now: i64,
) -> Result<Option<SessionStartEvidence>> {
    ensure!(now >= 0, "開始観測の現在時刻が不正");
    let workspace = validate_workspace_id(workspace_id)?;
    let Some(hash) = start_session_hash(&workspace, session_id)? else {
        return Ok(None);
    };
    if !path.try_exists()? {
        return Ok(None);
    }
    let mut conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .context("開始観測DBを読めない")?;
    conn.busy_timeout(Duration::from_millis(100))?;
    conn.pragma_update(None, "query_only", true)?;
    let tx = conn.transaction()?;
    validate_start_schema(&tx)?;
    let Some(fence) = read_fence(&tx, &workspace)? else {
        return Ok(None);
    };
    if fence.active_session_hash.as_deref() != Some(&hash) {
        return Ok(None);
    }
    let at = fence
        .active_observed_at_ms
        .context("開始観測の有効時刻がない")?;
    let cutoff = now.saturating_sub(RETENTION_DAYS * DAY_MS).max(0);
    if at < cutoff || fence.observed_at_ms > now {
        return Ok(None);
    }
    let retained_at = retained_id(&tx, &workspace, &hash)?.context("開始観測の有効ID履歴がない")?;
    if retained_at < cutoff || retained_at > now {
        return Ok(None);
    }
    Ok(Some(SessionStartEvidence {
        observed_at_ms: at,
        generation: fence.generation,
    }))
}

fn retained_id(conn: &Connection, workspace: &str, hash: &str) -> Result<Option<i64>> {
    let at = conn
        .query_row(
            "SELECT retained_at_ms FROM session_start_ids WHERE workspace_id=?1 AND session_hash=?2",
            params![workspace, hash],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    ensure!(at.is_none_or(|at| at >= 0), "開始観測の保持時刻が不正");
    Ok(at)
}

fn read_fence(conn: &Connection, workspace: &str) -> Result<Option<Fence>> {
    let fence = conn
        .query_row(
            "SELECT generation, observed_at_ms, active_session_hash, active_observed_at_ms
             FROM workspace_fences WHERE workspace_id=?1",
            [workspace],
            |row| {
                Ok(Fence {
                    generation: row.get(0)?,
                    observed_at_ms: row.get(1)?,
                    active_session_hash: row.get(2)?,
                    active_observed_at_ms: row.get(3)?,
                })
            },
        )
        .optional()?;
    if let Some(fence) = &fence {
        fence.validate()?;
    }
    Ok(fence)
}

fn initialize_start_schema(conn: &Connection) -> Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let app_id: i64 = conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let objects: i64 =
        conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |row| row.get(0))?;
    if version == 0 && app_id == 0 && objects == 0 {
        conn.execute_batch(&format!("{FENCE_SQL};\n{IDS_SQL};\n{IDS_TIME_SQL};"))?;
        conn.pragma_update(None, "application_id", START_APPLICATION_ID)?;
        conn.pragma_update(None, "user_version", START_SCHEMA_VERSION)?;
    }
    validate_start_schema(conn)
}

fn validate_start_schema(conn: &Connection) -> Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let app_id: i64 = conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
    ensure!(
        version == START_SCHEMA_VERSION && app_id == START_APPLICATION_ID,
        "未対応または不正な開始観測schema"
    );
    let mut statement = conn.prepare(
        "SELECT name, sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let actual = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        actual
            == vec![
                ("session_start_ids".into(), IDS_SQL.into()),
                ("session_start_ids_time".into(), IDS_TIME_SQL.into()),
                ("workspace_fences".into(), FENCE_SQL.into()),
            ],
        "開始観測schemaの構造が不一致"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "00000000-0000-0000-0000-000000000001";
    const OTHER_WORKSPACE: &str = "00000000-0000-0000-0000-000000000002";
    const AT: i64 = 100 * DAY_MS;

    fn start(path: &Path, id: Option<&str>, source: Option<&str>, at: i64) {
        record_session_start_at(path, WORKSPACE, id, source, at, at).unwrap();
    }

    fn read(path: &Path, id: &str, at: i64) -> Option<SessionStartEvidence> {
        read_session_start_at(path, WORKSPACE, Some(id), at).unwrap()
    }

    #[test]
    fn mcp_before_hook_can_read_later_without_creating_storage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-created").join("starts.sqlite3");
        assert_eq!(read(&path, "actual-id", AT), None);
        assert!(!path.parent().unwrap().exists());
        start(&path, Some("actual-id"), Some("startup"), AT);
        assert_eq!(
            read(&path, "actual-id", AT),
            Some(SessionStartEvidence {
                observed_at_ms: AT,
                generation: 1,
            })
        );
        assert_eq!(read(&path, "different-id", AT), None);
    }

    #[test]
    fn duplicate_keeps_first_observed_time_and_generation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("starts.sqlite3");
        start(&path, Some("one"), Some("startup"), AT);
        let first = read(&path, "one", AT);
        start(&path, Some("one"), Some("startup"), AT + 1);
        assert_eq!(read(&path, "one", AT + 1), first);
    }

    /// 2026-09-06: MCPが/clear前のIDを保持しても、旧会話の開始を再利用しない。
    #[test]
    fn every_transition_invalidates_and_old_startup_cannot_revive() {
        for source in [
            Some("resume"),
            Some("clear"),
            Some("compact"),
            Some("fork"),
            Some("future"),
            None,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("starts.sqlite3");
            start(&path, Some("old"), Some("startup"), AT);
            start(&path, Some("new"), source, AT + 1);
            assert_eq!(read(&path, "old", AT + 1), None);
            assert_eq!(read(&path, "new", AT + 1), None);
            start(&path, Some("old"), Some("startup"), AT + 2);
            assert_eq!(read(&path, "old", AT + 2), None);
            start(&path, Some("new"), Some("startup"), AT + 3);
            assert_eq!(read(&path, "new", AT + 3), None);
        }
    }

    #[test]
    fn distinct_start_invalidates_parallel_session_and_stale_id_invalidates_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("starts.sqlite3");
        start(&path, Some("one"), Some("startup"), AT);
        start(&path, Some("two"), Some("startup"), AT + 1);
        assert_eq!(read(&path, "one", AT + 1), None);
        assert_eq!(read(&path, "two", AT + 1).unwrap().generation, 2);
        start(&path, Some("one"), Some("startup"), AT + 2);
        assert_eq!(read(&path, "one", AT + 2), None);
        assert_eq!(read(&path, "two", AT + 2), None);
    }

    #[test]
    fn separate_workspaces_and_case_normalization_are_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("starts.sqlite3");
        start(&path, Some("same-id"), Some("startup"), AT);
        record_session_start_at(
            &path,
            OTHER_WORKSPACE,
            Some("same-id"),
            Some("startup"),
            AT,
            AT,
        )
        .unwrap();
        start(&path, Some("same-id"), Some("resume"), AT + 1);
        assert_eq!(read(&path, "same-id", AT + 1), None);
        assert!(
            read_session_start_at(&path, OTHER_WORKSPACE, Some("same-id"), AT + 1)
                .unwrap()
                .is_some()
        );
        let workspace = "A0000000-0000-0000-0000-000000000001";
        record_session_start_at(&path, workspace, Some("uuid-case"), Some("startup"), AT, AT)
            .unwrap();
        assert!(
            read_session_start_at(
                &path,
                &workspace.to_ascii_lowercase(),
                Some("uuid-case"),
                AT
            )
            .unwrap()
            .is_some()
        );
    }

    #[test]
    fn delayed_startup_and_same_time_transition_only_invalidate() {
        for delayed_at in [AT - 1, AT] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("starts.sqlite3");
            start(&path, Some("current"), Some("startup"), AT);
            record_session_start_at(
                &path,
                WORKSPACE,
                Some("delayed"),
                Some("startup"),
                delayed_at,
                AT + 1,
            )
            .unwrap();
            assert_eq!(read(&path, "current", AT + 1), None);
            assert_eq!(read(&path, "delayed", AT + 1), None);
            start(&path, Some("delayed"), Some("startup"), AT + 2);
            assert_eq!(read(&path, "delayed", AT + 2), None);
        }
    }

    #[test]
    fn missing_invalid_id_or_time_invalidates_existing_evidence() {
        for id in [None, Some(""), Some("x".repeat(8_193)).as_deref()] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("starts.sqlite3");
            start(&path, Some("current"), Some("startup"), AT);
            start(&path, id, Some("startup"), AT + 1);
            assert_eq!(read(&path, "current", AT + 1), None);
            assert_eq!(
                read_session_start_at(&path, WORKSPACE, id, AT + 1).unwrap(),
                None
            );
        }
        for at in [-1, AT + 2] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("starts.sqlite3");
            start(&path, Some("current"), Some("startup"), AT);
            record_session_start_at(
                &path,
                WORKSPACE,
                Some("invalid-time"),
                Some("startup"),
                at,
                AT + 1,
            )
            .unwrap();
            assert_eq!(read(&path, "current", AT + 1), None);
            assert_eq!(read(&path, "invalid-time", AT + 1), None);
        }
    }

    #[test]
    fn retention_expires_evidence_and_tombstones_but_preserves_fence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("starts.sqlite3");
        let expires = AT + RETENTION_DAYS * DAY_MS;
        start(&path, Some("one"), Some("startup"), AT);
        record_session_start_at(
            &path,
            OTHER_WORKSPACE,
            Some("other-session"),
            Some("startup"),
            AT,
            AT,
        )
        .unwrap();
        assert!(read(&path, "one", expires).is_some());
        assert_eq!(read(&path, "one", expires + 1), None);
        start(&path, None, Some("resume"), expires + 1);
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM session_start_ids", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(read_fence(&conn, WORKSPACE).unwrap().unwrap().generation, 2);
        assert!(
            read_fence(&conn, WORKSPACE)
                .unwrap()
                .unwrap()
                .active_session_hash
                .is_none()
        );
        assert!(
            read_fence(&conn, OTHER_WORKSPACE)
                .unwrap()
                .unwrap()
                .active_session_hash
                .is_none()
        );
        assert_eq!(
            read_session_start_at(&path, OTHER_WORKSPACE, Some("other-session"), expires + 1)
                .unwrap(),
            None
        );
        record_session_start_at(
            &path,
            WORKSPACE,
            Some("delayed"),
            Some("startup"),
            AT,
            expires + 2,
        )
        .unwrap();
        assert_eq!(read(&path, "delayed", expires + 2), None);
    }

    #[test]
    fn raw_ids_are_not_stored_and_read_does_not_change_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("starts.sqlite3");
        let secret = "host-private-session-id-never-store-verbatim";
        start(&path, Some(secret), Some("startup"), AT);
        let before = std::fs::read(&path).unwrap();
        assert!(!String::from_utf8_lossy(&before).contains(secret));
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).unwrap();
        assert!(read(&path, secret, AT).is_some());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let hash = start_session_hash(WORKSPACE, Some(secret))
            .unwrap()
            .unwrap();
        let ctx = EventContext {
            surface: ClientSurface::ClaudeCode,
            workspace_id: Some(WORKSPACE),
            session_id: Some(secret),
            prompt_id: None,
            turn_id: None,
            permission_mode: None,
        };
        let event = LedgerEvent::write(
            ctx,
            AT,
            "call",
            WriteTool::Propose,
            WriteOutcome::Success,
            None,
        )
        .unwrap();
        assert_eq!(event.session_hash.as_deref(), Some(hash.as_str()));
    }

    #[test]
    fn corrupt_or_unknown_schema_and_invalid_workspace_fail_without_repair() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("starts.sqlite3");
        assert!(
            record_session_start_at(
                &path,
                "not-a-workspace",
                Some("one"),
                Some("startup"),
                AT,
                AT
            )
            .is_err()
        );
        assert!(!path.exists());
        std::fs::write(&path, b"broken-database").unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(read_session_start_at(&path, WORKSPACE, Some("one"), AT).is_err());
        assert!(
            record_session_start_at(&path, WORKSPACE, Some("one"), Some("startup"), AT, AT)
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let other = dir.path().join("future.sqlite3");
        start(&other, Some("one"), Some("startup"), AT);
        let conn = Connection::open(&other).unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        let before = std::fs::read(&other).unwrap();
        assert!(read_session_start_at(&other, WORKSPACE, Some("one"), AT).is_err());
        assert!(
            record_session_start_at(&other, WORKSPACE, Some("one"), Some("startup"), AT, AT)
                .is_err()
        );
        assert_eq!(std::fs::read(&other).unwrap(), before);
    }

    #[test]
    fn concurrent_starts_never_leave_two_active_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("starts.sqlite3");
        // schema作成時の競合を分け、以下では世代更新のtransactionを検査する。
        start(&path, None, Some("resume"), AT - 1);
        std::thread::scope(|scope| {
            let one = scope.spawn(|| {
                record_session_start_at(&path, WORKSPACE, Some("one"), Some("startup"), AT, AT + 1)
            });
            let two = scope.spawn(|| {
                record_session_start_at(
                    &path,
                    WORKSPACE,
                    Some("two"),
                    Some("startup"),
                    AT + 1,
                    AT + 1,
                )
            });
            one.join().unwrap().unwrap();
            two.join().unwrap().unwrap();
        });
        let active = ["one", "two"]
            .iter()
            .filter(|id| read(&path, id, AT + 1).is_some())
            .count();
        assert!(active <= 1);
        assert_eq!(read(&path, "one", AT + 1), None);
    }
}
