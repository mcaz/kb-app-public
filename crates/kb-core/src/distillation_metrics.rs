//! 蒸留の所要時間だけを端末へ記録する。計測障害を正本更新の成否へ混ぜない。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, ensure};
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};

use crate::distillation_ai::{AiRunError, DistillationAiProvider, DistillationAiSettings};
use crate::distillation_jobs::JobLease;
use crate::vault::Vault;

// 数万件の初回整理でも診断が無制限に肥大化しない。表示は直近20試行に絞る。
const RETAIN_RUNS: usize = 200;
const DISPLAY_RUNS: usize = 20;
// 通常は20工程程度。呼出元の誤ループでも1試行の保存量を制限する。
const MAX_STAGES: u32 = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Prepare,
    CliCheck,
    ModelCatalog,
    AiResponse,
    Search,
    Validate,
    Commit,
    Export,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Applied,
    NoChange,
    Blocked,
    RetryWait,
    Cancelled,
    Interrupted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Failure {
    Ai { kind: AiRunError },
    SnapshotChanged,
    LeaseChanged,
    ReviewFailed,
    ContextLimit,
    ContextSizeLimit,
    ReviewRoundLimit,
    ReviewNoProgress,
    NotSupported,
    Cancelled,
    NeedsReview,
    ExportPending,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StageView {
    pub stage: Stage,
    pub round: u32,
    pub started_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub elapsed_ms: u64,
    pub elapsed_is_estimate: bool,
    pub succeeded: Option<bool>,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RunView {
    pub run_id: String,
    pub provider: Option<DistillationAiProvider>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub attempt: u32,
    pub generation: i64,
    pub batch_size: u32,
    pub completed_notes: Option<u32>,
    pub input_bytes: Option<u64>,
    pub started_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub elapsed_ms: u64,
    pub elapsed_is_estimate: bool,
    pub outcome: Option<Outcome>,
    pub failure: Option<Failure>,
    pub stages: Vec<StageView>,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct MetricsView {
    pub available: bool,
    pub runs: Vec<RunView>,
}

impl MetricsView {
    pub fn unavailable() -> Self {
        Self {
            available: false,
            runs: Vec::new(),
        }
    }
}

fn failures() -> &'static Mutex<HashSet<PathBuf>> {
    static FAILURES: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    FAILURES.get_or_init(|| Mutex::new(HashSet::new()))
}

fn active_runs() -> &'static Mutex<HashSet<(PathBuf, String)>> {
    static ACTIVE: OnceLock<Mutex<HashSet<(PathBuf, String)>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashSet::new()))
}

fn mark_failed(path: &Path) {
    if let Ok(mut failures) = failures().lock() {
        failures.insert(path.to_owned());
    }
}

