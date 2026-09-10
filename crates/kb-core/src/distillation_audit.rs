//! 継続蒸留の増分worksetと受入gateを作るread-only監査。
//!
//! plannerの意味判定を増やさず、前回checkpointとの差分と現在の全体健全性を分離して返す。
//! checkpointは実行権限ではなく、executorは従来どおりplan全体を再照合する。

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::authority::NoteUid;
use crate::connect::BackupStatus;
use crate::distillation::{
    DistillationOperation, DistillationPlan, DistillationPlanEntry, DistillationRisk, PLAN_SCHEMA,
    PLANNER_PROFILE,
};
use crate::storage_contract::StorageReport;
use crate::vault::Vault;

pub const AUDIT_SCHEMA: &str = "kb-app.distillation-audit/v1";
pub const CHECKPOINT_SCHEMA: &str = "kb-app.distillation-checkpoint/v1";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistillationAuditArguments {
    #[serde(default)]
    pub baseline: Option<DistillationCheckpoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistillationCheckpointEntry {
    pub note: String,
    pub note_uid: Option<NoteUid>,
    pub input_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistillationCheckpoint {
    pub schema: String,
    pub checkpoint_id: String,
    pub plan_schema: String,
    pub planner_profile: String,
    pub plan_id: String,
    pub snapshot_digest: String,
    pub snapshot_note_count: usize,
    pub entries: Vec<DistillationCheckpointEntry>,
}

impl DistillationCheckpoint {
    pub fn from_plan(plan: &DistillationPlan) -> Self {
        let mut checkpoint = Self {
            schema: CHECKPOINT_SCHEMA.to_string(),
            checkpoint_id: String::new(),
            plan_schema: plan.schema.to_string(),
            planner_profile: plan.planner_profile.to_string(),
            plan_id: plan.plan_id.clone(),
            snapshot_digest: plan.snapshot.digest.clone(),
            snapshot_note_count: plan.snapshot.note_count,
            entries: plan
                .entries
                .iter()
                .map(|entry| DistillationCheckpointEntry {
                    note: entry.note.clone(),
                    note_uid: entry
                        .note_uid
                        .as_deref()
                        .map(str::parse)
                        .transpose()
                        .expect("plannerが検証済みのnote_uid"),
                    input_hash: entry.input_hash.clone(),
                })
                .collect(),
        };
        checkpoint.checkpoint_id = checkpoint_digest(&checkpoint);
        checkpoint
    }
}

#[derive(Serialize)]
struct CheckpointMaterial<'a> {
    schema: &'a str,
    plan_schema: &'a str,
    planner_profile: &'a str,
    plan_id: &'a str,
    snapshot_digest: &'a str,
    snapshot_note_count: usize,
    entries: &'a [DistillationCheckpointEntry],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DistillationAuditMode {
    Full,
    Incremental,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DistillationMove {
    pub note_uid: NoteUid,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DistillationDelta {
    pub mode: DistillationAuditMode,
    pub baseline_plan_id: Option<String>,
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
    pub moved: Vec<DistillationMove>,
    pub unchanged: usize,
    /// 追加・変更・移動と、未解決候補、その依存グラフ閉包。AIが全文確認する最小集合。
    pub workset: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistillationAuditCheckCode {
    NoActionableEntries,
    NoUnresolvedEntries,
    NoNonNoneRiskEntries,
    NoPendingMarkdownExports,
    StorageContractValid,
    RemoteBackupCurrentOrLocalOnly,
    ArtifactMetadataStable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DistillationAuditCheck {
    pub code: DistillationAuditCheckCode,
    pub passed: bool,
    /// 0が合格。件数で表せない検査は失敗を1件として返す。
    pub violations: usize,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DistillationAcceptanceGate {
    pub passed: bool,
    pub checks: Vec<DistillationAuditCheck>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DistillationStorageAudit {
    pub valid: bool,
    pub report: Option<StorageReport>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DistillationBackupAudit {
    pub configured: bool,
    pub pending_commits: Option<usize>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DistillationAuditReport {
    pub schema: &'static str,
    pub audit_id: String,
    pub read_only: bool,
    pub plan: DistillationPlan,
    pub checkpoint: DistillationCheckpoint,
    pub delta: DistillationDelta,
    pub gate: DistillationAcceptanceGate,
    pub pending_markdown_exports: usize,
    pub storage_contract: DistillationStorageAudit,
    pub remote_backup: DistillationBackupAudit,
}

#[derive(Serialize)]
struct AuditMaterial<'a> {
    schema: &'static str,
    plan_id: &'a str,
    checkpoint: &'a DistillationCheckpoint,
    delta: &'a DistillationDelta,
    gate: &'a DistillationAcceptanceGate,
    pending_markdown_exports: usize,
    storage_contract_digest: Option<&'a str>,
    storage_contract_error: Option<&'a str>,
    remote_backup: &'a DistillationBackupAudit,
}

pub fn audit(
    vault: &Vault,
    conn: &Connection,
    baseline: Option<&DistillationCheckpoint>,
) -> Result<DistillationAuditReport> {
    let plan = crate::distillation::plan(conn)?;
    let pending_markdown_exports = crate::note_store::pending_count(conn)?;
    let storage_contract = match crate::storage_contract::verify(vault) {
        Ok(report) => DistillationStorageAudit {
            valid: true,
            report: Some(report),
            error: None,
        },
        Err(error) => DistillationStorageAudit {
            valid: false,
            report: None,
            error: Some(error.to_string()),
        },
    };
    let remote_backup = match crate::connect::backup_status(vault) {
        Ok(BackupStatus { remote, pending }) => DistillationBackupAudit {
            configured: remote.is_some(),
            pending_commits: Some(pending),
            error: None,
        },
        Err(error) => DistillationBackupAudit {
            configured: false,
            pending_commits: None,
            error: Some(error.to_string()),
        },
    };
    build_report(
        plan,
        baseline,
        pending_markdown_exports,
        storage_contract,
        remote_backup,
    )
}

fn build_report(
    plan: DistillationPlan,
    baseline: Option<&DistillationCheckpoint>,
    pending_markdown_exports: usize,
    storage_contract: DistillationStorageAudit,
    remote_backup: DistillationBackupAudit,
) -> Result<DistillationAuditReport> {
    let checkpoint = DistillationCheckpoint::from_plan(&plan);
    let delta = build_delta(&plan, baseline)?;
    let gate = build_gate(
        &plan,
        pending_markdown_exports,
        &storage_contract,
        &remote_backup,
    );
    let mut report = DistillationAuditReport {
        schema: AUDIT_SCHEMA,
        audit_id: String::new(),
        read_only: true,
        plan,
        checkpoint,
        delta,
        gate,
        pending_markdown_exports,
        storage_contract,
        remote_backup,
    };
    refresh_audit_id(&mut report)?;
    Ok(report)
}

/// cadenceは監査前後の版を比較してから受け入れる。gate変更後もaudit IDを本文へ照合する。
pub(crate) fn check_artifact_metadata(
    report: &mut DistillationAuditReport,
    stable: bool,
    detail: Option<String>,
) -> Result<()> {
    report.gate.checks.push(DistillationAuditCheck {
        code: DistillationAuditCheckCode::ArtifactMetadataStable,
        passed: stable,
        violations: usize::from(!stable),
        detail,
    });
    report.gate.passed = report.gate.checks.iter().all(|check| check.passed);
    refresh_audit_id(report)
}

fn refresh_audit_id(report: &mut DistillationAuditReport) -> Result<()> {
    let material = AuditMaterial {
        schema: report.schema,
        plan_id: &report.plan.plan_id,
        checkpoint: &report.checkpoint,
        delta: &report.delta,
        gate: &report.gate,
        pending_markdown_exports: report.pending_markdown_exports,
        storage_contract_digest: report
            .storage_contract
            .report
            .as_ref()
            .map(|storage| storage.digest.as_str()),
        storage_contract_error: report.storage_contract.error.as_deref(),
        remote_backup: &report.remote_backup,
    };
    report.audit_id =
        sha256(&serde_json::to_vec(&material).context("蒸留audit materialのserialize")?);
    Ok(())
}

fn build_delta(
    plan: &DistillationPlan,
    baseline: Option<&DistillationCheckpoint>,
) -> Result<DistillationDelta> {
    let Some(baseline) = baseline else {
        let notes = plan
            .entries
            .iter()
            .map(|entry| entry.note.clone())
            .collect::<Vec<_>>();
        return Ok(DistillationDelta {
            mode: DistillationAuditMode::Full,
            baseline_plan_id: None,
            added: notes.clone(),
            changed: Vec::new(),
            removed: Vec::new(),
            moved: Vec::new(),
            unchanged: 0,
            workset: notes,
        });
    };
    validate_checkpoint(baseline)?;

    let previous = baseline
        .entries
        .iter()
        .map(|entry| (checkpoint_identity(entry), entry))
        .collect::<BTreeMap<_, _>>();
    let current = plan
        .entries
        .iter()
        .map(|entry| (plan_identity(entry), entry))
        .collect::<BTreeMap<_, _>>();

    let mut added = BTreeSet::new();
    let mut changed = BTreeSet::new();
    let mut removed = BTreeSet::new();
    let mut moved = Vec::new();
    let mut unchanged = 0;
    let mut triggers = BTreeSet::new();

    for (identity, entry) in &current {
        let Some(before) = previous.get(identity) else {
            added.insert(entry.note.clone());
            triggers.insert(entry.note.clone());
            continue;
        };
        if before.input_hash != entry.input_hash {
            changed.insert(entry.note.clone());
            triggers.insert(entry.note.clone());
        }
        if before.note != entry.note {
            let note_uid = entry
                .note_uid
                .as_deref()
                .expect("path変更のidentityはnote_uid")
                .parse()
                .expect("plannerが検証済みのnote_uid");
            moved.push(DistillationMove {
                note_uid,
                from: before.note.clone(),
                to: entry.note.clone(),
            });
            triggers.insert(before.note.clone());
            triggers.insert(entry.note.clone());
        }
        if before.input_hash == entry.input_hash && before.note == entry.note {
            unchanged += 1;
        }
    }
    for (identity, entry) in &previous {
        if !current.contains_key(identity) {
            removed.insert(entry.note.clone());
            triggers.insert(entry.note.clone());
        }
    }
    moved.sort_by(|left, right| left.note_uid.cmp(&right.note_uid));

    let current_notes = plan
        .entries
        .iter()
        .map(|entry| entry.note.as_str())
        .collect::<BTreeSet<_>>();
    let mut workset = plan
        .entries
        .iter()
        .filter(|entry| {
            triggers.contains(&entry.note)
                || entry.operation != DistillationOperation::Keep
                || entry.risk != DistillationRisk::None
                || entry
                    .depends_on
                    .iter()
                    .any(|dependency| triggers.contains(dependency))
        })
        .map(|entry| entry.note.clone())
        .collect::<BTreeSet<_>>();
    loop {
        let before = workset.len();
        for entry in &plan.entries {
            if workset.contains(&entry.note) {
                workset.extend(
                    entry
                        .depends_on
                        .iter()
                        .filter(|dependency| current_notes.contains(dependency.as_str()))
                        .cloned(),
                );
            } else if entry
                .depends_on
                .iter()
                .any(|dependency| workset.contains(dependency))
            {
                workset.insert(entry.note.clone());
            }
        }
        if workset.len() == before {
            break;
        }
    }

    Ok(DistillationDelta {
        mode: DistillationAuditMode::Incremental,
        baseline_plan_id: Some(baseline.plan_id.clone()),
        added: added.into_iter().collect(),
        changed: changed.into_iter().collect(),
        removed: removed.into_iter().collect(),
        moved,
        unchanged,
        workset: workset.into_iter().collect(),
    })
}

fn build_gate(
    plan: &DistillationPlan,
    pending_markdown_exports: usize,
    storage_contract: &DistillationStorageAudit,
    remote_backup: &DistillationBackupAudit,
) -> DistillationAcceptanceGate {
    let risks = &plan.summary.risks;
    let non_none_risks = risks.low + risks.medium + risks.high + risks.blocked;
    let remote_violations = match (
        remote_backup.error.as_ref(),
        remote_backup.configured,
        remote_backup.pending_commits,
    ) {
        (Some(_), _, _) => 1,
        (None, true, Some(pending)) => pending,
        (None, true, None) => 1,
        (None, false, _) => 0,
    };
    let checks = vec![
        check(
            DistillationAuditCheckCode::NoActionableEntries,
            plan.summary.actionable,
            None,
        ),
        check(
            DistillationAuditCheckCode::NoUnresolvedEntries,
            plan.summary.operations.unresolved,
            None,
        ),
        check(
            DistillationAuditCheckCode::NoNonNoneRiskEntries,
            non_none_risks,
            None,
        ),
        check(
            DistillationAuditCheckCode::NoPendingMarkdownExports,
            pending_markdown_exports,
            None,
        ),
        check(
            DistillationAuditCheckCode::StorageContractValid,
            usize::from(!storage_contract.valid),
            storage_contract.error.clone(),
        ),
        check(
            DistillationAuditCheckCode::RemoteBackupCurrentOrLocalOnly,
            remote_violations,
            remote_backup.error.clone(),
        ),
    ];
    DistillationAcceptanceGate {
        passed: checks.iter().all(|check| check.passed),
        checks,
    }
}

fn check(
    code: DistillationAuditCheckCode,
    violations: usize,
    detail: Option<String>,
) -> DistillationAuditCheck {
    DistillationAuditCheck {
        code,
        passed: violations == 0,
        violations,
        detail,
    }
}

pub(crate) fn validate_checkpoint(checkpoint: &DistillationCheckpoint) -> Result<()> {
    if checkpoint.schema != CHECKPOINT_SCHEMA {
        bail!("蒸留checkpoint schemaが一致しない")
    }
    if checkpoint.plan_schema != PLAN_SCHEMA || checkpoint.planner_profile != PLANNER_PROFILE {
        bail!("蒸留checkpointのplanner契約が現在と一致しない")
    }
    if checkpoint.snapshot_note_count != checkpoint.entries.len() {
        bail!("蒸留checkpointのnote countとentries数が一致しない")
    }
    if !valid_sha256(&checkpoint.checkpoint_id)
        || !valid_sha256(&checkpoint.plan_id)
        || !valid_sha256(&checkpoint.snapshot_digest)
    {
        bail!("蒸留checkpointのcheckpoint/plan/snapshot hashが不正")
    }
    if checkpoint.checkpoint_id != checkpoint_digest(checkpoint) {
        bail!("蒸留checkpointの内容とcheckpoint IDが一致しない")
    }
    let mut notes = BTreeSet::new();
    let mut identities = BTreeSet::new();
    for entry in &checkpoint.entries {
        if entry.note.trim().is_empty() || !valid_sha256(&entry.input_hash) {
            bail!("蒸留checkpoint entryが不正")
        }
        if !notes.insert(entry.note.as_str()) {
            bail!("蒸留checkpointでnote IDが重複している: {}", entry.note)
        }
        let identity = checkpoint_identity(entry);
        if !identities.insert(identity) {
            bail!("蒸留checkpointでstable identityが重複している")
        }
    }
    Ok(())
}

fn checkpoint_identity(entry: &DistillationCheckpointEntry) -> String {
    entry.note_uid.as_ref().map_or_else(
        || format!("note:{}", entry.note),
        |uid| format!("uid:{uid}"),
    )
}

fn plan_identity(entry: &DistillationPlanEntry) -> String {
    entry.note_uid.as_ref().map_or_else(
        || format!("note:{}", entry.note),
        |uid| format!("uid:{uid}"),
    )
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 71
        && value.strip_prefix("sha256:").is_some_and(|hash| {
            hash.bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
}

fn checkpoint_digest(checkpoint: &DistillationCheckpoint) -> String {
    let material = CheckpointMaterial {
        schema: &checkpoint.schema,
        plan_schema: &checkpoint.plan_schema,
        planner_profile: &checkpoint.planner_profile,
        plan_id: &checkpoint.plan_id,
        snapshot_digest: &checkpoint.snapshot_digest,
        snapshot_note_count: checkpoint.snapshot_note_count,
        entries: &checkpoint.entries,
    };
    sha256(&serde_json::to_vec(&material).expect("checkpoint materialをserializeできる"))
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

pub fn render_markdown(report: &DistillationAuditReport) -> String {
    let gate = if report.gate.passed {
        "PASS"
    } else {
        "ATTENTION"
    };
    let mut output = format!(
        "# Distillation audit\n\n- audit: `{}`\n- gate: **{}**\n- plan: `{}`\n- snapshot: `{}`\n- baseline: `{}`\n- workset: {}\n- pending Markdown exports: {}\n\n## Checks\n\n| check | passed | violations |\n|---|:---:|---:|\n",
        report.audit_id,
        gate,
        report.plan.plan_id,
        report.plan.snapshot.digest,
        report.delta.baseline_plan_id.as_deref().unwrap_or("none"),
        report.delta.workset.len(),
        report.pending_markdown_exports,
    );
    for check in &report.gate.checks {
        output.push_str(&format!(
            "| `{:?}` | {} | {} |\n",
            check.code,
            if check.passed { "yes" } else { "no" },
            check.violations,
        ));
    }
    if let Some(storage) = &report.storage_contract.report {
        output.push_str(&format!("\n- {}\n", storage.legacy_inventory.summary()));
    }
    output.push_str("\n## Incremental workset\n\n");
    if report.delta.workset.is_empty() {
        output.push_str("- 変更なし\n");
    } else {
        for note in &report.delta.workset {
            output.push_str(&format!("- `{note}`\n"));
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distillation::{
        DistillationSnapshot, DistillationSummary, OperationCounts, RiskCounts, SNAPSHOT_SCHEMA,
    };
    use crate::index::open_db;

    fn hash(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn entry(note: &str, uid: Option<NoteUid>, input_hash: String) -> DistillationPlanEntry {
        DistillationPlanEntry {
            note: note.into(),
            title: Some(note.into()),
            note_uid: uid.map(|uid| uid.to_string()),
            authority: None,
            input_hash,
            operation: DistillationOperation::Keep,
            risk: DistillationRisk::None,
            signals: Vec::new(),
            reason: "候補なし".into(),
            depends_on: Vec::new(),
        }
    }

    fn plan(entries: Vec<DistillationPlanEntry>) -> DistillationPlan {
        let count = entries.len();
        DistillationPlan {
            schema: PLAN_SCHEMA,
            planner_profile: PLANNER_PROFILE,
            plan_id: hash('a'),
            read_only: true,
            snapshot: DistillationSnapshot {
                schema: SNAPSHOT_SCHEMA,
                digest: hash('b'),
                note_count: count,
            },
            summary: DistillationSummary {
                operations: OperationCounts {
                    keep: count,
                    ..OperationCounts::default()
                },
                risks: RiskCounts {
                    none: count,
                    ..RiskCounts::default()
                },
                actionable: 0,
            },
            entries,
        }
    }

    fn storage_ok() -> DistillationStorageAudit {
        DistillationStorageAudit {
            valid: true,
            report: None,
            error: None,
        }
    }

    fn backup_ok() -> DistillationBackupAudit {
        DistillationBackupAudit {
            configured: false,
            pending_commits: Some(0),
            error: None,
        }
    }

    #[test]
    fn identical_checkpoint_has_empty_incremental_workset_and_stable_audit_id() {
        let current = plan(vec![entry("notes/a", Some(NoteUid::at(1)), hash('c'))]);
        let checkpoint = DistillationCheckpoint::from_plan(&current);
        let first = build_report(
            current.clone(),
            Some(&checkpoint),
            0,
            storage_ok(),
            backup_ok(),
        )
        .unwrap();
        let second =
            build_report(current, Some(&checkpoint), 0, storage_ok(), backup_ok()).unwrap();

        assert!(first.gate.passed);
        assert!(first.delta.workset.is_empty());
        assert_eq!(first.delta.unchanged, 1);
        assert_eq!(first.audit_id, second.audit_id);
    }

    #[test]
    fn stable_uid_distinguishes_move_from_add_and_remove() {
        let uid = NoteUid::at(1);
        let before = plan(vec![entry("notes/old", Some(uid.clone()), hash('c'))]);
        let checkpoint = DistillationCheckpoint::from_plan(&before);
        let after = plan(vec![entry("notes/new", Some(uid), hash('c'))]);
        let report = build_report(after, Some(&checkpoint), 0, storage_ok(), backup_ok()).unwrap();

        assert!(report.delta.added.is_empty());
        assert!(report.delta.removed.is_empty());
        assert_eq!(report.delta.moved.len(), 1);
        assert_eq!(report.delta.workset, vec!["notes/new"]);
    }

    #[test]
    fn incremental_workset_closes_both_sides_of_the_dependency_graph() {
        let mut previous = plan(vec![
            entry("notes/canonical", Some(NoteUid::at(1)), hash('c')),
            entry("notes/record", Some(NoteUid::at(2)), hash('d')),
            entry("notes/other", Some(NoteUid::at(3)), hash('e')),
        ]);
        previous.entries[0].depends_on = vec!["notes/record".into()];
        previous.entries[2].depends_on = vec!["notes/canonical".into()];
        let checkpoint = DistillationCheckpoint::from_plan(&previous);

        let mut current = previous;
        current.entries[0].input_hash = hash('f');
        let report =
            build_report(current, Some(&checkpoint), 0, storage_ok(), backup_ok()).unwrap();

        assert_eq!(report.delta.changed, vec!["notes/canonical"]);
        assert_eq!(
            report.delta.workset,
            vec!["notes/canonical", "notes/other", "notes/record"]
        );
    }

    #[test]
    fn modified_checkpoint_is_rejected_before_delta_classification() {
        let current = plan(vec![entry("notes/a", Some(NoteUid::at(1)), hash('c'))]);
        let mut checkpoint = DistillationCheckpoint::from_plan(&current);
        checkpoint.entries[0].input_hash = hash('d');

        let error =
            build_report(current, Some(&checkpoint), 0, storage_ok(), backup_ok()).unwrap_err();

        assert!(error.to_string().contains("checkpoint IDが一致しない"));
    }

    #[test]
    fn removed_note_stays_in_delta_while_its_current_dependent_enters_the_workset() {
        let dependent_uid = NoteUid::at(2);
        let before = plan(vec![
            entry("notes/removed", Some(NoteUid::at(1)), hash('c')),
            entry("notes/dependent", Some(dependent_uid.clone()), hash('d')),
        ]);
        let checkpoint = DistillationCheckpoint::from_plan(&before);
        let mut dependent = entry("notes/dependent", Some(dependent_uid), hash('d'));
        dependent.depends_on = vec!["notes/removed".into()];

        let report = build_report(
            plan(vec![dependent]),
            Some(&checkpoint),
            0,
            storage_ok(),
            backup_ok(),
        )
        .unwrap();

        assert_eq!(report.delta.removed, vec!["notes/removed"]);
        assert_eq!(report.delta.workset, vec!["notes/dependent"]);
    }

    #[test]
    fn published_example_contains_a_valid_checkpoint() {
        let example: serde_json::Value = serde_json::from_str(include_str!(
            "../../../schemas/examples/distillation-audit.example.json"
        ))
        .unwrap();
        let checkpoint: DistillationCheckpoint =
            serde_json::from_value(example["checkpoint"].clone()).unwrap();

        validate_checkpoint(&checkpoint).unwrap();
        assert_eq!(example["schema"], AUDIT_SCHEMA);
    }

    #[test]
    fn gate_fails_for_semantic_backlog_and_derived_state_drift() {
        let mut current = plan(vec![entry("notes/a", None, hash('c'))]);
        current.summary.actionable = 1;
        current.summary.operations.keep = 0;
        current.summary.operations.revise = 1;
        current.summary.risks.none = 0;
        current.summary.risks.medium = 1;
        current.entries[0].operation = DistillationOperation::Revise;
        current.entries[0].risk = DistillationRisk::Medium;
        let report = build_report(
            current,
            None,
            2,
            DistillationStorageAudit {
                valid: false,
                report: None,
                error: Some("snapshot mismatch".into()),
            },
            DistillationBackupAudit {
                configured: true,
                pending_commits: Some(3),
                error: None,
            },
        )
        .unwrap();

        assert!(!report.gate.passed);
        assert_eq!(
            report
                .gate
                .checks
                .iter()
                .filter(|check| !check.passed)
                .count(),
            5
        );
        assert_eq!(report.delta.workset, vec!["notes/a"]);
    }

    #[test]
    fn audit_does_not_write_to_the_runtime_database() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        let conn = open_db(&vault).unwrap();
        let before = conn.total_changes();

        let report = audit(&vault, &conn, None).unwrap();

        assert_eq!(conn.total_changes(), before);
        assert!(report.read_only);
        assert!(report.gate.passed);
        assert_eq!(report.plan.snapshot.note_count, 0);
    }
}
