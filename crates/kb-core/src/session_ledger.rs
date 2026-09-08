//! 会話本文を持たない端末ローカルの観測台帳。Vault・索引・同期から独立する。
//! stdout準備と書込完了を分け、hostが受信・利用したという推測へ進めない。

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::client_surface::{ClientSurface, HookOutputBudget, HookOutputUnit};
use crate::hook_delivery::OutputStats;
use crate::write_rejection::WriteRejection;

mod observation;
pub use observation::*;
mod session_start;
pub(crate) mod trend;
pub use session_start::*;

const DAY_MS: i64 = 86_400_000;
pub const RETENTION_DAYS: i64 = 90;
const SCHEMA_VERSION: i64 = 2;
const APPLICATION_ID: i64 = 0x4b42_534c;
const TABLE_SQL: &str = "CREATE TABLE ledger_events (
    event_id TEXT PRIMARY KEY NOT NULL,
    observed_at_ms INTEGER NOT NULL,
    workspace_id TEXT,
    payload TEXT NOT NULL,
    emission_state TEXT,
    receipt_hash TEXT
)";
const INDEX_SQL: &str = "CREATE INDEX ledger_events_time ON ledger_events(observed_at_ms)";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    Default,
    Manual,
    AcceptEdits,
    Plan,
    BypassPermissions,
    DontAsk,
    Auto,
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl PermissionMode {
    pub fn from_hint(hint: &str) -> Option<Self> {
        match hint {
            "default" => Some(Self::Default),
            "manual" => Some(Self::Manual),
            "acceptEdits" | "accept_edits" => Some(Self::AcceptEdits),
            "plan" => Some(Self::Plan),
            "bypassPermissions" | "bypass_permissions" => Some(Self::BypassPermissions),
            "dontAsk" | "dont_ask" => Some(Self::DontAsk),
            "auto" => Some(Self::Auto),
            "read-only" | "read_only" => Some(Self::ReadOnly),
            "workspace-write" | "workspace_write" => Some(Self::WorkspaceWrite),
            "danger-full-access" | "danger_full_access" => Some(Self::DangerFullAccess),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct EventContext<'a> {
    pub surface: ClientSurface,
    pub workspace_id: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub prompt_id: Option<&'a str>,
    pub turn_id: Option<&'a str>,
    pub permission_mode: Option<PermissionMode>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HookTimings {
    pub initialize_ms: Option<u64>,
    pub search_ms: Option<u64>,
    pub render_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookFilterReason {
    NonUserEvent,
    MissingPrompt,
    ShortPrompt,
    SlashCommand,
    SystemNotification,
    TaskNotification,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookErrorStage {
    Input,
    Initialize,
    Search,
    Render,
    Stdout,
    Protocol,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostVersionStatus {
    Unverified,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapRationale {
    OperationalHeadroomUnverifiedHost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ValidationDate {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapAssumption {
    pub budget: HookOutputBudget,
    pub conservative_fallback: bool,
    pub host_version: Option<HostVersion>,
    pub host_version_status: HostVersionStatus,
    pub operational_rationale: CapRationale,
    pub validated_date: ValidationDate,
}

impl CapAssumption {
    pub fn for_surface(surface: ClientSurface) -> Self {
        Self {
            budget: surface.hook_output_budget(),
            conservative_fallback: true,
            host_version: None,
            host_version_status: HostVersionStatus::Unverified,
            operational_rationale: CapRationale::OperationalHeadroomUnverifiedHost,
            // この日付は運用予算を定めた日であり、稼働hostの受信を確認した日ではない。
            validated_date: ValidationDate {
                year: 2026,
                month: 9,
                day: 5,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HookObservation {
    OutputPrepared {
        stats: OutputStats,
        timings: HookTimings,
        cap_assumption: CapAssumption,
    },
    Filtered {
        reason: HookFilterReason,
        timings: HookTimings,
    },
    Error {
        stage: HookErrorStage,
        timings: HookTimings,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteTool {
    Propose,
    Update,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteOutcome {
    Success,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Observation {
    Hook {
        observation: HookObservation,
    },
    Write {
        tool: WriteTool,
        outcome: WriteOutcome,
        // 旧観測のnullを未分類として残し、自由形式error文字列は持たない。
        code: Option<WriteRejection>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LedgerEvent {
    event_id: String,
    observed_at_ms: i64,
    surface: ClientSurface,
    workspace_id: Option<String>,
    session_hash: Option<String>,
    prompt_hash: Option<String>,
    turn_hash: Option<String>,
    fallback_day: Option<i64>,
    permission_mode: Option<PermissionMode>,
    #[serde(default)]
    measurement: MeasurementContext,
    observation: Observation,
}

impl LedgerEvent {
    pub fn with_measurement(mut self, measurement: MeasurementContext) -> Result<Self> {
        self.measurement = measurement;
        self.validate()?;
        Ok(self)
    }

    pub fn hook(
        context: EventContext<'_>,
        observed_at_ms: i64,
        observation: HookObservation,
    ) -> Result<Self> {
        let stable_id = context.turn_id.or(context.prompt_id);
        Self::new(
            context,
            observed_at_ms,
            "hook",
            stable_id,
            Observation::Hook { observation },
        )
    }

    pub fn write(
        context: EventContext<'_>,
        observed_at_ms: i64,
        call_id: &str,
        tool: WriteTool,
        outcome: WriteOutcome,
        code: Option<WriteRejection>,
    ) -> Result<Self> {
        ensure!(!call_id.is_empty(), "台帳のwrite call IDがない");
        Self::new(
            context,
            observed_at_ms,
            "write",
            Some(call_id),
            Observation::Write {
                tool,
                outcome,
                code,
            },
        )
    }

    fn new(
        context: EventContext<'_>,
        observed_at_ms: i64,
        domain: &str,
        stable_id: Option<&str>,
        observation: Observation,
    ) -> Result<Self> {
        ensure!(observed_at_ms >= 0, "台帳の時刻が不正");
        ensure!(
            context.surface != ClientSurface::Unknown,
            "未知clientは台帳へ記録しない"
        );
        let workspace_id = context
            .workspace_id
            .map(validate_workspace_id)
            .transpose()?;
        let scope = serde_json::to_string(&(context.surface, &workspace_id))?;
        let hash = |kind: &str, raw: Option<&str>| -> Result<Option<String>> {
            raw.map(|raw| {
                ensure!(
                    !raw.is_empty() && raw.len() <= 8_192,
                    "台帳のopaque ID長が不正"
                );
                Ok(digest(&[kind, &scope, raw]))
            })
            .transpose()
        };
        let session_hash = hash("session", context.session_id)?;
        let prompt_hash = hash("prompt", context.prompt_id)?;
        let turn_hash = hash("turn", context.turn_id)?;
        let fallback_day = session_hash.is_none().then_some(observed_at_ms / DAY_MS);
        let grouping = serde_json::to_string(&(&session_hash, fallback_day))?;
        let event_key = match stable_id {
            Some(raw) => {
                ensure!(
                    !raw.is_empty() && raw.len() <= 8_192,
                    "台帳のevent ID長が不正"
                );
                digest(&[domain, &scope, &grouping, raw])
            }
            None => digest(&[
                domain,
                &scope,
                &grouping,
                &format!("{:x?}", rand::random::<[u8; 16]>()),
            ]),
        };
        let event = Self {
            event_id: event_key,
            observed_at_ms,
            surface: context.surface,
            workspace_id,
            session_hash,
            prompt_hash,
            turn_hash,
            fallback_day,
            permission_mode: context.permission_mode,
            measurement: MeasurementContext::default(),
            observation,
        };
        event.validate()?;
        Ok(event)
    }

    fn validate(&self) -> Result<()> {
        self.measurement.validate(self)?;
        ensure!(
            self.observed_at_ms >= 0 && self.surface != ClientSurface::Unknown,
            "台帳eventの時刻またはsurfaceが不正"
        );
        ensure!(valid_hash(&self.event_id), "台帳event hashが不正");
        for hash in [&self.session_hash, &self.prompt_hash, &self.turn_hash]
            .into_iter()
            .flatten()
        {
            ensure!(valid_hash(hash), "台帳opaque ID hashが不正");
        }
        if let Some(workspace) = &self.workspace_id {
            ensure!(
                validate_workspace_id(workspace)? == *workspace,
                "台帳workspace IDの表記が不正"
            );
        }
        ensure!(
            self.fallback_day
                == self
                    .session_hash
                    .is_none()
                    .then_some(self.observed_at_ms / DAY_MS),
            "台帳session区分が不正"
        );
        if let Observation::Hook {
            observation:
                HookObservation::OutputPrepared {
                    stats,
                    cap_assumption,
                    ..
                },
        } = &self.observation
        {
            ensure!(
                cap_assumption.host_version.is_none() && cap_assumption.conservative_fallback,
                "未確認のhost版で予算を緩めない"
            );
            let date = cap_assumption.validated_date;
            time::Date::from_calendar_date(
                date.year,
                time::Month::try_from(date.month)?,
                date.day,
            )?;
            ensure!(
                cap_assumption.budget == self.surface.hook_output_budget(),
                "台帳のsurfaceと出力予算が不一致"
            );
            ensure!(
                stats.budget_unit == cap_assumption.budget.unit
                    && stats.budget_limit == cap_assumption.budget.limit,
                "台帳の出力統計と予算が不一致"
            );
            let measured = match stats.budget_unit {
                HookOutputUnit::Utf8Bytes => stats.emitted_bytes,
                HookOutputUnit::Utf16CodeUnits => stats.emitted_utf16_units,
            };
            ensure!(
                measured <= stats.budget_limit && stats.emitted_documents <= 10,
                "台帳の出力統計が予算外"
            );
            ensure!(
                stats.emitted_chars <= stats.emitted_utf16_units
                    && stats.emitted_utf16_units <= stats.emitted_chars.saturating_mul(2)
                    && stats.emitted_chars <= stats.emitted_bytes
                    && stats.emitted_bytes <= stats.emitted_chars.saturating_mul(4),
                "台帳の文字数統計が不整合"
            );
            ensure!(
                stats.capped
                    || (stats.trimmed_documents == 0
                        && stats.omitted_candidates == 0
                        && stats.omitted_warnings == 0
                        && stats.shortened_warnings == 0
                        && stats.shortened_candidates == 0),
                "台帳の省略統計が不整合"
            );
        }
        if let Observation::Write { code, outcome, .. } = &self.observation {
            ensure!(
                code.is_none() || *outcome == WriteOutcome::Error,
                "成功観測に拒否codeは付けない"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppendDisposition {
    Inserted,
    Deduplicated,
    Expired,
}

#[derive(Clone, Debug)]
pub struct HookReceipt {
    event_id: String,
    token: String,
}

#[derive(Clone, Debug)]
pub struct AppendOutcome {
    pub disposition: AppendDisposition,
    pub receipt: Option<HookReceipt>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEmissionOutcome {
    Emitted,
    StdoutFailed,
}

#[derive(Clone, Debug)]
pub struct SummaryQuery {
    pub since_ms: i64,
    pub until_ms: i64,
    pub workspace_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct GroupingCounts {
    pub actual_sessions: usize,
    pub daily_fallback_days: usize,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct WriteRejectionCount {
    pub code: WriteRejection,
    pub propose: u64,
    pub update: u64,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SurfaceSummary {
    pub surface: ClientSurface,
    pub hook_groups: GroupingCounts,
    pub write_groups: GroupingCounts,
    pub hook_output_prepared: u64,
    pub hook_output_emitted: u64,
    pub hook_stdout_failed: u64,
    pub hook_filtered: u64,
    pub hook_errors: u64,
    pub emitted_chars: u64,
    pub emitted_bytes: u64,
    pub emitted_utf16_units: u64,
    pub emitted_documents: u64,
    pub trimmed_documents: u64,
    pub capped_outputs: u64,
    pub propose_successes: u64,
    pub propose_errors: u64,
    pub update_successes: u64,
    pub update_errors: u64,
    pub write_rejections: Vec<WriteRejectionCount>,
    pub unclassified_propose_errors: u64,
    pub unclassified_update_errors: u64,
    pub last_successful_propose_at_ms: Option<i64>,
}

impl SurfaceSummary {
    pub(crate) fn empty(surface: ClientSurface) -> Self {
        Accumulator::new(surface).summary
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkspaceSummary {
    pub workspace_id: String,
    pub surfaces: Vec<SurfaceSummary>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LedgerSummary {
    pub exists: bool,
    pub since_ms: i64,
    pub until_ms: i64,
    pub retention_days: i64,
    pub workspaces: Vec<WorkspaceSummary>,
    pub unassigned: Vec<SurfaceSummary>,
}

pub fn now_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

fn runtime_path() -> Result<PathBuf> {
    Ok(crate::app_data_dir()?
        .join("session-ledger")
        .join("ledger.sqlite3"))
}

pub fn append(event: &LedgerEvent) -> Result<AppendOutcome> {
    append_at(&runtime_path()?, event)
}
pub fn finalize_hook(receipt: &HookReceipt, outcome: HookEmissionOutcome) -> Result<()> {
    finalize_hook_at(&runtime_path()?, receipt, outcome)
}
pub fn summary(query: &SummaryQuery) -> Result<LedgerSummary> {
    summary_at(&runtime_path()?, query)
}

fn digest(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"kb-app-session-ledger-v1");
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    format!("{:x}", hash.finalize())
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_workspace_id(value: &str) -> Result<String> {
    let uuid = value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        });
    ensure!(
        crate::artifact::is_ulid(value) || uuid,
        "台帳workspace IDはULIDまたはUUIDが必要"
    );
    Ok(if uuid {
        value.to_ascii_lowercase()
    } else {
        value.to_string()
    })
}

pub fn append_at(path: &Path, event: &LedgerEvent) -> Result<AppendOutcome> {
    append_at_time(path, event, now_ms())
}

fn append_at_time(path: &Path, event: &LedgerEvent, now: i64) -> Result<AppendOutcome> {
    event.validate()?;
    ensure!(
        event.observed_at_ms <= now.saturating_add(60_000),
        "台帳eventの時刻が未来すぎる"
    );
    let parent = path.parent().context("台帳保存先の親がない")?;
    std::fs::create_dir_all(parent).context("台帳directoryを作成できない")?;
    let mut conn = Connection::open(path).context("台帳DBを開けない")?;
    conn.busy_timeout(Duration::from_millis(100))?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    initialize_or_validate_schema(&tx)?;
    let cutoff = now.saturating_sub(RETENTION_DAYS * DAY_MS).max(0);
    tx.execute(
        "DELETE FROM ledger_events WHERE observed_at_ms < ?1",
        [cutoff],
    )?;
    if event.observed_at_ms < cutoff {
        tx.commit()?;
        return Ok(AppendOutcome {
            disposition: AppendDisposition::Expired,
            receipt: None,
        });
    }
    let payload = serde_json::to_string(event)?;
    let prepared = matches!(
        event.observation,
        Observation::Hook {
            observation: HookObservation::OutputPrepared { .. }
        }
    );
    let receipt_hash = prepared.then(|| digest(&["prepared", &payload]));
    let inserted = tx.execute(
        "INSERT INTO ledger_events(event_id, observed_at_ms, workspace_id, payload, emission_state, receipt_hash)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(event_id) DO NOTHING",
        params![event.event_id, event.observed_at_ms, event.workspace_id, payload, prepared.then_some("prepared"), receipt_hash],
    )? == 1;
    tx.commit()?;
    Ok(AppendOutcome {
        disposition: if inserted {
            AppendDisposition::Inserted
        } else {
            AppendDisposition::Deduplicated
        },
        // 重複した別呼び出しへ既存観測の完了権を渡さない。
        receipt: if inserted {
            receipt_hash.map(|token| HookReceipt {
                event_id: event.event_id.clone(),
                token,
            })
        } else {
            None
        },
    })
}

pub fn finalize_hook_at(
    path: &Path,
    receipt: &HookReceipt,
    outcome: HookEmissionOutcome,
) -> Result<()> {
    ensure!(
        valid_hash(&receipt.event_id) && valid_hash(&receipt.token),
        "台帳receiptが不正"
    );
    let mut conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    conn.busy_timeout(Duration::from_millis(100))?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    validate_schema(&tx)?;
    let row = tx
        .query_row(
            "SELECT payload, emission_state, receipt_hash FROM ledger_events WHERE event_id = ?1",
            [&receipt.event_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?
        .context("台帳のprepared観測がない")?;
    let event: LedgerEvent = serde_json::from_str(&row.0).context("台帳event形式が不正")?;
    event.validate()?;
    ensure!(
        event.event_id == receipt.event_id
            && row.2.as_deref() == Some(receipt.token.as_str())
            && digest(&["prepared", &row.0]) == receipt.token,
        "台帳receiptとprepared観測が不一致"
    );
    ensure!(
        matches!(
            event.observation,
            Observation::Hook {
                observation: HookObservation::OutputPrepared { .. }
            }
        ),
        "台帳eventは出力準備ではない"
    );
    let next = match outcome {
        HookEmissionOutcome::Emitted => "emitted",
        HookEmissionOutcome::StdoutFailed => "stdout_failed",
    };
    ensure!(
        row.1.as_deref() == Some("prepared") || row.1.as_deref() == Some(next),
        "台帳の出力完了状態が競合"
    );
    tx.execute(
        "UPDATE ledger_events SET emission_state = ?1 WHERE event_id = ?2",
        params![next, receipt.event_id],
    )?;
    tx.commit()?;
    Ok(())
}

fn initialize_or_validate_schema(conn: &Connection) -> Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let app_id: i64 = conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let objects: i64 =
        conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |row| row.get(0))?;
    if version == 0 && app_id == 0 && objects == 0 {
        conn.execute_batch(&format!("{TABLE_SQL};\n{INDEX_SQL};"))?;
        conn.pragma_update(None, "application_id", APPLICATION_ID)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    validate_schema(conn)?;
    // code:nullだけを読める旧binaryに、新しい拒否codeを空台帳と誤認させない。
    // payloadは変えないため、進行中のPrepared receiptも有効なまま引き継げる。
    if version == 1 {
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    Ok(())
}

fn validate_schema(conn: &Connection) -> Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let app_id: i64 = conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
    ensure!(
        matches!(version, 1 | SCHEMA_VERSION) && app_id == APPLICATION_ID,
        "未対応または不正な台帳schema"
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
                ("ledger_events".into(), TABLE_SQL.into()),
                ("ledger_events_time".into(), INDEX_SQL.into())
            ],
        "台帳schemaの構造が不一致"
    );
    Ok(())
}

pub fn summary_at(path: &Path, query: &SummaryQuery) -> Result<LedgerSummary> {
    ensure!(
        query.since_ms >= 0 && query.until_ms > query.since_ms,
        "台帳集計期間が不正"
    );
    let workspace = query
        .workspace_id
        .as_deref()
        .map(validate_workspace_id)
        .transpose()?;
    let mut summary = LedgerSummary {
        exists: false,
        since_ms: query.since_ms,
        until_ms: query.until_ms,
        retention_days: RETENTION_DAYS,
        workspaces: Vec::new(),
        unassigned: Vec::new(),
    };
    let mut groups = BTreeMap::<(Option<String>, String), Accumulator>::new();
    summary.exists =
        scan_validated_events(path, query, workspace.as_deref(), true, |event, state| {
            let key = (
                event.workspace_id.clone(),
                serde_json::to_string(&event.surface)?,
            );
            groups
                .entry(key)
                .or_insert_with(|| Accumulator::new(event.surface))
                .observe(event, state)?;
            Ok(())
        })?;
    let mut workspaces = BTreeMap::<String, Vec<SurfaceSummary>>::new();
    for ((workspace, _), accumulator) in groups {
        let surface = accumulator.finish();
        if let Some(workspace) = workspace {
            workspaces.entry(workspace).or_default().push(surface);
        } else {
            summary.unassigned.push(surface);
        }
    }
    summary.workspaces = workspaces
        .into_iter()
        .map(|(workspace_id, surfaces)| WorkspaceSummary {
            workspace_id,
            surfaces,
        })
        .collect();
    Ok(summary)
}

/// 集計窓を同じsnapshotで走査し、すべての集計でpayloadとreceiptの検証を共有する。
fn scan_validated_events(
    path: &Path,
    query: &SummaryQuery,
    workspace: Option<&str>,
    include_unassigned: bool,
    mut observe: impl FnMut(&LedgerEvent, Option<&str>) -> Result<()>,
) -> Result<bool> {
    match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let mut conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(Duration::from_millis(100))?;
    conn.pragma_update(None, "query_only", true)?;
    let tx = conn.transaction()?;
    validate_schema(&tx)?;
    let mut statement = tx.prepare(
        "SELECT event_id, observed_at_ms, workspace_id, payload, emission_state, receipt_hash
         FROM ledger_events WHERE observed_at_ms >= ?1 AND observed_at_ms < ?2
         AND (?3 IS NULL OR workspace_id = ?3 OR (?4 AND workspace_id IS NULL)) ORDER BY observed_at_ms, event_id",
    )?;
    let mut rows = statement.query(params![
        query.since_ms,
        query.until_ms,
        workspace,
        include_unassigned
    ])?;
    while let Some(row) = rows.next()? {
        let payload: String = row.get(3)?;
        let event: LedgerEvent = serde_json::from_str(&payload).context("台帳event形式が不正")?;
        event.validate()?;
        ensure!(
            event.event_id == row.get::<_, String>(0)?
                && event.observed_at_ms == row.get::<_, i64>(1)?
                && event.workspace_id == row.get::<_, Option<String>>(2)?,
            "台帳の索引とeventが不一致"
        );
        let state: Option<String> = row.get(4)?;
        let token: Option<String> = row.get(5)?;
        let prepared = matches!(
            event.observation,
            Observation::Hook {
                observation: HookObservation::OutputPrepared { .. }
            }
        );
        if prepared {
            ensure!(
                matches!(
                    state.as_deref(),
                    Some("prepared" | "emitted" | "stdout_failed")
                ) && token.as_deref() == Some(digest(&["prepared", &payload]).as_str()),
                "台帳の出力観測状態が不正"
            );
        } else {
            ensure!(
                state.is_none() && token.is_none(),
                "台帳eventに不正な出力観測状態がある"
            );
        }
        observe(&event, state.as_deref())?;
    }
    Ok(true)
}

struct Accumulator {
    summary: SurfaceSummary,
    hook_sessions: HashSet<String>,
    hook_days: HashSet<i64>,
    write_sessions: HashSet<String>,
    write_days: HashSet<i64>,
}

impl Accumulator {
    fn new(surface: ClientSurface) -> Self {
        Self {
            summary: SurfaceSummary {
                surface,
                hook_groups: GroupingCounts::default(),
                write_groups: GroupingCounts::default(),
                hook_output_prepared: 0,
                hook_output_emitted: 0,
                hook_stdout_failed: 0,
                hook_filtered: 0,
                hook_errors: 0,
                emitted_chars: 0,
                emitted_bytes: 0,
                emitted_utf16_units: 0,
                emitted_documents: 0,
                trimmed_documents: 0,
                capped_outputs: 0,
                propose_successes: 0,
                propose_errors: 0,
                update_successes: 0,
                update_errors: 0,
                write_rejections: Vec::new(),
                unclassified_propose_errors: 0,
                unclassified_update_errors: 0,
                last_successful_propose_at_ms: None,
            },
            hook_sessions: HashSet::new(),
            hook_days: HashSet::new(),
            write_sessions: HashSet::new(),
            write_days: HashSet::new(),
        }
    }

    fn observe(&mut self, event: &LedgerEvent, state: Option<&str>) -> Result<()> {
        let (sessions, days) = if matches!(event.observation, Observation::Hook { .. }) {
            (&mut self.hook_sessions, &mut self.hook_days)
        } else {
            (&mut self.write_sessions, &mut self.write_days)
        };
        if let Some(session) = &event.session_hash {
            sessions.insert(session.clone());
        }
        if let Some(day) = event.fallback_day {
            days.insert(day);
        }
        let summary = &mut self.summary;
        match &event.observation {
            Observation::Hook { observation } => match observation {
                HookObservation::OutputPrepared { stats, .. } => match state {
                    Some("prepared") => summary.hook_output_prepared += 1,
                    Some("stdout_failed") => summary.hook_stdout_failed += 1,
                    Some("emitted") => {
                        summary.hook_output_emitted += 1;
                        for (total, count) in [
                            (&mut summary.emitted_chars, stats.emitted_chars),
                            (&mut summary.emitted_bytes, stats.emitted_bytes),
                            (&mut summary.emitted_utf16_units, stats.emitted_utf16_units),
                            (&mut summary.emitted_documents, stats.emitted_documents),
                            (&mut summary.trimmed_documents, stats.trimmed_documents),
                        ] {
                            *total = total
                                .checked_add(count as u64)
                                .context("台帳集計値が上限外")?;
                        }
                        summary.capped_outputs += u64::from(stats.capped);
                    }
                    _ => anyhow::bail!("台帳の出力状態が不正"),
                },
                HookObservation::Filtered { .. } => summary.hook_filtered += 1,
                HookObservation::Error { .. } => summary.hook_errors += 1,
            },
            Observation::Write {
                tool,
                outcome,
                code,
            } => {
                if *outcome == WriteOutcome::Error {
                    if let Some(code) = code {
                        let index = summary
                            .write_rejections
                            .iter()
                            .position(|entry| entry.code == *code);
                        let index = index.unwrap_or_else(|| {
                            summary.write_rejections.push(WriteRejectionCount {
                                code: *code,
                                propose: 0,
                                update: 0,
                            });
                            summary.write_rejections.len() - 1
                        });
                        match tool {
                            WriteTool::Propose => summary.write_rejections[index].propose += 1,
                            WriteTool::Update => summary.write_rejections[index].update += 1,
                        }
                    } else {
                        match tool {
                            WriteTool::Propose => summary.unclassified_propose_errors += 1,
                            WriteTool::Update => summary.unclassified_update_errors += 1,
                        }
                    }
                }
                match (tool, outcome) {
                    (WriteTool::Propose, WriteOutcome::Success) => {
                        summary.propose_successes += 1;
                        summary.last_successful_propose_at_ms = Some(
                            summary
                                .last_successful_propose_at_ms
                                .map_or(event.observed_at_ms, |last| {
                                    last.max(event.observed_at_ms)
                                }),
                        );
                    }
                    (WriteTool::Propose, WriteOutcome::Error) => summary.propose_errors += 1,
                    (WriteTool::Update, WriteOutcome::Success) => summary.update_successes += 1,
                    (WriteTool::Update, WriteOutcome::Error) => summary.update_errors += 1,
                }
            }
        }
        Ok(())
    }

    fn finish(mut self) -> SurfaceSummary {
        self.summary
            .write_rejections
            .sort_by_key(|entry| entry.code);
        self.summary.hook_groups = GroupingCounts {
            actual_sessions: self.hook_sessions.len(),
            daily_fallback_days: self.hook_days.len(),
        };
        self.summary.write_groups = GroupingCounts {
            actual_sessions: self.write_sessions.len(),
            daily_fallback_days: self.write_days.len(),
        };
        self.summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook_delivery::render_hook_delivery;
    use serde_json::json;

    const WORKSPACE_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const WORKSPACE_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

    #[test]
    fn legacy_null_errors_remain_unclassified_and_v2_counts_typed_rejections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite3");
        let at = now_ms();
        let ctx = context(Some(WORKSPACE_A), Some("one"), None);
        let legacy = LedgerEvent::write(
            ctx,
            at,
            "legacy",
            WriteTool::Propose,
            WriteOutcome::Error,
            None,
        )
        .unwrap();
        append_at(&path, &legacy).unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        let before = std::fs::read(&path).unwrap();
        let report = summary_at(&path, &query(at + 1)).unwrap();
        assert_eq!(
            report.workspaces[0].surfaces[0].unclassified_propose_errors,
            1
        );
        assert_eq!(
            before,
            std::fs::read(&path).unwrap(),
            "read-only集計でmigrationしない"
        );

        let rejected = LedgerEvent::write(
            ctx,
            at,
            "new",
            WriteTool::Update,
            WriteOutcome::Error,
            Some(WriteRejection::TagVocabulary),
        )
        .unwrap();
        append_at(&path, &rejected).unwrap();
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 2);
        let report = summary_at(&path, &query(at + 1)).unwrap();
        let row = &report.workspaces[0].surfaces[0];
        assert_eq!(row.propose_errors, 1);
        assert_eq!(row.update_errors, 1);
        assert_eq!(row.unclassified_update_errors, 0);
        assert_eq!(row.write_rejections[0].code, WriteRejection::TagVocabulary);
        assert_eq!(row.write_rejections[0].update, 1);
        assert_eq!(row.write_rejections[0].propose, 0);
        assert!(
            LedgerEvent::write(
                ctx,
                at,
                "invalid",
                WriteTool::Propose,
                WriteOutcome::Success,
                Some(WriteRejection::TagVocabulary)
            )
            .is_err()
        );
    }

    #[test]
    fn v1_migration_preserves_pending_receipts_and_unknown_codes_are_not_erased() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite3");
        let at = now_ms();
        let ctx = context(Some(WORKSPACE_A), Some("one"), Some("turn"));
        let event = prepared(ctx, at);
        let receipt = append_at(&path, &event).unwrap().receipt.unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        let write = LedgerEvent::write(
            ctx,
            at,
            "new",
            WriteTool::Propose,
            WriteOutcome::Error,
            Some(WriteRejection::TagVocabulary),
        )
        .unwrap();
        append_at(&path, &write).unwrap();
        finalize_hook_at(&path, &receipt, HookEmissionOutcome::Emitted).unwrap();
        assert_eq!(
            summary_at(&path, &query(at + 1)).unwrap().workspaces[0].surfaces[0]
                .hook_output_emitted,
            1
        );
        let mut unknown = serde_json::to_value(&write).unwrap();
        unknown["observation"]["code"] = json!("future_rejection");
        conn.execute(
            "UPDATE ledger_events SET payload=?1 WHERE event_id=?2",
            params![unknown.to_string(), write.event_id],
        )
        .unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(summary_at(&path, &query(at + 1)).is_err());
        assert_eq!(before, std::fs::read(&path).unwrap());
    }

    fn context<'a>(
        workspace: Option<&'a str>,
        session: Option<&'a str>,
        turn: Option<&'a str>,
    ) -> EventContext<'a> {
        EventContext {
            surface: ClientSurface::CodexCli,
            workspace_id: workspace,
            session_id: session,
            prompt_id: None,
            turn_id: turn,
            permission_mode: None,
        }
    }

    fn filtered(context: EventContext<'_>, at: i64) -> LedgerEvent {
        LedgerEvent::hook(
            context,
            at,
            HookObservation::Filtered {
                reason: HookFilterReason::ShortPrompt,
                timings: HookTimings::default(),
            },
        )
        .unwrap()
    }

    fn prepared(context: EventContext<'_>, at: i64) -> LedgerEvent {
        let delivery = render_hook_delivery(&json!({
            "hits": [{"id": "notes/private-title"}],
            "documents": [{"id": "notes/private-title", "text": "本文秘密:DO_NOT_STORE_THIS_BODY"}]
        }), context.surface.hook_output_budget()).unwrap();
        LedgerEvent::hook(
            context,
            at,
            HookObservation::OutputPrepared {
                stats: delivery.stats,
                timings: HookTimings {
                    initialize_ms: Some(1),
                    search_ms: Some(2),
                    render_ms: Some(3),
                },
                cap_assumption: CapAssumption::for_surface(context.surface),
            },
        )
        .unwrap()
    }

    fn query(until: i64) -> SummaryQuery {
        SummaryQuery {
            since_ms: 0,
            until_ms: until,
            workspace_id: None,
        }
    }

    #[test]
    fn append_reopens_deduplicates_turns_and_keeps_distinct_write_retries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/ledger.sqlite3");
        let at = now_ms();
        let ctx = context(Some(WORKSPACE_A), Some("session"), Some("turn"));
        let event = filtered(ctx, at);
        assert_eq!(
            append_at(&path, &event).unwrap().disposition,
            AppendDisposition::Inserted
        );
        let duplicate = filtered(ctx, at + 1);
        assert_eq!(
            append_at(&path, &duplicate).unwrap().disposition,
            AppendDisposition::Deduplicated
        );
        for call_id in ["process-a/request-1", "process-b/request-1"] {
            let write = LedgerEvent::write(
                ctx,
                at,
                call_id,
                WriteTool::Propose,
                WriteOutcome::Success,
                None,
            )
            .unwrap();
            assert_eq!(
                append_at(&path, &write).unwrap().disposition,
                AppendDisposition::Inserted
            );
        }
        let summary = summary_at(&path, &query(at + 2)).unwrap();
        let surface = &summary.workspaces[0].surfaces[0];
        assert_eq!(surface.hook_filtered, 1);
        assert_eq!(surface.propose_successes, 2);
        assert_eq!(surface.last_successful_propose_at_ms, Some(at));
    }

    #[test]
    fn concurrent_appends_preserve_all_distinct_observations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite3");
        let at = now_ms();
        append_at(
            &path,
            &filtered(context(Some(WORKSPACE_A), None, Some("seed")), at),
        )
        .unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let threads = (0..4)
            .map(|index| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let call = format!("process-{index}/request-1");
                    let event = LedgerEvent::write(
                        context(Some(WORKSPACE_A), None, None),
                        at,
                        &call,
                        WriteTool::Update,
                        WriteOutcome::Success,
                        None,
                    )
                    .unwrap();
                    barrier.wait();
                    append_at(&path, &event).unwrap()
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            assert_eq!(
                thread.join().unwrap().disposition,
                AppendDisposition::Inserted
            );
        }
        assert_eq!(
            summary_at(&path, &query(at + 1)).unwrap().workspaces[0].surfaces[0].update_successes,
            4
        );
    }

    #[test]
    fn scopes_keep_surface_workspace_and_unknown_workspace_separate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite3");
        let at = now_ms();
        let mut hashes = HashSet::new();
        for (workspace, surface) in [
            (Some(WORKSPACE_A), ClientSurface::CodexCli),
            (Some(WORKSPACE_A), ClientSurface::ClaudeCode),
            (Some(WORKSPACE_B), ClientSurface::CodexCli),
            (None, ClientSurface::CodexCli),
        ] {
            let mut ctx = context(workspace, Some("same-session"), Some("same-turn"));
            ctx.surface = surface;
            let event = filtered(ctx, at);
            hashes.insert(event.session_hash.clone());
            append_at(&path, &event).unwrap();
        }
        assert_eq!(hashes.len(), 4);
        let mut filter = query(at + 1);
        filter.workspace_id = Some(WORKSPACE_A.into());
        let summary = summary_at(&path, &filter).unwrap();
        assert_eq!(summary.workspaces.len(), 1);
        assert_eq!(summary.workspaces[0].workspace_id, WORKSPACE_A);
        assert_eq!(summary.workspaces[0].surfaces.len(), 2);
        assert!(
            summary.workspaces[0]
                .surfaces
                .iter()
                .all(|surface| surface.hook_groups.actual_sessions == 1)
        );
        assert_eq!(summary.unassigned.len(), 1);
        assert_eq!(summary.unassigned[0].hook_filtered, 1);
    }

    #[test]
    fn fallback_days_never_become_sessions_or_join_hook_and_write_groups() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite3");
        let at = now_ms();
        let ctx = context(Some(WORKSPACE_A), None, None);
        let first = filtered(ctx, at);
        let second = filtered(ctx, at);
        assert_ne!(first.event_id, second.event_id);
        append_at(&path, &first).unwrap();
        append_at(&path, &second).unwrap();
        let write = LedgerEvent::write(
            ctx,
            at,
            "process/request",
            WriteTool::Propose,
            WriteOutcome::Error,
            None,
        )
        .unwrap();
        append_at(&path, &write).unwrap();
        let actual_hook = filtered(
            context(Some(WORKSPACE_A), Some("actual-session"), Some("one")),
            at,
        );
        append_at(&path, &actual_hook).unwrap();
        let summary = summary_at(&path, &query(at + 1)).unwrap();
        let surface = &summary.workspaces[0].surfaces[0];
        assert_eq!(surface.hook_groups.actual_sessions, 1);
        assert_eq!(surface.hook_groups.daily_fallback_days, 1);
        assert_eq!(surface.write_groups.actual_sessions, 0);
        assert_eq!(surface.write_groups.daily_fallback_days, 1);
        assert_eq!(surface.hook_filtered, 3);
        assert_eq!(surface.propose_errors, 1);
    }

    #[test]
    fn prepared_receipts_only_finalize_the_exact_observation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite3");
        let at = now_ms();
        let ctx = context(Some(WORKSPACE_A), Some("session"), Some("turn"));
        let event = prepared(ctx, at);
        let receipt = append_at(&path, &event).unwrap().receipt.unwrap();
        let duplicate = append_at(&path, &prepared(ctx, at + 1)).unwrap();
        assert_eq!(duplicate.disposition, AppendDisposition::Deduplicated);
        assert!(duplicate.receipt.is_none());
        let before = summary_at(&path, &query(at + 2)).unwrap();
        assert_eq!(before.workspaces[0].surfaces[0].hook_output_prepared, 1);
        assert_eq!(before.workspaces[0].surfaces[0].emitted_bytes, 0);
        let wrong = HookReceipt {
            event_id: receipt.event_id.clone(),
            token: "0".repeat(64),
        };
        assert!(finalize_hook_at(&path, &wrong, HookEmissionOutcome::Emitted).is_err());
        finalize_hook_at(&path, &receipt, HookEmissionOutcome::Emitted).unwrap();
        finalize_hook_at(&path, &receipt, HookEmissionOutcome::Emitted).unwrap();
        assert!(finalize_hook_at(&path, &receipt, HookEmissionOutcome::StdoutFailed).is_err());
        let second = prepared(
            context(Some(WORKSPACE_A), Some("session"), Some("other-turn")),
            at,
        );
        let failed = append_at(&path, &second).unwrap().receipt.unwrap();
        finalize_hook_at(&path, &failed, HookEmissionOutcome::StdoutFailed).unwrap();
        let after = summary_at(&path, &query(at + 2)).unwrap();
        let surface = &after.workspaces[0].surfaces[0];
        assert_eq!(surface.hook_output_prepared, 0);
        assert_eq!(surface.hook_output_emitted, 1);
        assert_eq!(surface.hook_stdout_failed, 1);
        assert_eq!(surface.emitted_documents, 1);
    }

    #[test]
    fn no_raw_identifiers_note_payload_or_arbitrary_permission_strings_reach_storage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite3");
        let at = now_ms();
        let mut ctx = context(
            Some(WORKSPACE_A),
            Some("/private/user/RAW_SESSION_SECRET"),
            Some("RAW_TURN_SECRET"),
        );
        ctx.prompt_id = Some("RAW_PROMPT_ID_SECRET");
        ctx.permission_mode = PermissionMode::from_hint("RAW_PERMISSION_SECRET");
        append_at(&path, &prepared(ctx, at)).unwrap();
        let write = LedgerEvent::write(
            ctx,
            at,
            "RAW_CALL_SECRET",
            WriteTool::Update,
            WriteOutcome::Error,
            None,
        )
        .unwrap();
        append_at(&path, &write).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let stored = String::from_utf8_lossy(&bytes);
        for raw in [
            "RAW_SESSION_SECRET",
            "RAW_TURN_SECRET",
            "RAW_PROMPT_ID_SECRET",
            "RAW_PERMISSION_SECRET",
            "RAW_CALL_SECRET",
            "private-title",
            "DO_NOT_STORE_THIS_BODY",
        ] {
            assert!(!stored.contains(raw), "{raw}");
        }
        assert!(stored.contains("output_prepared"));
        assert!(stored.contains(WORKSPACE_A));
    }

    #[test]
    fn readonly_missing_summary_creates_nothing_and_existing_summary_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent/ledger.sqlite3");
        let at = now_ms();
        assert!(!summary_at(&path, &query(at + 1)).unwrap().exists);
        assert!(!path.parent().unwrap().exists());
        append_at(&path, &filtered(context(None, None, None), at)).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert!(summary_at(&path, &query(at + 1)).unwrap().exists);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified
        );
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn corrupt_future_and_foreign_schemas_are_rejected_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let at = now_ms();
        let event = filtered(context(None, None, None), at);
        for kind in ["corrupt", "future", "foreign", "malformed"] {
            let path = dir.path().join(format!("{kind}.sqlite3"));
            match kind {
                "corrupt" => std::fs::write(&path, "not a database: do not overwrite").unwrap(),
                "future" => {
                    append_at(&path, &event).unwrap();
                    Connection::open(&path)
                        .unwrap()
                        .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
                        .unwrap();
                }
                "foreign" => Connection::open(&path)
                    .unwrap()
                    .execute_batch("CREATE TABLE other(data TEXT)")
                    .unwrap(),
                _ => {
                    append_at(&path, &event).unwrap();
                    Connection::open(&path)
                        .unwrap()
                        .execute_batch("ALTER TABLE ledger_events ADD COLUMN extra TEXT")
                        .unwrap();
                }
            }
            let before = std::fs::read(&path).unwrap();
            assert!(summary_at(&path, &query(at + 1)).is_err(), "{kind}");
            assert!(append_at(&path, &event).is_err(), "{kind}");
            assert_eq!(std::fs::read(&path).unwrap(), before, "{kind}");
        }
    }

    #[test]
    fn time_window_is_half_open_and_retention_expires_after_ninety_days() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite3");
        let at = now_ms();
        let boundary = at - RETENTION_DAYS * DAY_MS;
        let ctx = context(Some(WORKSPACE_A), None, None);
        let old = filtered(ctx, boundary - 1);
        append_at_time(&path, &old, boundary - 1).unwrap();
        let retained = LedgerEvent::write(
            ctx,
            boundary,
            "process/boundary",
            WriteTool::Propose,
            WriteOutcome::Success,
            None,
        )
        .unwrap();
        append_at_time(&path, &retained, boundary).unwrap();
        let current = filtered(ctx, at);
        append_at_time(&path, &current, at).unwrap();
        let window = SummaryQuery {
            since_ms: boundary,
            until_ms: at,
            workspace_id: None,
        };
        let summary = summary_at(&path, &window).unwrap();
        assert_eq!(summary.workspaces[0].surfaces[0].propose_successes, 1);
        assert_eq!(summary.workspaces[0].surfaces[0].hook_filtered, 0);
        assert_eq!(
            summary.workspaces[0].surfaces[0].last_successful_propose_at_ms,
            Some(boundary)
        );
        let count: i64 = Connection::open(&path)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM ledger_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
        assert_eq!(
            append_at_time(&path, &old, at).unwrap().disposition,
            AppendDisposition::Expired
        );
        let future = filtered(ctx, at + 60_001);
        assert!(append_at_time(&path, &future, at).is_err());
        assert!(
            summary_at(
                &path,
                &SummaryQuery {
                    since_ms: at,
                    until_ms: at,
                    workspace_id: None
                }
            )
            .is_err()
        );
    }

    #[test]
    fn invalid_workspace_unknown_surface_and_bad_hashes_cannot_be_appended() {
        let at = now_ms();
        assert!(
            LedgerEvent::hook(
                context(Some("/private/raw-path"), None, None),
                at,
                HookObservation::Error {
                    stage: HookErrorStage::Input,
                    timings: HookTimings::default()
                }
            )
            .is_err()
        );
        let mut ctx = context(None, None, None);
        ctx.surface = ClientSurface::Unknown;
        assert!(
            LedgerEvent::hook(
                ctx,
                at,
                HookObservation::Error {
                    stage: HookErrorStage::Unknown,
                    timings: HookTimings::default()
                }
            )
            .is_err()
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing/ledger.sqlite3");
        let mut event = filtered(context(None, None, None), at);
        event.session_hash = Some("raw-session".into());
        assert!(append_at(&path, &event).is_err());
        assert!(!path.exists());
    }
}