fn path(vault: &Vault) -> Result<PathBuf> {
    // 閲覧で保管庫IDや計測DBを新規作成しない。
    let id = crate::workspace::stored_workspace_id(vault)?;
    Ok(crate::app_data_dir()?
        .join("distillation-metrics")
        .join(format!("{id}.sqlite")))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn elapsed_ms(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn storage_ms(duration: u64) -> i64 {
    i64::try_from(duration).unwrap_or(i64::MAX)
}

fn read_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    u64::try_from(row.get::<_, i64>(index)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn estimated_ms(start_ms: i64, now: i64) -> u64 {
    u64::try_from(now.saturating_sub(start_ms)).unwrap_or(0)
}

fn opaque_run_id(token: &str) -> String {
    // leaseそのものは画面へ公開せず、照合用の不可逆な識別子だけを共有する。
    crate::distillation::sha256(token.as_bytes())
}

struct Recording {
    conn: Connection,
    next_sequence: u32,
    finished: bool,
}

pub(crate) struct Recorder {
    path: Option<PathBuf>,
    run_id: String,
    started: Instant,
    registered: bool,
    recording: Mutex<Option<Recording>>,
}

impl Recorder {
    pub(crate) fn start(
        vault: &Vault,
        lease: &JobLease,
        settings: &DistillationAiSettings,
    ) -> Self {
        Self::start_at(path(vault).ok(), lease, settings)
    }

    fn start_at(
        path: Option<PathBuf>,
        lease: &JobLease,
        settings: &DistillationAiSettings,
    ) -> Self {
        let started = Instant::now();
        let run_id = opaque_run_id(&lease.token);
        let recording =
            path.as_ref().and_then(
                |path| match start_recording(path, &run_id, lease, settings) {
                    Ok(recording) => {
                        if let Ok(mut failures) = failures().lock() {
                            failures.remove(path);
                        }
                        if let Ok(mut active) = active_runs().lock() {
                            active.insert((path.to_owned(), run_id.clone()));
                        }
                        Some(recording)
                    }
                    Err(_) => {
                        mark_failed(path);
                        None
                    }
                },
            );
        let registered = recording.is_some();
        Self {
            path,
            run_id,
            started,
            registered,
            recording: Mutex::new(recording),
        }
    }

    fn record(&self, write: impl FnOnce(&mut Recording) -> Result<()>) {
        let Ok(mut recording) = self.recording.lock() else {
            if let Some(path) = &self.path {
                mark_failed(path);
            }
            return;
        };
        if let Some(active) = recording.as_mut()
            && !active.finished
            && write(active).is_err()
        {
            if let Some(path) = &self.path {
                mark_failed(path);
            }
            *recording = None;
        }
    }

    /// 計測の保存失敗は処理を止めない。対象本文や識別子は保存せず件数だけを残す。
    pub(crate) fn set_batch_size(&self, count: usize) {
        self.record(|recording| {
            recording.conn.execute(
                "UPDATE runs SET batch_size=?2 WHERE run_id=?1",
                params![self.run_id, u32::try_from(count.max(1)).unwrap_or(u32::MAX)],
            )?;
            Ok(())
        });
    }

    /// 複数回の呼出しは最大入力バイト数で比較する。トークン数や累積利用量ではない。
    pub(crate) fn record_input_bytes(&self, bytes: usize) {
        self.record(|recording| {
            recording.conn.execute(
                "UPDATE runs SET input_bytes=MAX(COALESCE(input_bytes, 0), ?2) WHERE run_id=?1",
                params![self.run_id, i64::try_from(bytes).unwrap_or(i64::MAX)],
            )?;
            Ok(())
        });
    }

    /// 正本の取引が確定した後で呼ぶ。ファイル出力待ちでも完了件数を失わない。
    pub(crate) fn set_completed_notes(&self, count: usize) {
        self.record(|recording| {
            recording.conn.execute(
                "UPDATE runs SET completed_notes=MIN(batch_size, ?2) WHERE run_id=?1",
                params![self.run_id, u32::try_from(count).unwrap_or(u32::MAX)],
            )?;
            Ok(())
        });
    }

    pub(crate) fn measure<T, E>(
        &self,
        stage: Stage,
        round: u32,
        operation: impl FnOnce() -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E> {
        let mut sequence = None;
        self.record(|recording| {
            let next = recording.next_sequence;
            ensure!(next < MAX_STAGES, "計測工程数が範囲外");
            recording.next_sequence = next.saturating_add(1);
            // 工程と試行全体の経過を一緒に確定し、計時自身のfsync回数も抑える。
            let tx = recording.conn.unchecked_transaction()?;
            tx.execute(
                "INSERT INTO stages(run_id, sequence, stage, round, started_at_ms)
                 VALUES(?1, ?2, ?3, ?4, ?5)",
                params![
                    self.run_id,
                    next,
                    serde_json::to_string(&stage)?,
                    round,
                    now_ms()
                ],
            )?;
            tx.execute(
                "UPDATE runs SET elapsed_ms=?2 WHERE run_id=?1",
                params![self.run_id, storage_ms(elapsed_ms(self.started))],
            )?;
            tx.commit()?;
            sequence = Some(next);
            Ok(())
        });
        // DB待機・診断保存を工程の計測値へ含めない。
        let started = Instant::now();
        let result = operation();
        let duration = elapsed_ms(started);
        if let Some(sequence) = sequence {
            self.record(|recording| {
                let tx = recording.conn.unchecked_transaction()?;
                tx.execute(
                    "UPDATE stages SET finished_at_ms=?3, elapsed_ms=?4, succeeded=?5
                     WHERE run_id=?1 AND sequence=?2",
                    params![
                        self.run_id,
                        sequence,
                        now_ms(),
                        storage_ms(duration),
                        result.is_ok()
                    ],
                )?;
                tx.execute(
                    "UPDATE runs SET elapsed_ms=?2 WHERE run_id=?1",
                    params![self.run_id, storage_ms(elapsed_ms(self.started))],
                )?;
                tx.commit()?;
                Ok(())
            });
        }
        result
    }

    pub(crate) fn finish(&self, outcome: Outcome, failure: Option<Failure>) {
        self.record(|recording| {
            recording.conn.execute(
                "UPDATE runs SET finished_at_ms=?2, elapsed_ms=?3, outcome=?4, failure=?5
                 WHERE run_id=?1",
                params![
                    self.run_id,
                    now_ms(),
                    storage_ms(elapsed_ms(self.started)),
                    serde_json::to_string(&outcome)?,
                    failure
                        .map(|value| serde_json::to_string(&value))
                        .transpose()?,
                ],
            )?;
            recording.finished = true;
            Ok(())
        });
        if self.registered
            && let Some(path) = &self.path
            && let Ok(mut active) = active_runs().lock()
        {
            active.remove(&(path.clone(), self.run_id.clone()));
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.finish(Outcome::Interrupted, None);
    }
}

fn start_recording(
    path: &Path,
    run_id: &str,
    lease: &JobLease,
    settings: &DistillationAiSettings,
) -> Result<Recording> {
    // 任意文字列の本文がモデル欄経由で診断へ混入しないよう、通常設定と同じ検証を使う。
    settings.validate()?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("計測DBの親がない"))?;
    std::fs::create_dir_all(parent)?;
    let conn = Connection::open(path)?;
    // 計測の競合で本処理を長く待たせない。読み手は書込を必要としないrollback journal。
    conn.busy_timeout(Duration::from_millis(50))?;
    conn.execute_batch(
        "PRAGMA foreign_keys=ON;
         CREATE TABLE IF NOT EXISTS runs(
             run_id TEXT PRIMARY KEY, provider TEXT, model TEXT, reasoning_effort TEXT,
             attempt INTEGER NOT NULL, generation INTEGER NOT NULL, started_at_ms INTEGER NOT NULL,
             finished_at_ms INTEGER, elapsed_ms INTEGER NOT NULL DEFAULT 0, outcome TEXT, failure TEXT
         );
         CREATE TABLE IF NOT EXISTS stages(
             run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
             sequence INTEGER NOT NULL, stage TEXT NOT NULL, round INTEGER NOT NULL,
             started_at_ms INTEGER NOT NULL, finished_at_ms INTEGER,
             elapsed_ms INTEGER NOT NULL DEFAULT 0, succeeded INTEGER,
             PRIMARY KEY(run_id, sequence)
         );",
    )?;
    let tx = conn.unchecked_transaction()?;
    let columns = run_columns(&tx)?;
    for (name, declaration) in [
        ("batch_size", "INTEGER NOT NULL DEFAULT 1"),
        ("completed_notes", "INTEGER"),
        ("input_bytes", "INTEGER"),
    ] {
        if !columns.contains(name) {
            tx.execute_batch(&format!("ALTER TABLE runs ADD COLUMN {name} {declaration}"))?;
        }
    }
    tx.execute(
        "INSERT INTO runs(run_id, provider, model, reasoning_effort, attempt, generation, started_at_ms,
                          completed_notes)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
        params![
            run_id,
            settings.provider.map(|value| serde_json::to_string(&value)).transpose()?,
            settings.model,
            settings.reasoning_effort,
            lease.attempt,
            lease.generation,
            now_ms(),
        ],
    )?;
    // 端末時計が戻っても、開始したばかりの試行を古い記録として捨てない。
    tx.execute(
        "DELETE FROM runs WHERE run_id IN (
             SELECT run_id FROM runs ORDER BY rowid DESC LIMIT -1 OFFSET ?1
         )",
        [i64::try_from(RETAIN_RUNS)?],
    )?;
    tx.commit()?;
    Ok(Recording {
        conn,
        next_sequence: 0,
        finished: false,
    })
}

fn run_columns(conn: &Connection) -> Result<HashSet<String>> {
    let mut select = conn.prepare("PRAGMA table_info(runs)")?;
    Ok(select
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<_>>()?)
}

/// 閲覧は計測用ファイルを作らず、失効したleaseの記録を実行中として返さない。
pub fn read(vault: &Vault, core: &Connection) -> Result<MetricsView> {
    read_at(&path(vault)?, core, now_ms())
}

fn read_at(path: &Path, core: &Connection, now: i64) -> Result<MetricsView> {
    let available = failures()
        .lock()
        .map(|failures| !failures.contains(path))
        .unwrap_or(false);
    if !path.try_exists()? {
        return Ok(MetricsView {
            available,
            runs: Vec::new(),
        });
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(Duration::from_millis(50))?;
    let snapshot = conn.unchecked_transaction()?;
    let mut live = core.prepare(
        "SELECT lease_token, generation, attempt FROM distillation_jobs
         WHERE state='running' AND lease_token IS NOT NULL AND lease_expires_at>?1",
    )?;
    let live: HashMap<String, (i64, u32)> = live
        .query_map([now / 1000], |row| {
            Ok((
                opaque_run_id(&row.get::<_, String>(0)?),
                (row.get(1)?, row.get(2)?),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    // 旧版の履歴を開くだけではDBを書き換えない。未記録値を0件・0バイトと誤表示しない。
    let columns = run_columns(&snapshot)?;
    let batch_size = if columns.contains("batch_size") {
        "batch_size"
    } else {
        "1"
    };
    let completed_notes = if columns.contains("completed_notes") {
        "completed_notes"
    } else {
        "NULL"
    };
    let input_bytes = if columns.contains("input_bytes") {
        "input_bytes"
    } else {
        "NULL"
    };
    let mut select = snapshot.prepare(&format!(
        "SELECT run_id, provider, model, reasoning_effort, attempt, generation, started_at_ms,
                finished_at_ms, elapsed_ms, outcome, failure, {batch_size}, {completed_notes}, {input_bytes}
         FROM runs ORDER BY rowid DESC LIMIT ?1",
    ))?;
    let rows = select.query_map([i64::try_from(DISPLAY_RUNS)?], |row| {
        Ok((
            RunView {
                run_id: row.get(0)?,
                provider: None,
                model: row.get(2)?,
                reasoning_effort: row.get(3)?,
                attempt: row.get(4)?,
                generation: row.get(5)?,
                batch_size: row.get(11)?,
                completed_notes: row.get(12)?,
                input_bytes: row
                    .get::<_, Option<i64>>(13)?
                    .map(|_| read_u64(row, 13))
                    .transpose()?,
                started_at_ms: row.get(6)?,
                finished_at_ms: row.get(7)?,
                elapsed_ms: read_u64(row, 8)?,
                elapsed_is_estimate: false,
                outcome: None,
                failure: None,
                stages: Vec::new(),
            },
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(9)?,
            row.get::<_, Option<String>>(10)?,
        ))
    })?;
    let mut runs = Vec::new();
    for row in rows {
        let (mut run, provider, outcome, failure) = row?;
        run.provider = provider.map(|raw| serde_json::from_str(&raw)).transpose()?;
        run.outcome = outcome.map(|raw| serde_json::from_str(&raw)).transpose()?;
        run.failure = failure.map(|raw| serde_json::from_str(&raw)).transpose()?;
        // DB確定でleaseが消えても、同じ試行のファイル出力はまだ動いている。
        // 別processの生存は直接判定できないため、leaseは記録上の実行中として扱う。
        let active = active_runs()
            .lock()
            .map(|active| active.contains(&(path.to_owned(), run.run_id.clone())))
            .unwrap_or(false);
        let running = run.outcome.is_none()
            && (active || live.get(&run.run_id) == Some(&(run.generation, run.attempt)));
        if running {
            run.elapsed_ms = estimated_ms(run.started_at_ms, now);
            run.elapsed_is_estimate = true;
        } else if run.outcome.is_none() {
            // 強制終了時の最終所要時間は分からない。最後に保存済みの観測時間を残す。
            run.outcome = Some(Outcome::Interrupted);
            run.failure = Some(Failure::LeaseChanged);
        }
        let mut stages = snapshot.prepare(
            "SELECT stage, round, started_at_ms, finished_at_ms, elapsed_ms, succeeded
             FROM stages WHERE run_id=?1 ORDER BY sequence LIMIT 128",
        )?;
        let rows = stages.query_map([&run.run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get::<_, Option<i64>>(3)?,
                read_u64(row, 4)?,
                row.get(5)?,
            ))
        })?;
        for row in rows {
            let (stage, round, started_at_ms, finished_at_ms, stored_elapsed, succeeded) = row?;
            let estimated = running && finished_at_ms.is_none();
            run.stages.push(StageView {
                stage: serde_json::from_str(&stage)?,
                round,
                started_at_ms,
                finished_at_ms,
                elapsed_ms: if estimated {
                    estimated_ms(started_at_ms, now)
                } else {
                    stored_elapsed
                },
                elapsed_is_estimate: estimated,
                succeeded,
            });
        }
        runs.push(run);
    }
    Ok(MetricsView { available, runs })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Vault, Connection, JobLease) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let core = Connection::open_in_memory().unwrap();
        core.execute_batch(
            "CREATE TABLE distillation_jobs(
                state TEXT, lease_token TEXT, generation INTEGER,
                attempt INTEGER, lease_expires_at INTEGER
             );",
        )
        .unwrap();
        let lease = JobLease {
            note: "notes/private-note-title".into(),
            generation: 3,
            token: format!("{:032x}", rand::random::<u128>()),
            input_hash: "private-document-hash".into(),
            expires_at: now_ms() / 1000 + 600,
            attempt: 2,
        };
        core.execute(
            "INSERT INTO distillation_jobs VALUES('running', ?1, ?2, ?3, ?4)",
            params![
                lease.token,
                lease.generation,
                lease.attempt,
                lease.expires_at
            ],
        )
        .unwrap();
        (dir, vault, core, lease)
    }

    fn settings() -> DistillationAiSettings {
        DistillationAiSettings {
            enabled: true,
            provider: Some(DistillationAiProvider::Codex),
            model: Some("gpt-test".into()),
            reasoning_effort: Some("high".into()),
            ..Default::default()
        }
    }

    /// 2026-09-07: 時計の推定を確定所要時間と混ぜず、本文やCLIの生エラーは残さない。
    #[test]
    fn records_monotonic_durations_and_only_allowlisted_metadata() {
        let (_dir, vault, core, lease) = setup();
        let recorder = Recorder::start(&vault, &lease, &settings());
        let result = recorder.measure(Stage::AiResponse, 2, || {
            let view = read_at(&path(&vault).unwrap(), &core, now_ms() + 50_000).unwrap();
            let run = &view.runs[0];
            assert!(run.outcome.is_none());
            assert!(run.elapsed_is_estimate);
            assert!(run.elapsed_ms >= 50_000);
            assert!(run.stages[0].elapsed_is_estimate);
            assert!(run.stages[0].succeeded.is_none());
            Err::<(), _>("private-prompt-and-error")
        });
        assert_eq!(result, Err("private-prompt-and-error"));
        recorder.finish(
            Outcome::RetryWait,
            Some(Failure::Ai {
                kind: AiRunError::TimedOut,
            }),
        );
        let view = read_at(&path(&vault).unwrap(), &core, now_ms() + 100_000).unwrap();
        assert!(view.available);
        let run = &view.runs[0];
        assert_eq!(run.outcome, Some(Outcome::RetryWait));
        assert_eq!(run.attempt, 2);
        assert_eq!(run.generation, 3);
        assert_eq!(run.provider, Some(DistillationAiProvider::Codex));
        assert_eq!(run.model.as_deref(), Some("gpt-test"));
        assert_eq!(run.reasoning_effort.as_deref(), Some("high"));
        assert!(!run.elapsed_is_estimate);
        assert!(run.elapsed_ms < 50_000);
        assert!(run.finished_at_ms.is_some());
        assert!(!run.stages[0].elapsed_is_estimate);
        assert_eq!(run.stages[0].succeeded, Some(false));
        assert_eq!(run.stages[0].round, 2);
        assert!(run.stages[0].elapsed_ms < 50_000);
        assert_ne!(run.run_id, lease.token);
        let db_bytes = std::fs::read(path(&vault).unwrap()).unwrap();
        let db_text = String::from_utf8_lossy(&db_bytes);
        for private in [&lease.note, &lease.input_hash, &lease.token] {
            assert!(!db_text.contains(private));
        }
        assert!(!db_text.contains("private-prompt-and-error"));
    }

    /// 2026-09-07: バッチ件数とAI呼出回数を混同せず、追加読込後の最大入力を比較する。
    #[test]
    fn batch_counts_and_peak_input_survive_commit_and_export() {
        let (_dir, vault, core, lease) = setup();
        let recorder = Recorder::start(&vault, &lease, &settings());
        recorder.set_batch_size(6);
        recorder.record_input_bytes(32_000);
        recorder.record_input_bytes(48_000);
        recorder.record_input_bytes(24_000);
        let before = read(&vault, &core).unwrap();
        assert_eq!(before.runs[0].batch_size, 6);
        assert_eq!(before.runs[0].completed_notes, Some(0));
        assert_eq!(before.runs[0].input_bytes, Some(48_000));
        core.execute("DELETE FROM distillation_jobs", []).unwrap();
        recorder.set_completed_notes(6);
        recorder
            .measure(Stage::Export, 1, || {
                let view = read(&vault, &core).unwrap();
                assert_eq!(view.runs[0].completed_notes, Some(6));
                assert!(view.runs[0].outcome.is_none());
                Ok::<_, ()>(())
            })
            .unwrap();
        recorder.finish(Outcome::Applied, None);
        recorder.set_completed_notes(0);
        let view = read(&vault, &core).unwrap();
        assert_eq!(view.runs[0].completed_notes, Some(6));
        assert_eq!(view.runs[0].input_bytes, Some(48_000));
    }

    /// 2026-09-07: 一件ずつ実行していた旧履歴も、閲覧で移行せず未記録量を維持する。
    #[test]
    fn legacy_history_reads_without_writes_and_migrates_only_when_recording() {
        let (_dir, vault, core, lease) = setup();
        let filename = path(&vault).unwrap();
        std::fs::create_dir_all(filename.parent().unwrap()).unwrap();
        let conn = Connection::open(&filename).unwrap();
        conn.execute_batch(
            "CREATE TABLE runs(
                run_id TEXT PRIMARY KEY, provider TEXT, model TEXT, reasoning_effort TEXT,
                attempt INTEGER NOT NULL, generation INTEGER NOT NULL, started_at_ms INTEGER NOT NULL,
                finished_at_ms INTEGER, elapsed_ms INTEGER NOT NULL DEFAULT 0, outcome TEXT, failure TEXT
             );
             CREATE TABLE stages(
                run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
                sequence INTEGER NOT NULL, stage TEXT NOT NULL, round INTEGER NOT NULL,
                started_at_ms INTEGER NOT NULL, finished_at_ms INTEGER,
                elapsed_ms INTEGER NOT NULL DEFAULT 0, succeeded INTEGER,
                PRIMARY KEY(run_id, sequence)
             );
             INSERT INTO runs(run_id, attempt, generation, started_at_ms, finished_at_ms, outcome)
             VALUES('legacy-run', 1, 1, 1000, 2000, '\"no_change\"');"
        ).unwrap();
        drop(conn);
        let before = std::fs::read(&filename).unwrap();
        let view = read(&vault, &core).unwrap();
        assert_eq!(view.runs[0].batch_size, 1);
        assert_eq!(view.runs[0].completed_notes, None);
        assert_eq!(view.runs[0].input_bytes, None);
        assert_eq!(before, std::fs::read(&filename).unwrap());
        let recorder = Recorder::start(&vault, &lease, &settings());
        recorder.set_batch_size(3);
        recorder.set_completed_notes(3);
        recorder.finish(Outcome::NoChange, None);
        let view = read(&vault, &core).unwrap();
        assert_eq!(view.runs.len(), 2);
        assert_eq!(view.runs[0].batch_size, 3);
        assert_eq!(view.runs[0].completed_notes, Some(3));
        assert_eq!(view.runs[1].completed_notes, None);
        assert_eq!(view.runs[1].input_bytes, None);
        assert_eq!(view.runs[1].outcome, Some(Outcome::NoChange));
    }

    /// 2026-09-07: 設定を開くことだけで計測用ファイルや保管庫IDを作らない。
    #[test]
    fn empty_reads_do_not_create_files_and_vaults_are_isolated() {
        let (_dir, vault, core, lease) = setup();
        let filename = path(&vault).unwrap();
        assert!(!filename.exists());
        assert!(read(&vault, &core).unwrap().runs.is_empty());
        assert!(!filename.exists());
        let recorder = Recorder::start(&vault, &lease, &settings());
        recorder.finish(Outcome::NoChange, None);
        let (_other_dir, other_vault, other_core, _) = setup();
        assert!(read(&other_vault, &other_core).unwrap().runs.is_empty());
        assert!(!path(&other_vault).unwrap().exists());
        assert_eq!(read(&vault, &core).unwrap().runs.len(), 1);
    }

    /// 2026-09-07: 古い試行を新世代の実行中へ誤対応させず、閲覧では記録を修復しない。
    #[test]
    fn stale_and_expired_leases_are_interrupted_without_writes() {
        let (_dir, vault, core, lease) = setup();
        let recorder = Recorder::start(&vault, &lease, &settings());
        recorder
            .measure(Stage::Prepare, 0, || Ok::<_, ()>(()))
            .unwrap();
        let filename = path(&vault).unwrap();
        let before = std::fs::read(&filename).unwrap();
        active_runs()
            .lock()
            .unwrap()
            .remove(&(filename.clone(), recorder.run_id.clone()));
        core.execute("UPDATE distillation_jobs SET generation=generation+1", [])
            .unwrap();
        let view = read(&vault, &core).unwrap();
        assert_eq!(view.runs[0].outcome, Some(Outcome::Interrupted));
        assert_eq!(view.runs[0].failure, Some(Failure::LeaseChanged));
        assert!(view.runs[0].finished_at_ms.is_none());
        assert!(!view.runs[0].elapsed_is_estimate);
        assert_eq!(before, std::fs::read(&filename).unwrap());
        core.execute("UPDATE distillation_jobs SET generation=generation-1", [])
            .unwrap();
        let expired = read_at(&filename, &core, (lease.expires_at + 1) * 1000).unwrap();
        assert_eq!(expired.runs[0].outcome, Some(Outcome::Interrupted));
        assert_eq!(before, std::fs::read(filename).unwrap());
    }

    /// 2026-09-07: 計測失敗で成功を失敗へ変えず、失敗理由や本文も副carへ逃がさない。
    #[test]
    fn broken_storage_preserves_the_original_operation_result() {
        let (dir, _vault, core, lease) = setup();
        let filename = dir.path().join("corrupt.sqlite");
        std::fs::write(&filename, "broken sqlite").unwrap();
        let recorder = Recorder::start_at(Some(filename.clone()), &lease, &settings());
        recorder.set_batch_size(6);
        recorder.record_input_bytes(48_000);
        recorder.set_completed_notes(6);
        assert_eq!(
            recorder.measure(Stage::Commit, 0, || Ok::<_, &str>(42)),
            Ok(42)
        );
        assert_eq!(
            recorder.measure(Stage::Commit, 0, || Err::<(), _>("original error")),
            Err("original error")
        );
        recorder.finish(Outcome::Applied, None);
        assert!(failures().lock().unwrap().contains(&filename));
        assert!(read_at(&filename, &core, now_ms()).is_err());
        assert_eq!(std::fs::read_to_string(filename).unwrap(), "broken sqlite");
    }

    /// 2026-09-07: DB確定後にleaseが消えても、残るファイル出力を中断扱いにしない。
    #[test]
    fn export_after_core_commit_remains_running_until_recorder_finishes() {
        let (_dir, vault, core, lease) = setup();
        let recorder = Recorder::start(&vault, &lease, &settings());
        core.execute("DELETE FROM distillation_jobs", []).unwrap();
        recorder
            .measure(Stage::Export, 0, || {
                let view = read(&vault, &core).unwrap();
                assert!(view.runs[0].outcome.is_none());
                assert!(view.runs[0].elapsed_is_estimate);
                assert_eq!(view.runs[0].stages[0].stage, Stage::Export);
                Ok::<_, ()>(())
            })
            .unwrap();
        recorder.finish(Outcome::Applied, None);
        assert_eq!(
            read(&vault, &core).unwrap().runs[0].outcome,
            Some(Outcome::Applied)
        );
    }

    /// 2026-09-07: 長期の初回整理でも保存試行数を200、表示を20に制限する。
    #[test]
    fn retention_removes_old_stage_rows_and_returns_latest_twenty() {
        let (_dir, vault, core, mut lease) = setup();
        let mut newest = String::new();
        for attempt in 1..=205 {
            lease.token = format!("{:032x}", rand::random::<u128>());
            lease.attempt = attempt;
            newest = opaque_run_id(&lease.token);
            let recorder = Recorder::start(&vault, &lease, &settings());
            recorder
                .measure(Stage::Prepare, 0, || Ok::<_, ()>(()))
                .unwrap();
            recorder.finish(Outcome::NoChange, None);
        }
        let conn = Connection::open(path(&vault).unwrap()).unwrap();
        let count = |table: &str| -> usize {
            usize::try_from(
                conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            )
            .unwrap()
        };
        assert_eq!(count("runs"), RETAIN_RUNS);
        assert_eq!(count("stages"), RETAIN_RUNS);
        let view = read(&vault, &core).unwrap();
        assert_eq!(view.runs.len(), DISPLAY_RUNS);
        assert_eq!(view.runs[0].run_id, newest);
        assert_eq!(view.runs[0].attempt, 205);
    }

    /// 2026-09-07: 呼出元が途中で戻った場合は、正常終了に見せず中断を記録する。
    #[test]
    fn drop_records_interruption_and_does_not_overwrite_a_finished_outcome() {
        let (_dir, vault, core, mut lease) = setup();
        {
            let _recorder = Recorder::start(&vault, &lease, &settings());
        }
        assert_eq!(
            read(&vault, &core).unwrap().runs[0].outcome,
            Some(Outcome::Interrupted)
        );
        lease.token = format!("{:032x}", rand::random::<u128>());
        {
            let recorder = Recorder::start(&vault, &lease, &settings());
            recorder.finish(Outcome::Applied, Some(Failure::ExportPending));
        }
        let run = &read(&vault, &core).unwrap().runs[0];
        assert_eq!(run.outcome, Some(Outcome::Applied));
        assert_eq!(run.failure, Some(Failure::ExportPending));
    }
}
