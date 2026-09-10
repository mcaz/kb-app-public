//! 継続蒸留checkpointを端末ローカルに保持し、監査深度をcadenceで選ぶ。
//!
//! 時間で意味変更を自動実行しない。ここが永続化するのは最後に受入gateを通った
//! checkpoint・Artifact変更印・実行時刻で、semantic executorのsnapshot再照合は省略しない。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fs4::fs_std::FileExt;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::authority::{AuthorityRole, AuthorityStatus};
use crate::distillation::DistillationPlanEntry;
use crate::distillation_audit::{
    DistillationAuditCheckCode, DistillationAuditReport, DistillationCheckpoint,
};
use crate::vault::Vault;

pub const CADENCE_STATE_SCHEMA: &str = "kb-app.distillation-cadence-state/v1";
pub const CADENCE_STATUS_SCHEMA: &str = "kb-app.distillation-cadence-status/v1";
pub const CADENCE_RUN_SCHEMA: &str = "kb-app.distillation-cadence-run/v1";

const DAY_SECONDS: i64 = 24 * 60 * 60;
const WEEK_SECONDS: i64 = 7 * DAY_SECONDS;
// 月境界は端末timezoneで揺れるため、v1は30日間隔へ固定する。
const MONTH_SECONDS: i64 = 30 * DAY_SECONDS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistillationCadenceLane {
    AfterWrite,
    Daily,
    Weekly,
    Monthly,
}

