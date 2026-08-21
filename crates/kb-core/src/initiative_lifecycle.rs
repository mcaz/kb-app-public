//! 完了したinitiativeをsnapshot固定で閉じる専用executor。
//!
//! 意味本文の蒸留とは分離し、AI管理のactive canonical initiativeをhistoricalへ
//! 遷移する1操作だけを扱う。plan / apply / rollbackは全対象を同じDB transactionに
//! 固定し、title・body・description・tags・relations・identityは変更しない。

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::authority::{AuthorityRole, AuthorityStatus, NoteNamespace};
use crate::distillation::DistillationSnapshot;
use crate::frontmatter::{Generated, Note, now_iso};
use crate::note_id::NoteId;
use crate::vault::Vault;

pub const PLAN_SCHEMA: &str = "kb-app.initiative-closure-plan/v1";
pub const PLANNER_PROFILE: &str = "initiative-close-v1";
pub const EXECUTION_REQUEST_SCHEMA: &str = "kb-app.initiative-closure-execution-request/v1";
pub const EXECUTION_RESULT_SCHEMA: &str = "kb-app.initiative-closure-execution-result/v1";
pub const ROLLBACK_RESULT_SCHEMA: &str = "kb-app.initiative-closure-rollback-result/v1";

const MAX_CHANGES: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitiativeClosureChange {
    pub note: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitiativeClosureArguments {
    pub changes: Vec<InitiativeClosureChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InitiativeClosurePlanEntry {
    pub note: String,
    pub note_uid: String,
    pub input_hash: String,
    pub from_status: AuthorityStatus,
    pub to_status: AuthorityStatus,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InitiativeClosurePlan {
    pub schema: &'static str,
    pub planner_profile: &'static str,
    pub plan_id: String,
    pub read_only: bool,
    pub snapshot: DistillationSnapshot,
    pub changes: Vec<InitiativeClosurePlanEntry>,
}

#[derive(Serialize)]
struct PlanIdentity<'a> {
    schema: &'a str,
    planner_profile: &'a str,
    snapshot: &'a DistillationSnapshot,
    changes: &'a [InitiativeClosurePlanEntry],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitiativeClosureExecutionChange {
    pub note: String,
    pub note_uid: String,
    pub input_hash: String,
    pub from_status: AuthorityStatus,
    pub to_status: AuthorityStatus,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitiativeClosureExecutionRequest {
    pub schema: String,
    pub plan_schema: String,
    pub planner_profile: String,
    pub plan_id: String,
    pub snapshot_digest: String,
    pub snapshot_note_count: usize,
    pub changes: Vec<InitiativeClosureExecutionChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutedInitiativeClosure {
    pub note: String,
    pub note_uid: String,
    pub before_hash: String,
    pub after_hash: String,
    pub from_status: AuthorityStatus,
    pub to_status: AuthorityStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InitiativeClosureExecutionReport {
    pub schema: &'static str,
    pub execution_id: String,
    pub plan_id: String,
    pub before_snapshot_digest: String,
    pub after_snapshot_digest: String,
    pub status: &'static str,
    pub changes: Vec<ExecutedInitiativeClosure>,
    pub pending_exports: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown_export_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InitiativeClosureRollbackReport {
    pub schema: &'static str,
    pub rollback_id: String,
    pub execution_id: String,
    pub plan_id: String,
    pub before_snapshot_digest: String,
    pub after_snapshot_digest: String,
    pub status: &'static str,
    pub restored_notes: Vec<String>,
    pub pending_exports: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown_export_error: Option<String>,
}

struct PreparedClosure {
    note: String,
    note_uid: String,
    reason: String,
    before_document: String,
    after_note: Note,
    after_document: String,
}

struct StoredRun {
    plan_id: String,
    before_snapshot_digest: String,
    after_snapshot_digest: String,
    request: InitiativeClosureExecutionRequest,
    before_documents: BTreeMap<String, String>,
    after_documents: BTreeMap<String, String>,
    status: String,
}

pub fn plan(
    conn: &Connection,
    arguments: InitiativeClosureArguments,
) -> Result<InitiativeClosurePlan> {
    let transaction = conn.unchecked_transaction()?;
    let plan = plan_in_transaction(&transaction, arguments)?;
    transaction.commit()?;
    Ok(plan)
}

fn plan_in_transaction(
    conn: &Connection,
    mut arguments: InitiativeClosureArguments,
) -> Result<InitiativeClosurePlan> {
    validate_arguments(&mut arguments)?;
    let mechanical = crate::distillation::plan_in_transaction(conn)?;
    let entries = mechanical
        .entries
        .iter()
        .map(|entry| (entry.note.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut changes = Vec::with_capacity(arguments.changes.len());
    for requested in arguments.changes {
        let entry = entries
            .get(requested.note.as_str())
            .with_context(|| format!("planに対象ノートがない: {}", requested.note))?;
        let document = read_document(conn, &requested.note)?;
        let note = Note::parse(&document).with_context(|| {
            format!("initiative closure対象をparseできない: {}", requested.note)
        })?;
        require_closable(&note, &requested.note)?;
        let note_uid = note
            .front
            .note_uid
            .as_ref()
            .context("note_uidのないlegacyノートはinitiative closure対象外")?
            .to_string();
        changes.push(InitiativeClosurePlanEntry {
            note: requested.note,
            note_uid,
            input_hash: entry.input_hash.clone(),
            from_status: AuthorityStatus::Active,
            to_status: AuthorityStatus::Historical,
            reason: requested.reason,
        });
    }
    let plan_id = crate::distillation::sha256(
        &serde_json::to_vec(&PlanIdentity {
            schema: PLAN_SCHEMA,
            planner_profile: PLANNER_PROFILE,
            snapshot: &mechanical.snapshot,
            changes: &changes,
        })
        .context("initiative closure plan identityのserialize")?,
    );
    Ok(InitiativeClosurePlan {
        schema: PLAN_SCHEMA,
        planner_profile: PLANNER_PROFILE,
        plan_id,
        read_only: true,
        snapshot: mechanical.snapshot,
        changes,
    })
}

pub fn execute(
    vault: &Vault,
    conn: &Connection,
    request: InitiativeClosureExecutionRequest,
    client: &str,
) -> Result<InitiativeClosureExecutionReport> {
    let request = validate_request(request)?;
    let request_json =
        serde_json::to_string(&request).context("initiative closure requestのserialize")?;
    let execution_id = crate::distillation::sha256(request_json.as_bytes());
    let transaction = conn.unchecked_transaction()?;
    let duplicate: Option<String> = transaction
        .query_row(
            "SELECT status FROM distillation_runs WHERE execution_id=?1",
            [&execution_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(status) = duplicate {
        bail!("同じinitiative closure executionは実行済み: {execution_id} ({status})");
    }
    require_clean_outbox(&transaction)?;

    let before_plan = plan_in_transaction(
        &transaction,
        InitiativeClosureArguments {
            changes: request
                .changes
                .iter()
                .map(|change| InitiativeClosureChange {
                    note: change.note.clone(),
                    reason: change.reason.clone(),
                })
                .collect(),
        },
    )?;
    require_plan_match(&request, &before_plan)?;
    let planned = before_plan
        .changes
        .iter()
        .map(|change| (change.note.as_str(), change))
        .collect::<BTreeMap<_, _>>();
    let mut prepared = Vec::with_capacity(request.changes.len());
    let mut before_documents = BTreeMap::new();
    let mut after_documents = BTreeMap::new();
    for change in &request.changes {
        let expected = planned
            .get(change.note.as_str())
            .with_context(|| format!("planに対象ノートがない: {}", change.note))?;
        if change.note_uid != expected.note_uid
            || change.input_hash != expected.input_hash
            || change.from_status != expected.from_status
            || change.to_status != expected.to_status
            || change.reason != expected.reason
        {
            bail!("initiative closure plan entryが一致しない: {}", change.note);
        }
        let before_document = read_document(&transaction, &change.note)?;
        if crate::distillation::sha256(before_document.as_bytes()) != change.input_hash {
            bail!("実行直前のinput hashが一致しない: {}", change.note);
        }
        let before_note = Note::parse(&before_document)
            .with_context(|| format!("initiative closure対象をparseできない: {}", change.note))?;
        require_closable(&before_note, &change.note)?;
        if before_note.front.note_uid.as_ref().map(ToString::to_string)
            != Some(change.note_uid.clone())
        {
            bail!("note_uidがplanと一致しない: {}", change.note);
        }
        let mut after_note = before_note;
        after_note
            .front
            .authority
            .as_mut()
            .expect("require_closableでauthority確認済み")
            .status = AuthorityStatus::Historical;
        after_note.front.generated = Some(Generated {
            by: client.to_string(),
            at: now_iso(),
        });
        let after_document = after_note.to_file_string()?;
        before_documents.insert(change.note.clone(), before_document.clone());
        after_documents.insert(change.note.clone(), after_document.clone());
        prepared.push(PreparedClosure {
            note: change.note.clone(),
            note_uid: change.note_uid.clone(),
            reason: change.reason.clone(),
            before_document,
            after_note,
            after_document,
        });
    }

    for (index, change) in prepared.iter().enumerate() {
        let op_id = format!(
            "{}:initiative-close:{index}",
            execution_id.trim_start_matches("sha256:")
        );
        crate::note_store::queue_put(
            vault,
            &transaction,
            &change.note,
            &change.after_note,
            &op_id,
            &format!(
                "**Initiative closed**: [{}](/{note}.md)をexecution `{execution_id}`でhistorical化。理由: {}",
                change
                    .after_note
                    .front
                    .title
                    .as_deref()
                    .unwrap_or(&change.note),
                change.reason,
                note = change.note,
            ),
            &format!(
                "initiative: close {} ({execution_id}, via {client})",
                change.note
            ),
        )?;
    }
    crate::index::validate_authority_index(&transaction)?;
    let after_plan = crate::distillation::plan_in_transaction(&transaction)?;
    transaction.execute(
        "INSERT INTO distillation_runs(
            execution_id, plan_id, before_snapshot_digest, after_snapshot_digest,
            request_json, before_documents, after_documents, client, applied_at, status
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'applied')",
        rusqlite::params![
            execution_id,
            request.plan_id,
            before_plan.snapshot.digest,
            after_plan.snapshot.digest,
            request_json,
            serde_json::to_string(&before_documents)?,
            serde_json::to_string(&after_documents)?,
            client,
            now_iso(),
        ],
    )?;
    transaction.commit()?;

    let markdown_export_error = vault
        .flush_note_exports(conn)
        .err()
        .map(|error| error.to_string());
    let pending_exports = crate::note_store::pending_count(conn)?;
    let changes = prepared
        .into_iter()
        .map(|change| ExecutedInitiativeClosure {
            note: change.note,
            note_uid: change.note_uid,
            before_hash: crate::distillation::sha256(change.before_document.as_bytes()),
            after_hash: crate::distillation::sha256(change.after_document.as_bytes()),
            from_status: AuthorityStatus::Active,
            to_status: AuthorityStatus::Historical,
        })
        .collect();
    Ok(InitiativeClosureExecutionReport {
        schema: EXECUTION_RESULT_SCHEMA,
        execution_id,
        plan_id: request.plan_id,
        before_snapshot_digest: before_plan.snapshot.digest,
        after_snapshot_digest: after_plan.snapshot.digest,
        status: "applied",
        changes,
        pending_exports,
        markdown_export_error,
    })
}

pub fn rollback(
    vault: &Vault,
    conn: &Connection,
    execution_id: &str,
    client: &str,
) -> Result<InitiativeClosureRollbackReport> {
    if !valid_hash(execution_id) {
        bail!("execution_idはsha256 digestで指定する");
    }
    let transaction = conn.unchecked_transaction()?;
    require_clean_outbox(&transaction)?;
    let stored = read_run(&transaction, execution_id)?;
    if stored.status != "applied" {
        bail!("initiative closure executionはrollback済み: {execution_id}");
    }
    let current = crate::distillation::plan_in_transaction(&transaction)?;
    if current.snapshot.digest != stored.after_snapshot_digest {
        bail!("initiative closure後のsnapshotから変更されているためrollbackできない");
    }
    for (note, expected) in &stored.after_documents {
        if read_document(&transaction, note)? != *expected {
            bail!("initiative closure後に対象が変更されているためrollbackできない: {note}");
        }
    }

    let rollback_id = crate::distillation::sha256(format!("{execution_id}:rollback").as_bytes());
    for (index, (note, document)) in stored.before_documents.iter().enumerate() {
        let restored = Note::parse(document).with_context(|| {
            format!("initiative closure rollback documentをparseできない: {note}")
        })?;
        let op_id = format!(
            "{}:initiative-close-rollback:{index}",
            rollback_id.trim_start_matches("sha256:")
        );
        crate::note_store::queue_put(
            vault,
            &transaction,
            note,
            &restored,
            &op_id,
            &format!(
                "**Initiative closure rollback**: [{}](/{note}.md)を`{execution_id}`以前のactive状態へ復元。",
                restored.front.title.as_deref().unwrap_or(note),
            ),
            &format!("initiative: rollback {note} ({rollback_id}, via {client})"),
        )?;
    }
    crate::index::validate_authority_index(&transaction)?;
    let restored_plan = plan_in_transaction(
        &transaction,
        InitiativeClosureArguments {
            changes: stored
                .request
                .changes
                .iter()
                .map(|change| InitiativeClosureChange {
                    note: change.note.clone(),
                    reason: change.reason.clone(),
                })
                .collect(),
        },
    )?;
    if restored_plan.snapshot.digest != stored.before_snapshot_digest
        || restored_plan.plan_id != stored.plan_id
    {
        bail!("rollback後のsnapshotが元のinitiative closure planへ戻らない");
    }
    transaction.execute(
        "UPDATE distillation_runs
         SET status='rolled_back', rollback_id=?2, rolled_back_at=?3
         WHERE execution_id=?1 AND status='applied'",
        rusqlite::params![execution_id, rollback_id, now_iso()],
    )?;
    transaction.commit()?;

    let markdown_export_error = vault
        .flush_note_exports(conn)
        .err()
        .map(|error| error.to_string());
    let pending_exports = crate::note_store::pending_count(conn)?;
    Ok(InitiativeClosureRollbackReport {
        schema: ROLLBACK_RESULT_SCHEMA,
        rollback_id,
        execution_id: execution_id.to_string(),
        plan_id: stored.plan_id,
        before_snapshot_digest: current.snapshot.digest,
        after_snapshot_digest: restored_plan.snapshot.digest,
        status: "rolled_back",
        restored_notes: stored.before_documents.into_keys().collect(),
        pending_exports,
        markdown_export_error,
    })
}

fn validate_arguments(arguments: &mut InitiativeClosureArguments) -> Result<()> {
    if arguments.changes.is_empty() || arguments.changes.len() > MAX_CHANGES {
        bail!("initiative closure planは1〜{MAX_CHANGES}変更にする");
    }
    arguments
        .changes
        .sort_by(|left, right| left.note.cmp(&right.note));
    let mut notes = BTreeSet::new();
    for change in &arguments.changes {
        NoteId::parse(&change.note)?;
        if !notes.insert(change.note.clone()) {
            bail!(
                "initiative closure plan内で対象が重複している: {}",
                change.note
            );
        }
        validate_reason(&change.reason)?;
    }
    Ok(())
}

fn validate_request(
    mut request: InitiativeClosureExecutionRequest,
) -> Result<InitiativeClosureExecutionRequest> {
    if request.schema != EXECUTION_REQUEST_SCHEMA
        || request.plan_schema != PLAN_SCHEMA
        || request.planner_profile != PLANNER_PROFILE
    {
        bail!("未対応のinitiative closure execution契約");
    }
    if !valid_hash(&request.plan_id) || !valid_hash(&request.snapshot_digest) {
        bail!("plan_idとsnapshot_digestはsha256 digestで指定する");
    }
    if request.changes.is_empty() || request.changes.len() > MAX_CHANGES {
        bail!("initiative closure executionは1〜{MAX_CHANGES}変更にする");
    }
    request
        .changes
        .sort_by(|left, right| left.note.cmp(&right.note));
    let mut notes = BTreeSet::new();
    for change in &request.changes {
        NoteId::parse(&change.note)?;
        if !notes.insert(change.note.clone()) {
            bail!(
                "initiative closure execution内で対象が重複している: {}",
                change.note
            );
        }
        if !crate::artifact::is_ulid(&change.note_uid) {
            bail!("note_uidは26文字のULIDにする: {}", change.note);
        }
        if !valid_hash(&change.input_hash) {
            bail!("input_hashはsha256 digestで指定する: {}", change.note);
        }
        if change.from_status != AuthorityStatus::Active
            || change.to_status != AuthorityStatus::Historical
        {
            bail!("initiative closureはactiveからhistoricalへの遷移だけを受け付ける");
        }
        validate_reason(&change.reason)?;
    }
    Ok(request)
}

fn require_plan_match(
    request: &InitiativeClosureExecutionRequest,
    plan: &InitiativeClosurePlan,
) -> Result<()> {
    if request.plan_schema != plan.schema
        || request.planner_profile != plan.planner_profile
        || request.plan_id != plan.plan_id
        || request.snapshot_digest != plan.snapshot.digest
        || request.snapshot_note_count != plan.snapshot.note_count
        || !plan.read_only
    {
        bail!("initiative closure planが現在のDB snapshotと一致しない。planからやり直す");
    }
    Ok(())
}

fn require_closable(note: &Note, id: &str) -> Result<()> {
    if note.front.origin.as_deref() != Some("agent") {
        bail!("initiative closureはAI管理ノートだけを変更する: {id}");
    }
    let authority = note
        .front
        .authority
        .as_ref()
        .context("legacy authorityのbackfillはinitiative closure対象外")?;
    if authority.namespace != NoteNamespace::Initiatives
        || authority.role != AuthorityRole::Canonical
        || authority.status != AuthorityStatus::Active
    {
        bail!("initiative closureはactive canonical initiativeだけを変更する: {id}");
    }
    if note.front.note_uid.is_none() {
        bail!("note_uidのないlegacyノートはinitiative closure対象外");
    }
    Ok(())
}

fn require_clean_outbox(conn: &Connection) -> Result<()> {
    let pending = crate::note_store::pending_count(conn)?;
    if pending != 0 {
        bail!("未出力のDB更新が{pending}件あるためinitiative closureを開始できない");
    }
    Ok(())
}

fn validate_reason(reason: &str) -> Result<()> {
    if reason.trim() != reason
        || reason.is_empty()
        || reason.chars().count() > 500
        || reason.contains(['\n', '\r'])
    {
        bail!("initiative完了理由は1〜500文字の一行で指定する");
    }
    Ok(())
}

fn valid_hash(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .chars()
            .all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character))
}

fn read_document(conn: &Connection, note: &str) -> Result<String> {
    let id = NoteId::parse(note)?;
    conn.query_row(
        "SELECT document FROM notes WHERE id=?1",
        [id.as_str()],
        |row| row.get(0),
    )
    .with_context(|| format!("ノートが見つからない: {note}"))
}

fn read_run(conn: &Connection, execution_id: &str) -> Result<StoredRun> {
    let row: Option<(String, String, String, String, String, String, String)> = conn
        .query_row(
            "SELECT plan_id, before_snapshot_digest, after_snapshot_digest, request_json,
                    before_documents, after_documents, status
             FROM distillation_runs WHERE execution_id=?1",
            [execution_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?;
    let (plan_id, before_snapshot_digest, after_snapshot_digest, request, before, after, status) =
        row.with_context(|| format!("initiative closure executionが見つからない: {execution_id}"))?;
    Ok(StoredRun {
        plan_id,
        before_snapshot_digest,
        after_snapshot_digest,
        request: serde_json::from_str(&request)
            .context("保存済みinitiative closure requestのparse")?,
        before_documents: serde_json::from_str(&before)
            .context("保存済みinitiative closure rollback元documentのparse")?,
        after_documents: serde_json::from_str(&after)
            .context("保存済みinitiative closure execution後documentのparse")?,
        status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{Authority, AuthorityRole, NoteNamespace};
    use crate::vault::{NoteProposal, NoteUpdate};

    struct Fixture {
        _dir: tempfile::TempDir,
        vault: Vault,
        conn: Connection,
        first: String,
        second: String,
        request: InitiativeClosureExecutionRequest,
        before_documents: BTreeMap<String, String>,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let mut ids = Vec::new();
        for (title, scope) in [
            ("first initiative", "test/first"),
            ("second initiative", "test/second"),
        ] {
            ids.push(
                vault
                    .propose(
                        &conn,
                        NoteProposal {
                            title,
                            body: "完了内容",
                            description: Some("完了したinitiative"),
                            tags: &["test".to_string()],
                            authority: Authority {
                                namespace: NoteNamespace::Initiatives,
                                role: AuthorityRole::Canonical,
                                status: AuthorityStatus::Active,
                                scope: scope.into(),
                            },
                            relations: Vec::new(),
                            allow_new_tags: true,
                            client: "test/client",
                        },
                    )
                    .unwrap(),
            );
        }
        let arguments = InitiativeClosureArguments {
            changes: ids
                .iter()
                .map(|note| InitiativeClosureChange {
                    note: note.clone(),
                    reason: "予定成果を完了し最終監査を通過した".into(),
                })
                .collect(),
        };
        let plan = plan(&conn, arguments).unwrap();
        let request = InitiativeClosureExecutionRequest {
            schema: EXECUTION_REQUEST_SCHEMA.into(),
            plan_schema: plan.schema.into(),
            planner_profile: plan.planner_profile.into(),
            plan_id: plan.plan_id.clone(),
            snapshot_digest: plan.snapshot.digest.clone(),
            snapshot_note_count: plan.snapshot.note_count,
            changes: plan
                .changes
                .iter()
                .map(|change| InitiativeClosureExecutionChange {
                    note: change.note.clone(),
                    note_uid: change.note_uid.clone(),
                    input_hash: change.input_hash.clone(),
                    from_status: change.from_status,
                    to_status: change.to_status,
                    reason: change.reason.clone(),
                })
                .collect(),
        };
        let before_documents = ids
            .iter()
            .map(|note| (note.clone(), read_document(&conn, note).unwrap()))
            .collect();
        Fixture {
            _dir: dir,
            vault,
            conn,
            first: ids[0].clone(),
            second: ids[1].clone(),
            request,
            before_documents,
        }
    }

    #[test]
    fn closes_two_initiatives_atomically_and_rolls_back() {
        let fixture = fixture();
        let original_plan = fixture.request.plan_id.clone();
        let report = execute(
            &fixture.vault,
            &fixture.conn,
            fixture.request.clone(),
            "test/executor",
        )
        .unwrap();
        assert_eq!(report.changes.len(), 2);
        assert_eq!(report.pending_exports, 0);
        for note in [&fixture.first, &fixture.second] {
            let stored = fixture
                .vault
                .read_note_from_db(&fixture.conn, note)
                .unwrap();
            assert_eq!(
                stored.front.authority.unwrap().status,
                AuthorityStatus::Historical
            );
        }
        let duplicate = execute(
            &fixture.vault,
            &fixture.conn,
            fixture.request.clone(),
            "test/executor",
        )
        .unwrap_err();
        assert!(duplicate.to_string().contains("実行済み"));

        let rolled_back = rollback(
            &fixture.vault,
            &fixture.conn,
            &report.execution_id,
            "test/executor",
        )
        .unwrap();
        assert_eq!(rolled_back.plan_id, original_plan);
        for (note, document) in &fixture.before_documents {
            assert_eq!(read_document(&fixture.conn, note).unwrap(), *document);
        }
        assert!(
            rollback(
                &fixture.vault,
                &fixture.conn,
                &report.execution_id,
                "test/executor"
            )
            .unwrap_err()
            .to_string()
            .contains("rollback済み")
        );
    }

    #[test]
    fn rejects_stale_or_tampered_plan_before_writing() {
        let stale_fixture = fixture();
        stale_fixture
            .vault
            .agent_update_note(
                &stale_fixture.conn,
                NoteUpdate {
                    id: &stale_fixture.first,
                    title: None,
                    body: Some("後続変更"),
                    description: None,
                    tags: None,
                    authority: None,
                    relations: None,
                    allow_new_tags: false,
                    client: "test/other",
                },
            )
            .unwrap();
        let second_before = read_document(&stale_fixture.conn, &stale_fixture.second).unwrap();
        assert!(
            execute(
                &stale_fixture.vault,
                &stale_fixture.conn,
                stale_fixture.request,
                "test/executor"
            )
            .unwrap_err()
            .to_string()
            .contains("planからやり直す")
        );
        assert_eq!(
            read_document(&stale_fixture.conn, &stale_fixture.second).unwrap(),
            second_before
        );

        let mut fixture = fixture();
        fixture.request.changes[0].note_uid = "01ARZ3NDEKTSV4RRFFQ69G5FAV".into();
        assert!(
            execute(
                &fixture.vault,
                &fixture.conn,
                fixture.request,
                "test/executor"
            )
            .unwrap_err()
            .to_string()
            .contains("plan entryが一致しない")
        );
    }

    #[test]
    fn second_write_failure_keeps_both_active_and_no_run() {
        let fixture = fixture();
        fixture
            .conn
            .execute_batch(&format!(
                "CREATE TRIGGER fail_second_close BEFORE UPDATE ON notes
                 WHEN NEW.id = '{}'
                 BEGIN SELECT RAISE(FAIL, 'fixture failure'); END;",
                fixture.second
            ))
            .unwrap();
        assert!(
            execute(
                &fixture.vault,
                &fixture.conn,
                fixture.request,
                "test/executor"
            )
            .is_err()
        );
        for (note, document) in &fixture.before_documents {
            assert_eq!(read_document(&fixture.conn, note).unwrap(), *document);
        }
        let runs: i64 = fixture
            .conn
            .query_row("SELECT count(*) FROM distillation_runs", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(runs, 0);
        assert_eq!(crate::note_store::pending_count(&fixture.conn).unwrap(), 0);
    }

    #[test]
    fn planner_rejects_non_initiative_human_and_already_historical() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let knowledge = vault
            .propose(
                &conn,
                NoteProposal {
                    title: "knowledge",
                    body: "本文",
                    description: Some("対象外"),
                    tags: &["test".to_string()],
                    authority: Authority {
                        namespace: NoteNamespace::Knowledge,
                        role: AuthorityRole::Canonical,
                        status: AuthorityStatus::Active,
                        scope: "test/knowledge".into(),
                    },
                    relations: Vec::new(),
                    allow_new_tags: true,
                    client: "test/client",
                },
            )
            .unwrap();
        let error = plan(
            &conn,
            InitiativeClosureArguments {
                changes: vec![InitiativeClosureChange {
                    note: knowledge,
                    reason: "完了".into(),
                }],
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("active canonical initiative"));
    }
}