impl DistillationCadenceLane {
    pub const ALL: [Self; 4] = [Self::AfterWrite, Self::Daily, Self::Weekly, Self::Monthly];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AfterWrite => "after_write",
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }

    const fn interval_seconds(self) -> Option<i64> {
        match self {
            Self::AfterWrite => None,
            Self::Daily => Some(DAY_SECONDS),
            Self::Weekly => Some(WEEK_SECONDS),
            Self::Monthly => Some(MONTH_SECONDS),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompletedAt {
    daily: Option<String>,
    weekly: Option<String>,
    monthly: Option<String>,
}

impl CompletedAt {
    fn get(&self, lane: DistillationCadenceLane) -> Option<&str> {
        match lane {
            DistillationCadenceLane::AfterWrite => None,
            DistillationCadenceLane::Daily => self.daily.as_deref(),
            DistillationCadenceLane::Weekly => self.weekly.as_deref(),
            DistillationCadenceLane::Monthly => self.monthly.as_deref(),
        }
    }

    fn set(&mut self, lane: DistillationCadenceLane, at: &str) {
        match lane {
            DistillationCadenceLane::AfterWrite => {}
            DistillationCadenceLane::Daily => self.daily = Some(at.to_string()),
            DistillationCadenceLane::Weekly => self.weekly = Some(at.to_string()),
            DistillationCadenceLane::Monthly => self.monthly = Some(at.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistillationCadenceFailure {
    pub at: String,
    pub audit_id: String,
    pub lanes: Vec<DistillationCadenceLane>,
    pub failed_checks: Vec<DistillationAuditCheckCode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DistillationCadenceState {
    schema: String,
    checkpoint: Option<DistillationCheckpoint>,
    // 2026-09-08: 旧v1は変更印を持たないため、一度再監査する。旧binaryへの逆互換は持たない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    accepted_artifact_stamp: Option<String>,
    completed_at: CompletedAt,
    last_failure: Option<DistillationCadenceFailure>,
}

impl Default for DistillationCadenceState {
    fn default() -> Self {
        Self {
            schema: CADENCE_STATE_SCHEMA.to_string(),
            checkpoint: None,
            accepted_artifact_stamp: None,
            completed_at: CompletedAt::default(),
            last_failure: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistillationCadenceLaneStatus {
    pub lane: DistillationCadenceLane,
    pub due: bool,
    pub last_completed_at: Option<String>,
    pub next_due_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistillationCadenceStatus {
    pub schema: String,
    pub checked_at: String,
    pub state_exists: bool,
    pub current_checkpoint_id: String,
    pub accepted_checkpoint_id: Option<String>,
    // 保存済みhook/ledgerに含まれる旧statusを引き続き読めるよう欠落を許す。
    #[serde(default)]
    pub current_artifact_stamp: Option<String>,
    #[serde(default)]
    pub accepted_artifact_stamp: Option<String>,
    pub lanes: Vec<DistillationCadenceLaneStatus>,
    pub last_failure: Option<DistillationCadenceFailure>,
}

impl DistillationCadenceStatus {
    pub fn due_lanes(&self) -> Vec<DistillationCadenceLane> {
        self.lanes
            .iter()
            .filter(|status| status.due)
            .map(|status| status.lane)
            .collect()
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistillationCadenceRunArguments {
    #[serde(default)]
    pub lane: Option<DistillationCadenceLane>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DistillationCadenceReviewItem {
    pub note: String,
    pub reasons: Vec<DistillationCadenceLane>,
}

#[derive(Debug, Serialize)]
pub struct DistillationCadenceRunReport {
    pub schema: &'static str,
    pub executed: bool,
    pub accepted: bool,
    pub selected_lanes: Vec<DistillationCadenceLane>,
    pub status_before: DistillationCadenceStatus,
    pub status_after: DistillationCadenceStatus,
    pub review_scope: Vec<DistillationCadenceReviewItem>,
    pub audit: Option<DistillationAuditReport>,
    pub state_persisted: bool,
}

pub fn status(vault: &Vault, conn: &Connection) -> Result<DistillationCadenceStatus> {
    let paths = StatePaths::for_vault(vault)?;
    status_at_path(vault, conn, &paths.state, OffsetDateTime::now_utc())
}

/// 書込後の判断支援では、plannerと同じsnapshotから期限を計算して全件走査の重複を避ける。
pub(crate) fn status_for_plan(
    vault: &Vault,
    plan: &crate::distillation::DistillationPlan,
) -> Result<DistillationCadenceStatus> {
    let paths = StatePaths::for_vault(vault)?;
    let (state, state_exists) = load_state(&paths.state)?;
    build_status(
        &DistillationCheckpoint::from_plan(plan).checkpoint_id,
        Some(&crate::artifact_metadata::stamp(vault)?),
        &state,
        state_exists,
        OffsetDateTime::now_utc(),
    )
}

pub fn run(
    vault: &Vault,
    conn: &Connection,
    arguments: DistillationCadenceRunArguments,
) -> Result<DistillationCadenceRunReport> {
    let paths = StatePaths::for_vault(vault)?;
    run_at_paths(vault, conn, arguments, OffsetDateTime::now_utc(), &paths)
}

pub(crate) fn status_for_checkpoint(
    vault: &Vault,
    checkpoint: &str,
) -> Result<DistillationCadenceStatus> {
    let paths = StatePaths::for_vault(vault)?;
    let (state, exists) = load_state(&paths.state)?;
    build_status(
        checkpoint,
        Some(&crate::artifact_metadata::stamp(vault)?),
        &state,
        exists,
        OffsetDateTime::now_utc(),
    )
}

fn status_at_path(
    vault: &Vault,
    conn: &Connection,
    state_path: &Path,
    now: OffsetDateTime,
) -> Result<DistillationCadenceStatus> {
    let (state, state_exists) = load_state(state_path)?;
    let plan = crate::distillation::plan(conn)?;
    let current = DistillationCheckpoint::from_plan(&plan);
    build_status(
        &current.checkpoint_id,
        Some(&crate::artifact_metadata::stamp(vault)?),
        &state,
        state_exists,
        now,
    )
}

fn run_at_paths(
    vault: &Vault,
    conn: &Connection,
    arguments: DistillationCadenceRunArguments,
    now: OffsetDateTime,
    paths: &StatePaths,
) -> Result<DistillationCadenceRunReport> {
    run_at_paths_with_audit(
        vault,
        conn,
        arguments,
        now,
        paths,
        crate::distillation_audit::audit,
    )
}

fn run_at_paths_with_audit(
    vault: &Vault,
    conn: &Connection,
    arguments: DistillationCadenceRunArguments,
    now: OffsetDateTime,
    paths: &StatePaths,
    audit: impl FnOnce(
        &Vault,
        &Connection,
        Option<&DistillationCheckpoint>,
    ) -> Result<DistillationAuditReport>,
) -> Result<DistillationCadenceRunReport> {
    fs::create_dir_all(&paths.directory).context("蒸留cadence状態directoryを作れない")?;
    let lock = fs::File::create(&paths.lock).context("蒸留cadence lockを作れない")?;
    lock.lock_exclusive()
        .context("蒸留cadence lockを取得できない")?;

    let (mut state, state_exists) = load_state(&paths.state)?;
    let plan = crate::distillation::plan(conn)?;
    let current = DistillationCheckpoint::from_plan(&plan);
    let before_stamp = crate::artifact_metadata::stamp(vault)?;
    let status_before = build_status(
        &current.checkpoint_id,
        Some(&before_stamp),
        &state,
        state_exists,
        now,
    )?;
    let selected_lanes = arguments
        .lane
        .map(|lane| vec![lane])
        .unwrap_or_else(|| status_before.due_lanes());

    if selected_lanes.is_empty() {
        return Ok(DistillationCadenceRunReport {
            schema: CADENCE_RUN_SCHEMA,
            executed: false,
            accepted: false,
            selected_lanes,
            status_after: status_before.clone(),
            status_before,
            review_scope: Vec::new(),
            audit: None,
            state_persisted: false,
        });
    }

    let mut report = audit(vault, conn, state.checkpoint.as_ref())?;
    let after_stamp = crate::artifact_metadata::stamp(vault);
    let stable = after_stamp
        .as_ref()
        .is_ok_and(|stamp| stamp == &before_stamp);
    let detail = match &after_stamp {
        Ok(_) if stable => None,
        Ok(_) => Some("監査中にArtifactメタデータが変わったため、変更印とcheckpointを受け入れなかった。再監査が必要。".into()),
        Err(error) => Some(format!("監査後のArtifactメタデータを確認できず、変更印とcheckpointを受け入れなかった。再監査が必要: {error:#}")),
    };
    crate::distillation_audit::check_artifact_metadata(&mut report, stable, detail)?;
    let after_stamp = after_stamp.ok();
    let review_scope = build_review_scope(&report, &selected_lanes);
    let at = format_at(now)?;
    if report.gate.passed {
        state.checkpoint = Some(report.checkpoint.clone());
        state.accepted_artifact_stamp = Some(before_stamp);
        for lane in &selected_lanes {
            state.completed_at.set(*lane, &at);
        }
        state.last_failure = None;
    } else {
        state.last_failure = Some(DistillationCadenceFailure {
            at,
            audit_id: report.audit_id.clone(),
            lanes: selected_lanes.clone(),
            failed_checks: report
                .gate
                .checks
                .iter()
                .filter(|check| !check.passed)
                .map(|check| check.code)
                .collect(),
        });
    }
    save_state(&paths.state, &state)?;
    let status_after = build_status(
        &report.checkpoint.checkpoint_id,
        after_stamp.as_deref(),
        &state,
        true,
        now,
    )?;
    let accepted = report.gate.passed;

    Ok(DistillationCadenceRunReport {
        schema: CADENCE_RUN_SCHEMA,
        executed: true,
        accepted,
        selected_lanes,
        status_before,
        status_after,
        review_scope,
        audit: Some(report),
        state_persisted: true,
    })
}

fn build_status(
    current: &str,
    current_artifact_stamp: Option<&str>,
    state: &DistillationCadenceState,
    state_exists: bool,
    now: OffsetDateTime,
) -> Result<DistillationCadenceStatus> {
    let changed = state
        .checkpoint
        .as_ref()
        .is_none_or(|accepted| accepted.checkpoint_id != current)
        || current_artifact_stamp.is_none()
        || state.accepted_artifact_stamp.as_deref() != current_artifact_stamp;
    let mut lanes = Vec::with_capacity(DistillationCadenceLane::ALL.len());
    for lane in DistillationCadenceLane::ALL {
        if lane == DistillationCadenceLane::AfterWrite {
            lanes.push(DistillationCadenceLaneStatus {
                lane,
                due: changed,
                last_completed_at: None,
                next_due_at: None,
            });
            continue;
        }
        let last = state.completed_at.get(lane).map(str::to_string);
        let next = last
            .as_deref()
            .map(parse_at)
            .transpose()?
            .map(|last| last + time::Duration::seconds(lane.interval_seconds().unwrap()));
        lanes.push(DistillationCadenceLaneStatus {
            lane,
            due: next.is_none_or(|next| now >= next),
            last_completed_at: last,
            next_due_at: next.map(format_at).transpose()?,
        });
    }
    Ok(DistillationCadenceStatus {
        schema: CADENCE_STATUS_SCHEMA.into(),
        checked_at: format_at(now)?,
        state_exists,
        current_checkpoint_id: current.to_owned(),
        accepted_checkpoint_id: state
            .checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.checkpoint_id.clone()),
        current_artifact_stamp: current_artifact_stamp.map(str::to_owned),
        accepted_artifact_stamp: state.accepted_artifact_stamp.clone(),
        lanes,
        last_failure: state.last_failure.clone(),
    })
}

fn build_review_scope(
    report: &DistillationAuditReport,
    lanes: &[DistillationCadenceLane],
) -> Vec<DistillationCadenceReviewItem> {
    let direct_changes = report
        .delta
        .added
        .iter()
        .chain(report.delta.changed.iter())
        .chain(report.delta.moved.iter().map(|moved| &moved.to))
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut scope = BTreeMap::<String, BTreeSet<DistillationCadenceLane>>::new();

    for lane in lanes {
        let selected = match lane {
            DistillationCadenceLane::AfterWrite => {
                let mut selected = direct_changes.clone();
                // 削除されたnoteは現在snapshotに存在しないため、参照側の現存noteを
                // 直接差分の代わりに確認する。
                if !report.delta.removed.is_empty() {
                    selected.extend(report.delta.workset.iter().cloned());
                }
                selected
            }
            DistillationCadenceLane::Daily => report
                .delta
                .workset
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>(),
            DistillationCadenceLane::Weekly => report
                .delta
                .workset
                .iter()
                .cloned()
                .chain(report.plan.entries.iter().filter_map(active_canonical_note))
                .collect::<BTreeSet<_>>(),
            DistillationCadenceLane::Monthly => report
                .plan
                .entries
                .iter()
                .map(|entry| entry.note.clone())
                .collect(),
        };
        for note in selected {
            scope.entry(note).or_default().insert(*lane);
        }
    }

    scope
        .into_iter()
        .map(|(note, reasons)| DistillationCadenceReviewItem {
            note,
            reasons: reasons.into_iter().collect(),
        })
        .collect()
}

fn active_canonical_note(entry: &DistillationPlanEntry) -> Option<String> {
    entry.authority.as_ref().and_then(|authority| {
        (authority.role == AuthorityRole::Canonical && authority.status == AuthorityStatus::Active)
            .then(|| entry.note.clone())
    })
}

fn load_state(path: &Path) -> Result<(DistillationCadenceState, bool)> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((DistillationCadenceState::default(), false));
        }
        Err(error) => return Err(error).context("蒸留cadence状態を読めない"),
    };
    let state: DistillationCadenceState =
        serde_json::from_str(&text).context("蒸留cadence状態JSONが不正")?;
    validate_state(&state)?;
    Ok((state, true))
}

fn validate_state(state: &DistillationCadenceState) -> Result<()> {
    if state.schema != CADENCE_STATE_SCHEMA {
        bail!("蒸留cadence state schemaが一致しない")
    }
    if let Some(checkpoint) = &state.checkpoint {
        crate::distillation_audit::validate_checkpoint(checkpoint)?;
    }
    if let Some(stamp) = &state.accepted_artifact_stamp {
        let valid = stamp.strip_prefix("sha256:").is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        if !valid {
            bail!("蒸留cadenceのArtifact変更印が不正")
        }
    }
    for at in [
        state.completed_at.daily.as_deref(),
        state.completed_at.weekly.as_deref(),
        state.completed_at.monthly.as_deref(),
        state
            .last_failure
            .as_ref()
            .map(|failure| failure.at.as_str()),
    ]
    .into_iter()
    .flatten()
    {
        parse_at(at)?;
    }
    Ok(())
}

fn save_state(path: &Path, state: &DistillationCadenceState) -> Result<()> {
    validate_state(state)?;
    let parent = path
        .parent()
        .context("蒸留cadence状態に親directoryがない")?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, state)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .context("蒸留cadence状態を確定できない")?;
    Ok(())
}

fn parse_at(value: &str) -> Result<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339)
        .with_context(|| format!("蒸留cadence日時がRFC 3339ではない: {value}"))
}

fn format_at(value: OffsetDateTime) -> Result<String> {
    value
        .format(&Rfc3339)
        .context("蒸留cadence日時をformatできない")
}

struct StatePaths {
    directory: PathBuf,
    state: PathBuf,
    lock: PathBuf,
}

impl StatePaths {
    fn for_vault(vault: &Vault) -> Result<Self> {
        let workspace_id = crate::workspace::stored_workspace_id(vault)?;
        let directory = crate::app_data_dir()?
            .join("distillation-cadence")
            .join(workspace_id);
        Ok(Self {
            state: directory.join("state.json"),
            lock: directory.join("run.lock"),
            directory,
        })
    }
}

pub fn render_markdown(report: &DistillationCadenceRunReport) -> String {
    let selected = report
        .selected_lanes
        .iter()
        .map(|lane| lane.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut output = format!(
        "# Distillation cadence\n\n- executed: {}\n- accepted: {}\n- lanes: {}\n- review scope: {}\n- artifact current: `{}`\n- artifact accepted: `{}`\n",
        report.executed,
        report.accepted,
        if selected.is_empty() {
            "none"
        } else {
            &selected
        },
        report.review_scope.len(),
        report
            .status_after
            .current_artifact_stamp
            .as_deref()
            .unwrap_or("unverified"),
        report
            .status_after
            .accepted_artifact_stamp
            .as_deref()
            .unwrap_or("none"),
    );
    if let Some(audit) = &report.audit {
        output.push_str(&format!(
            "- audit: `{}`\n- checkpoint: `{}`\n",
            audit.audit_id, audit.checkpoint.checkpoint_id
        ));
        if let Some(check) = audit.gate.checks.iter().find(|check| {
            check.code == DistillationAuditCheckCode::ArtifactMetadataStable && !check.passed
        }) {
            output.push_str(&format!(
                "- artifact_metadata_stable: false — {}\n",
                check
                    .detail
                    .as_deref()
                    .unwrap_or("Artifactメタデータが未確認のため再監査が必要")
            ));
        }
    }
    output.push_str("\n## Review scope\n\n");
    if report.review_scope.is_empty() {
        output.push_str("- 対象なし\n");
    } else {
        for item in &report.review_scope {
            let reasons = item
                .reasons
                .iter()
                .map(|lane| lane.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            output.push_str(&format!("- `{}` ({})\n", item.note, reasons));
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{Authority, NoteNamespace};
    use crate::index::open_db;
    use crate::vault::{NoteProposal, Vault};

    fn setup() -> (tempfile::TempDir, Vault, Connection, StatePaths) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        let conn = open_db(&vault).unwrap();
        let directory = dir.path().join("state");
        let paths = StatePaths {
            state: directory.join("state.json"),
            lock: directory.join("run.lock"),
            directory,
        };
        (dir, vault, conn, paths)
    }

    fn at(day: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(day * DAY_SECONDS).unwrap()
    }

    #[test]
    fn first_run_selects_all_due_lanes_and_persists_only_a_passing_checkpoint() {
        let (_dir, vault, conn, paths) = setup();
        let report = run_at_paths(
            &vault,
            &conn,
            DistillationCadenceRunArguments::default(),
            at(100),
            &paths,
        )
        .unwrap();

        assert!(report.executed);
        assert!(report.accepted);
        assert_eq!(report.selected_lanes, DistillationCadenceLane::ALL);
        assert!(report.status_after.due_lanes().is_empty());
        assert_eq!(
            report.status_after.current_artifact_stamp,
            report.status_after.accepted_artifact_stamp
        );
        assert_eq!(report.audit.as_ref().unwrap().gate.checks.len(), 7);
        assert!(paths.state.is_file());
    }

    #[test]
    fn time_lanes_become_due_at_fixed_intervals_without_changing_the_checkpoint() {
        let (_dir, vault, conn, paths) = setup();
        run_at_paths(
            &vault,
            &conn,
            DistillationCadenceRunArguments::default(),
            at(100),
            &paths,
        )
        .unwrap();

        let daily = status_at_path(&vault, &conn, &paths.state, at(101)).unwrap();
        assert_eq!(daily.due_lanes(), vec![DistillationCadenceLane::Daily]);
        let weekly = status_at_path(&vault, &conn, &paths.state, at(107)).unwrap();
        assert_eq!(
            weekly.due_lanes(),
            vec![
                DistillationCadenceLane::Daily,
                DistillationCadenceLane::Weekly
            ]
        );
        let monthly = status_at_path(&vault, &conn, &paths.state, at(130)).unwrap();
        assert_eq!(
            monthly.due_lanes(),
            vec![
                DistillationCadenceLane::Daily,
                DistillationCadenceLane::Weekly,
                DistillationCadenceLane::Monthly,
            ]
        );
    }

    #[test]
    fn a_new_note_makes_after_write_due_and_a_failed_gate_does_not_advance_baseline() {
        let (_dir, vault, conn, paths) = setup();
        let initial = run_at_paths(
            &vault,
            &conn,
            DistillationCadenceRunArguments::default(),
            at(100),
            &paths,
        )
        .unwrap();
        let accepted = initial.status_after.accepted_checkpoint_id.unwrap();
        let accepted_artifact = initial.status_after.accepted_artifact_stamp.unwrap();
        let metadata = vault.root.join(crate::ledger::DIR);
        fs::create_dir_all(&metadata).unwrap();
        fs::write(metadata.join("aliases.json"), "{}").unwrap();
        vault
            .propose(
                &conn,
                NoteProposal {
                    judgment: None,
                    title: "未接続record",
                    body: "本文",
                    description: Some("説明"),
                    tags: &["test".into()],
                    authority: Authority {
                        namespace: NoteNamespace::Records,
                        role: AuthorityRole::Record,
                        status: AuthorityStatus::Active,
                        scope: "test/unlinked-record".into(),
                    },
                    relations: Vec::new(),
                    allow_new_tags: true,
                    client: "test/client",
                    actor: None,
                    revision: None,
                },
            )
            .unwrap();

        let status = status_at_path(&vault, &conn, &paths.state, at(100)).unwrap();
        assert_eq!(
            status.due_lanes(),
            vec![DistillationCadenceLane::AfterWrite]
        );
        let failed = run_at_paths(
            &vault,
            &conn,
            DistillationCadenceRunArguments {
                lane: Some(DistillationCadenceLane::AfterWrite),
            },
            at(100),
            &paths,
        )
        .unwrap();

        assert!(!failed.accepted);
        assert_eq!(
            failed.status_after.accepted_checkpoint_id.as_deref(),
            Some(accepted.as_str())
        );
        assert_eq!(
            failed.status_after.due_lanes(),
            vec![DistillationCadenceLane::AfterWrite]
        );
        assert!(failed.status_after.last_failure.is_some());
        assert_eq!(
            failed.status_after.accepted_artifact_stamp.as_deref(),
            Some(accepted_artifact.as_str())
        );
        assert_ne!(
            failed.status_after.current_artifact_stamp,
            failed.status_after.accepted_artifact_stamp
        );
    }

    /// 2026-09-08: 旧v1の本文checkpointだけではArtifact監査済みと認めず、一度受け入れ直す。
    #[test]
    fn old_state_without_artifact_stamp_requires_one_audit_and_old_status_still_loads() {
        let (_dir, vault, conn, paths) = setup();
        let initial = run_at_paths(
            &vault,
            &conn,
            DistillationCadenceRunArguments::default(),
            at(100),
            &paths,
        )
        .unwrap();
        let mut old_state: serde_json::Value =
            serde_json::from_slice(&fs::read(&paths.state).unwrap()).unwrap();
        old_state
            .as_object_mut()
            .unwrap()
            .remove("accepted_artifact_stamp");
        fs::write(&paths.state, serde_json::to_vec(&old_state).unwrap()).unwrap();
        let status = status_at_path(&vault, &conn, &paths.state, at(100)).unwrap();
        assert_eq!(
            status.due_lanes(),
            vec![DistillationCadenceLane::AfterWrite]
        );
        assert!(status.accepted_artifact_stamp.is_none());
        assert_eq!(
            status.accepted_checkpoint_id,
            initial.status_after.accepted_checkpoint_id
        );
        let accepted = run_at_paths(
            &vault,
            &conn,
            DistillationCadenceRunArguments::default(),
            at(100),
            &paths,
        )
        .unwrap();
        assert!(accepted.accepted);
        assert!(accepted.status_after.due_lanes().is_empty());
        let unchanged = run_at_paths(
            &vault,
            &conn,
            DistillationCadenceRunArguments::default(),
            at(100),
            &paths,
        )
        .unwrap();
        assert!(!unchanged.executed);

        let mut old_status = serde_json::to_value(&initial.status_after).unwrap();
        old_status
            .as_object_mut()
            .unwrap()
            .remove("current_artifact_stamp");
        old_status
            .as_object_mut()
            .unwrap()
            .remove("accepted_artifact_stamp");
        let loaded: DistillationCadenceStatus = serde_json::from_value(old_status).unwrap();
        assert!(loaded.current_artifact_stamp.is_none());
        assert!(loaded.accepted_artifact_stamp.is_none());
        let (mut state, _) = load_state(&paths.state).unwrap();
        state.accepted_artifact_stamp = None;
        assert!(
            build_status(
                &accepted.status_after.current_checkpoint_id,
                None,
                &state,
                true,
                at(100)
            )
            .unwrap()
            .due_lanes()
            .contains(&DistillationCadenceLane::AfterWrite)
        );
    }

    /// 2026-09-08: gate合格後のalias更新・破損を受入に混ぜず、監査IDと失敗履歴も一致させる。
    #[test]
    fn metadata_race_or_post_audit_read_failure_preserves_all_accepted_state() {
        for changed in ["{}", "{broken"] {
            let (_dir, vault, conn, paths) = setup();
            let initial = run_at_paths(
                &vault,
                &conn,
                DistillationCadenceRunArguments::default(),
                at(100),
                &paths,
            )
            .unwrap();
            assert!(initial.accepted);
            let (before, _) = load_state(&paths.state).unwrap();
            let mut original_audit_id = String::new();
            let raced = run_at_paths_with_audit(
                &vault,
                &conn,
                DistillationCadenceRunArguments {
                    lane: Some(DistillationCadenceLane::Daily),
                },
                at(101),
                &paths,
                |vault, conn, checkpoint| {
                    let report = crate::distillation_audit::audit(vault, conn, checkpoint)?;
                    assert!(report.gate.passed);
                    original_audit_id = report.audit_id.clone();
                    let root = vault.root.join(crate::ledger::DIR);
                    fs::create_dir_all(&root)?;
                    fs::write(root.join("aliases.json"), changed)?;
                    Ok(report)
                },
            )
            .unwrap();
            assert!(raced.executed);
            assert!(!raced.accepted);
            assert!(raced.state_persisted);
            let audit = raced.audit.as_ref().unwrap();
            assert!(!audit.gate.passed);
            assert_ne!(audit.audit_id, original_audit_id);
            let check = audit.gate.checks.last().unwrap();
            assert_eq!(
                check.code,
                DistillationAuditCheckCode::ArtifactMetadataStable
            );
            assert!(!check.passed);
            assert!(check.detail.as_deref().unwrap().contains("再監査"));
            assert_eq!(
                raced.status_after.current_artifact_stamp.is_none(),
                changed == "{broken"
            );
            assert!(
                raced
                    .status_after
                    .due_lanes()
                    .contains(&DistillationCadenceLane::AfterWrite)
            );
            assert!(render_markdown(&raced).contains("artifact_metadata_stable: false"));
            let (after, _) = load_state(&paths.state).unwrap();
            assert_eq!(after.checkpoint, before.checkpoint);
            assert_eq!(
                after.accepted_artifact_stamp,
                before.accepted_artifact_stamp
            );
            assert_eq!(after.completed_at, before.completed_at);
            let failure = after.last_failure.unwrap();
            assert_eq!(failure.audit_id, audit.audit_id);
            assert_eq!(
                failure.failed_checks,
                vec![DistillationAuditCheckCode::ArtifactMetadataStable]
            );
            if changed == "{broken" {
                let bytes = fs::read(&paths.state).unwrap();
                assert!(
                    run_at_paths(
                        &vault,
                        &conn,
                        DistillationCadenceRunArguments::default(),
                        at(101),
                        &paths
                    )
                    .is_err()
                );
                assert_eq!(bytes, fs::read(&paths.state).unwrap());
            }
        }
    }

    #[test]
    fn weekly_scope_adds_active_canonical_and_monthly_scope_adds_every_note() {
        let (_dir, vault, conn, paths) = setup();
        for (title, role, namespace, scope) in [
            (
                "正本",
                AuthorityRole::Canonical,
                NoteNamespace::Knowledge,
                "test/canonical",
            ),
            (
                "履歴",
                AuthorityRole::Record,
                NoteNamespace::Records,
                "test/record",
            ),
        ] {
            vault
                .propose(
                    &conn,
                    NoteProposal {
                        judgment: None,
                        title,
                        body: "本文",
                        description: Some("説明"),
                        tags: &["test".into()],
                        authority: Authority {
                            namespace,
                            role,
                            status: AuthorityStatus::Active,
                            scope: scope.into(),
                        },
                        relations: Vec::new(),
                        allow_new_tags: true,
                        client: "test/client",
                        actor: None,
                        revision: None,
                    },
                )
                .unwrap();
        }

        let weekly = run_at_paths(
            &vault,
            &conn,
            DistillationCadenceRunArguments {
                lane: Some(DistillationCadenceLane::Weekly),
            },
            at(100),
            &paths,
        )
        .unwrap();
        assert!(
            weekly
                .review_scope
                .iter()
                .any(|item| item.note.contains("正本"))
        );

        let monthly = run_at_paths(
            &vault,
            &conn,
            DistillationCadenceRunArguments {
                lane: Some(DistillationCadenceLane::Monthly),
            },
            at(100),
            &paths,
        )
        .unwrap();
        assert_eq!(monthly.review_scope.len(), 2);
    }

    #[test]
    fn broken_or_tampered_state_fails_closed() {
        let (_dir, vault, conn, paths) = setup();
        fs::create_dir_all(&paths.directory).unwrap();
        fs::write(
            &paths.state,
            r#"{"schema":"future","checkpoint":null,"completed_at":{"daily":null,"weekly":null,"monthly":null},"last_failure":null}"#,
        )
        .unwrap();

        let error = status_at_path(&vault, &conn, &paths.state, at(100)).unwrap_err();
        assert!(error.to_string().contains("schema"));
    }

    #[test]
    fn published_example_uses_the_current_cadence_contract() {
        let example: serde_json::Value = serde_json::from_str(include_str!(
            "../../../schemas/examples/distillation-cadence.example.json"
        ))
        .unwrap();

        assert_eq!(example["schema"], CADENCE_RUN_SCHEMA);
        assert_eq!(example["status_before"]["schema"], CADENCE_STATUS_SCHEMA);
        assert_eq!(example["selected_lanes"], serde_json::json!([]));
    }
    #[test]
    fn cached_digest_changes_on_due_boundary_or_state_change_but_not_check_time() {
        let state = DistillationCadenceState {
            checkpoint: None,
            completed_at: CompletedAt {
                daily: Some(format_at(at(100)).unwrap()),
                weekly: Some(format_at(at(100)).unwrap()),
                monthly: Some(format_at(at(100)).unwrap()),
            },
            ..Default::default()
        };
        let before = crate::cadence_cache::from_status(
            build_status("fixture", None, &state, true, at(100)).unwrap(),
            2,
        )
        .unwrap();
        let shortly = crate::cadence_cache::from_status(
            build_status(
                "fixture",
                None,
                &state,
                true,
                at(100) + time::Duration::seconds(10),
            )
            .unwrap(),
            2,
        )
        .unwrap();
        assert_eq!(before.digest, shortly.digest);
        let due = crate::cadence_cache::from_status(
            build_status("fixture", None, &state, true, at(101)).unwrap(),
            2,
        )
        .unwrap();
        assert_ne!(before.digest, due.digest);
        let changed_count = crate::cadence_cache::from_status(
            build_status("fixture", None, &state, true, at(100)).unwrap(),
            3,
        )
        .unwrap();
        assert_ne!(before.digest, changed_count.digest);
        let mut failed = state;
        failed.last_failure = Some(DistillationCadenceFailure {
            at: format_at(at(100)).unwrap(),
            audit_id: "failure".into(),
            lanes: vec![DistillationCadenceLane::AfterWrite],
            failed_checks: vec![DistillationAuditCheckCode::NoActionableEntries],
        });
        let after = crate::cadence_cache::from_status(
            build_status("fixture", None, &failed, true, at(100)).unwrap(),
            2,
        )
        .unwrap();
        assert_ne!(before.digest, after.digest);
    }
}
