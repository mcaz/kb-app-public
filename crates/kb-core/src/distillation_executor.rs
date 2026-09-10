//! snapshot再照合付きのsemantic distillation executor。
//!
//! v1は既存のauthority付きAIノートに対するnormalize / revise / extractだけを扱う。
//! create・delete・merge・supersede・split・identity変更は専用の後続transactionが揃うまで
//! 能力として公開しない。

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::authority::{AuthorityRole, NoteRelation};
use crate::distillation::{
    DistillationOperation, DistillationPlan, TargetedDistillationArguments,
    TargetedDistillationChange, TargetedDistillationOperation,
};
use crate::frontmatter::{Generated, Note, now_iso};
use crate::note_id::NoteId;
use crate::vault::Vault;

pub const EXECUTION_REQUEST_SCHEMA: &str = "kb-app.distillation-execution-request/v1";
pub const EXECUTION_RESULT_SCHEMA: &str = "kb-app.distillation-execution-result/v1";
pub const ROLLBACK_RESULT_SCHEMA: &str = "kb-app.distillation-rollback-result/v1";

const MAX_CHANGES: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NullableString(pub Option<String>);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutableOperation {
    Normalize,
    Revise,
    Extract,
}

impl ExecutableOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normalize => "normalize",
            Self::Revise => "revise",
            Self::Extract => "extract",
        }
    }

    const fn planned(self) -> DistillationOperation {
        match self {
            Self::Normalize => DistillationOperation::Normalize,
            Self::Revise => DistillationOperation::Revise,
            Self::Extract => DistillationOperation::Extract,
        }
    }

    const fn targeted(self) -> TargetedDistillationOperation {
        match self {
            Self::Normalize => TargetedDistillationOperation::Normalize,
            Self::Revise => TargetedDistillationOperation::Revise,
            Self::Extract => TargetedDistillationOperation::Extract,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistillationTarget {
    pub title: NullableString,
    pub body: String,
    pub description: NullableString,
    pub tags: Vec<String>,
    pub relations: Vec<NoteRelation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistillationChange {
    pub note: String,
    pub input_hash: String,
    pub operation: ExecutableOperation,
    pub reason: String,
    pub target: DistillationTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistillationExecutionRequest {
    pub schema: String,
    pub plan_schema: String,
    pub planner_profile: String,
    pub plan_id: String,
    pub snapshot_digest: String,
    pub snapshot_note_count: usize,
    pub changes: Vec<DistillationChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutedChange {
    pub note: String,
    pub operation: ExecutableOperation,
    pub before_hash: String,
    pub after_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DistillationExecutionReport {
    pub schema: &'static str,
    pub execution_id: String,
    pub plan_id: String,
    pub before_snapshot_digest: String,
    pub after_snapshot_digest: String,
    pub after_plan_id: String,
    pub status: &'static str,
    pub changes: Vec<ExecutedChange>,
    pub pending_exports: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown_export_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DistillationRollbackReport {
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

struct PreparedChange {
    note: String,
    operation: ExecutableOperation,
    reason: String,
    before_document: String,
    after_note: Note,
    after_document: String,
}

struct StoredRun {
    plan_id: String,
    before_snapshot_digest: String,
    after_snapshot_digest: String,
    request: DistillationExecutionRequest,
    before_documents: BTreeMap<String, String>,
    after_documents: BTreeMap<String, String>,
    status: String,
}

pub fn execute(
    vault: &Vault,
    conn: &Connection,
    request: DistillationExecutionRequest,
    client: &str,
) -> Result<DistillationExecutionReport> {
    let request = validate_request(request)?;
    let request_json =
        serde_json::to_string(&request).context("蒸留execution requestのserialize")?;
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
        bail!("同じsemantic executionは実行済み: {execution_id} ({status})");
    }
    require_clean_outbox(&transaction)?;

    let before_plan = plan_for_request(&transaction, &request)?;
    require_plan_match(&request, &before_plan)?;
    let entries = before_plan
        .entries
        .iter()
        .map(|entry| (entry.note.as_str(), entry))
        .collect::<BTreeMap<_, _>>();

    let mut prepared = Vec::with_capacity(request.changes.len());
    let mut before_documents = BTreeMap::new();
    let mut after_documents = BTreeMap::new();
    for change in &request.changes {
        let entry = entries
            .get(change.note.as_str())
            .with_context(|| format!("planに対象ノートがない: {}", change.note))?;
        if entry.input_hash != change.input_hash {
            bail!("input hashがplanと一致しない: {}", change.note);
        }
        if entry.operation != change.operation.planned() {
            bail!(
                "plan operationとexecutionが一致しない: {} (plan={} / request={})",
                change.note,
                entry.operation.as_str(),
                change.operation.as_str()
            );
        }
        let before_document = read_document(&transaction, &change.note)?;
        if crate::distillation::sha256(before_document.as_bytes()) != change.input_hash {
            bail!("実行直前のinput hashが一致しない: {}", change.note);
        }
        let before_note = Note::parse(&before_document)
            .with_context(|| format!("実行対象をparseできない: {}", change.note))?;
        let after_note = prepare_target(&transaction, &before_note, change, client)?;
        let after_document = after_note.to_file_string()?;
        if after_document == before_document {
            bail!("semantic executionに実変更がない: {}", change.note);
        }
        before_documents.insert(change.note.clone(), before_document.clone());
        after_documents.insert(change.note.clone(), after_document.clone());
        prepared.push(PreparedChange {
            note: change.note.clone(),
            operation: change.operation,
            reason: change.reason.clone(),
            before_document,
            after_note,
            after_document,
        });
    }

    // 蒸留はアプリ主導の書込。モデルは`--client` hintの位置に依存しないので、
    // 確定できない間はUnknownのままにする(誤ったモデル名を来歴へ残さない)。
    let actor =
        crate::provenance::WriteActor::app_api(crate::provenance::client_product(client), None);
    for (index, change) in prepared.iter().enumerate() {
        let op_id = format!(
            "{}:apply:{index}",
            execution_id.trim_start_matches("sha256:")
        );
        let revision = crate::provenance::RevisionInput {
            kind: Some(crate::provenance::RevisionKind::Amend),
            summary: Some(change.reason.clone()),
            ..crate::provenance::RevisionInput::default()
        };
        let context = crate::provenance::WriteContext {
            actor: &actor,
            revision: Some(&revision),
            operation: crate::provenance::Operation::Distill,
        };
        crate::note_store::queue_put(
            vault,
            &transaction,
            &change.note,
            &change.after_note,
            &op_id,
            crate::note_store::WriteAttribution::new(
                &format!(
                    "**Distillation {}**: [{}](/{note}.md)をexecution `{execution_id}`で更新。理由: {}",
                    change.operation.as_str(),
                    change
                        .after_note
                        .front
                        .title
                        .as_deref()
                        .unwrap_or(&change.note),
                    change.reason,
                    note = change.note
                ),
                &format!(
                    "distill: {} {} ({execution_id}, via {client})",
                    change.operation.as_str(),
                    change.note
                ),
                &context,
            ),
        )?;
    }
    crate::index::validate_authority_index(&transaction)?;
    let after_plan = crate::distillation::plan_in_transaction(&transaction)?;
    let applied_at = now_iso();
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
            applied_at,
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
        .map(|change| ExecutedChange {
            note: change.note,
            operation: change.operation,
            before_hash: crate::distillation::sha256(change.before_document.as_bytes()),
            after_hash: crate::distillation::sha256(change.after_document.as_bytes()),
        })
        .collect();
    Ok(DistillationExecutionReport {
        schema: EXECUTION_RESULT_SCHEMA,
        execution_id,
        plan_id: request.plan_id,
        before_snapshot_digest: before_plan.snapshot.digest,
        after_snapshot_digest: after_plan.snapshot.digest,
        after_plan_id: after_plan.plan_id,
        status: "applied",
        changes,
        pending_exports,
        markdown_export_error,
    })
}

fn plan_for_request(
    conn: &Connection,
    request: &DistillationExecutionRequest,
) -> Result<DistillationPlan> {
    match request.planner_profile.as_str() {
        crate::distillation::PLANNER_PROFILE => crate::distillation::plan_in_transaction(conn),
        crate::distillation::TARGETED_PLANNER_PROFILE => {
            crate::distillation::plan_targeted_in_transaction(
                conn,
                TargetedDistillationArguments {
                    changes: request
                        .changes
                        .iter()
                        .map(|change| TargetedDistillationChange {
                            note: change.note.clone(),
                            operation: change.operation.targeted(),
                            reason: change.reason.clone(),
                        })
                        .collect(),
                },
            )
        }
        profile => bail!("未対応の蒸留planner profile: {profile}"),
    }
}

pub fn rollback(
    vault: &Vault,
    conn: &Connection,
    execution_id: &str,
    client: &str,
) -> Result<DistillationRollbackReport> {
    if !valid_hash(execution_id) {
        bail!("execution_idはsha256 digestで指定する");
    }
    let transaction = conn.unchecked_transaction()?;
    require_clean_outbox(&transaction)?;
    let stored = read_run(&transaction, execution_id)?;
    if stored.status != "applied" {
        bail!("semantic executionはrollback済み: {execution_id}");
    }

    let current_plan = crate::distillation::plan_in_transaction(&transaction)?;
    if current_plan.snapshot.digest != stored.after_snapshot_digest {
        bail!("execution後のsnapshotから変更されているためrollbackできない");
    }
    for (note, expected) in &stored.after_documents {
        if read_document(&transaction, note)? != *expected {
            bail!("execution後に対象が変更されているためrollbackできない: {note}");
        }
    }

    let operation_by_note = stored
        .request
        .changes
        .iter()
        .map(|change| (change.note.as_str(), change.operation))
        .collect::<BTreeMap<_, _>>();
    let rollback_id = crate::distillation::sha256(format!("{execution_id}:rollback").as_bytes());
    let actor =
        crate::provenance::WriteActor::app_api(crate::provenance::client_product(client), None);
    let revision = crate::provenance::RevisionInput {
        kind: Some(crate::provenance::RevisionKind::Reverse),
        summary: Some(format!("execution {execution_id} のrollback")),
        ..crate::provenance::RevisionInput::default()
    };
    let context = crate::provenance::WriteContext {
        actor: &actor,
        revision: Some(&revision),
        operation: crate::provenance::Operation::Distill,
    };
    for (index, (note, document)) in stored.before_documents.iter().enumerate() {
        let restored = Note::parse(document)
            .with_context(|| format!("rollback documentをparseできない: {note}"))?;
        let operation = operation_by_note
            .get(note.as_str())
            .copied()
            .context("rollback対象のoperationがない")?;
        let op_id = format!(
            "{}:rollback:{index}",
            rollback_id.trim_start_matches("sha256:")
        );
        crate::note_store::queue_put(
            vault,
            &transaction,
            note,
            &restored,
            &op_id,
            crate::note_store::WriteAttribution::new(
                &format!(
                    "**Distillation rollback**: [{}](/{note}.md)の{}を`{execution_id}`以前へ復元。",
                    restored.front.title.as_deref().unwrap_or(note),
                    operation.as_str()
                ),
                &format!("distill: rollback {note} ({rollback_id}, via {client})"),
                &context,
            ),
        )?;
    }
    crate::index::validate_authority_index(&transaction)?;
    let restored_plan = plan_for_request(&transaction, &stored.request)?;
    if restored_plan.snapshot.digest != stored.before_snapshot_digest
        || restored_plan.plan_id != stored.plan_id
    {
        bail!("rollback後のsnapshotが元のplanへ戻らない");
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
    Ok(DistillationRollbackReport {
        schema: ROLLBACK_RESULT_SCHEMA,
        rollback_id,
        execution_id: execution_id.to_string(),
        plan_id: stored.plan_id,
        before_snapshot_digest: current_plan.snapshot.digest,
        after_snapshot_digest: restored_plan.snapshot.digest,
        status: "rolled_back",
        restored_notes: stored.before_documents.into_keys().collect(),
        pending_exports,
        markdown_export_error,
    })
}

fn validate_request(
    mut request: DistillationExecutionRequest,
) -> Result<DistillationExecutionRequest> {
    if request.schema != EXECUTION_REQUEST_SCHEMA {
        bail!("未対応のsemantic execution schema: {}", request.schema);
    }
    if !valid_hash(&request.plan_id) || !valid_hash(&request.snapshot_digest) {
        bail!("plan_idとsnapshot_digestはsha256 digestで指定する");
    }
    if request.changes.is_empty() || request.changes.len() > MAX_CHANGES {
        bail!("semantic executionは1〜{MAX_CHANGES}変更にする");
    }
    request
        .changes
        .sort_by(|left, right| left.note.cmp(&right.note));
    let mut notes = BTreeSet::new();
    for change in &mut request.changes {
        NoteId::parse(&change.note)?;
        if !notes.insert(change.note.clone()) {
            bail!("semantic execution内で対象が重複している: {}", change.note);
        }
        if !valid_hash(&change.input_hash) {
            bail!("input_hashはsha256 digestで指定する: {}", change.note);
        }
        validate_reason(&change.reason)?;
        change.target.relations.sort();
    }
    Ok(request)
}

fn valid_hash(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .chars()
            .all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character))
}

fn validate_reason(reason: &str) -> Result<()> {
    if reason.trim() != reason
        || reason.is_empty()
        || reason.chars().count() > 500
        || reason.contains(['\n', '\r'])
    {
        bail!("semantic変更理由は1〜500文字の一行で指定する");
    }
    Ok(())
}

fn require_clean_outbox(conn: &Connection) -> Result<()> {
    let pending = crate::note_store::pending_count(conn)?;
    if pending != 0 {
        bail!("未出力のDB更新が{pending}件あるためsemantic executionを開始できない");
    }
    Ok(())
}

fn require_plan_match(
    request: &DistillationExecutionRequest,
    plan: &DistillationPlan,
) -> Result<()> {
    if request.plan_schema != plan.schema
        || request.planner_profile != plan.planner_profile
        || request.plan_id != plan.plan_id
        || request.snapshot_digest != plan.snapshot.digest
        || request.snapshot_note_count != plan.snapshot.note_count
        || !plan.read_only
    {
        bail!("蒸留planが現在のDB snapshotと一致しない。plan_distillationからやり直す");
    }
    Ok(())
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

pub(crate) fn prepare_target(
    conn: &Connection,
    before: &Note,
    change: &DistillationChange,
    client: &str,
) -> Result<Note> {
    if before.front.origin.as_deref() != Some("agent") {
        bail!(
            "semantic executorはAI管理ノートだけを変更する: {}",
            change.note
        );
    }
    let authority = before
        .front
        .authority
        .as_ref()
        .context("legacy authorityのbackfillはsemantic executor v1の対象外")?;
    if before.front.note_uid.is_none() {
        bail!("note_uidのないlegacyノートはsemantic executor v1の対象外");
    }
    crate::tags::validate(conn, &change.target.tags, false)?;

    let mut target_relations = change.target.relations.clone();
    target_relations.sort();
    let mut before_relations = before.front.relations.clone();
    before_relations.sort();
    let title_changed = before.front.title != change.target.title.0;
    let body_changed = before.body != change.target.body;
    let description_changed = before.front.description != change.target.description.0;
    let tags_changed = before.front.tags != change.target.tags;
    let relations_changed = before_relations != target_relations;

    match change.operation {
        ExecutableOperation::Normalize => {
            if title_changed || body_changed || tags_changed || relations_changed {
                bail!("normalizeはdescription以外を変更できない: {}", change.note);
            }
            if !description_changed
                || change
                    .target
                    .description
                    .0
                    .as_deref()
                    .is_none_or(|description| description.trim().is_empty())
            {
                bail!("normalizeは空でないdescriptionを追加する: {}", change.note);
            }
        }
        ExecutableOperation::Revise => {
            if !authority.is_active_canonical() {
                bail!("reviseはactive canonicalだけを変更する: {}", change.note);
            }
            if !(title_changed
                || body_changed
                || description_changed
                || tags_changed
                || relations_changed)
            {
                bail!("reviseにsemantic差分がない: {}", change.note);
            }
        }
        ExecutableOperation::Extract => {
            if authority.role != AuthorityRole::Record {
                bail!("extractはrecordだけを処理する: {}", change.note);
            }
            if title_changed || body_changed || tags_changed {
                bail!(
                    "recordのtitle・body・tagsはextractで変更できない: {}",
                    change.note
                );
            }
            if !relations_changed {
                bail!(
                    "extractはrecordのlineage relationを更新する: {}",
                    change.note
                );
            }
        }
    }

    let mut after = before.clone();
    after.front.title = change.target.title.0.clone();
    after.body = change.target.body.clone();
    after.front.description = change.target.description.0.clone();
    after.front.tags = change.target.tags.clone();
    after.front.relations = target_relations;
    after.front.generated = Some(Generated {
        by: client.to_string(),
        at: now_iso(),
    });
    Ok(after)
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
        row.with_context(|| format!("semantic executionが見つからない: {execution_id}"))?;
    Ok(StoredRun {
        plan_id,
        before_snapshot_digest,
        after_snapshot_digest,
        request: serde_json::from_str(&request).context("保存済みexecution requestのparse")?,
        before_documents: serde_json::from_str(&before)
            .context("保存済みrollback元documentのparse")?,
        after_documents: serde_json::from_str(&after)
            .context("保存済みexecution後documentのparse")?,
        status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{Authority, AuthorityStatus, NoteNamespace, RelationKind};
    use crate::vault::{NoteProposal, NoteUpdate};

    struct Fixture {
        _dir: tempfile::TempDir,
        vault: Vault,
        conn: Connection,
        canonical: String,
        record: String,
        request: DistillationExecutionRequest,
        before_documents: BTreeMap<String, String>,
    }

    fn authority(namespace: NoteNamespace, role: AuthorityRole, scope: &str) -> Authority {
        Authority {
            namespace,
            role,
            status: AuthorityStatus::Active,
            scope: scope.into(),
        }
    }

    fn target(note: &Note) -> DistillationTarget {
        DistillationTarget {
            title: NullableString(note.front.title.clone()),
            body: note.body.clone(),
            description: NullableString(note.front.description.clone()),
            tags: note.front.tags.clone(),
            relations: note.front.relations.clone(),
        }
    }

    fn entry<'a>(
        plan: &'a DistillationPlan,
        note: &str,
    ) -> &'a crate::distillation::DistillationPlanEntry {
        plan.entries
            .iter()
            .find(|entry| entry.note == note)
            .unwrap()
    }

    fn document(conn: &Connection, note: &str) -> String {
        read_document(conn, note).unwrap()
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let tags = vec!["test".to_string()];
        let canonical = vault
            .propose(
                &conn,
                NoteProposal {
                    judgment: None,
                    title: "a canonical",
                    body: "旧結論",
                    description: Some("現行の結論"),
                    tags: &tags,
                    authority: authority(
                        NoteNamespace::Knowledge,
                        AuthorityRole::Canonical,
                        "test/topic",
                    ),
                    relations: Vec::new(),
                    allow_new_tags: true,
                    client: "test/client",
                    actor: None,
                    revision: None,
                },
            )
            .unwrap();
        let canonical_note = vault.read_note_from_db(&conn, &canonical).unwrap();
        let target_uid = canonical_note.front.note_uid.clone().unwrap();
        let record = vault
            .propose(
                &conn,
                NoteProposal {
                    judgment: None,
                    title: "z record",
                    body: "原証拠",
                    description: Some("後続の観測"),
                    tags: &tags,
                    authority: authority(
                        NoteNamespace::Records,
                        AuthorityRole::Record,
                        "test/record",
                    ),
                    relations: vec![NoteRelation {
                        kind: RelationKind::Updates,
                        target: target_uid.clone(),
                    }],
                    allow_new_tags: true,
                    client: "test/client",
                    actor: None,
                    revision: None,
                },
            )
            .unwrap();

        let plan = crate::distillation::plan(&conn).unwrap();
        assert_eq!(
            entry(&plan, &canonical).operation,
            DistillationOperation::Revise
        );
        assert_eq!(
            entry(&plan, &record).operation,
            DistillationOperation::Extract
        );
        let mut canonical_target = target(&vault.read_note_from_db(&conn, &canonical).unwrap());
        canonical_target.body = "新しい証拠を反映した結論".into();
        let mut record_target = target(&vault.read_note_from_db(&conn, &record).unwrap());
        record_target.relations = vec![NoteRelation {
            kind: RelationKind::Supports,
            target: target_uid,
        }];
        let request = DistillationExecutionRequest {
            schema: EXECUTION_REQUEST_SCHEMA.into(),
            plan_schema: plan.schema.into(),
            planner_profile: plan.planner_profile.into(),
            plan_id: plan.plan_id.clone(),
            snapshot_digest: plan.snapshot.digest.clone(),
            snapshot_note_count: plan.snapshot.note_count,
            // 並び順はidentityに含めず、executorがnote ID順へ正規化する。
            changes: vec![
                DistillationChange {
                    note: record.clone(),
                    input_hash: entry(&plan, &record).input_hash.clone(),
                    operation: ExecutableOperation::Extract,
                    reason: "原証拠を保持したまま更新材料を反映済みlineageへ移す".into(),
                    target: record_target,
                },
                DistillationChange {
                    note: canonical.clone(),
                    input_hash: entry(&plan, &canonical).input_hash.clone(),
                    operation: ExecutableOperation::Revise,
                    reason: "後続recordの観測を現行結論へ反映する".into(),
                    target: canonical_target,
                },
            ],
        };
        let before_documents = [canonical.clone(), record.clone()]
            .into_iter()
            .map(|note| (note.clone(), document(&conn, &note)))
            .collect();
        Fixture {
            _dir: dir,
            vault,
            conn,
            canonical,
            record,
            request,
            before_documents,
        }
    }

    #[test]
    fn applies_one_snapshot_atomically_and_rolls_back_to_the_same_plan() {
        let fixture = fixture();
        let original_plan = crate::distillation::plan(&fixture.conn).unwrap();
        let report = execute(
            &fixture.vault,
            &fixture.conn,
            fixture.request.clone(),
            "test/executor",
        )
        .unwrap();
        assert_eq!(report.status, "applied");
        assert_eq!(report.pending_exports, 0);
        assert_eq!(report.changes.len(), 2);
        assert_eq!(
            fixture
                .vault
                .read_note_from_db(&fixture.conn, &fixture.canonical)
                .unwrap()
                .body,
            "新しい証拠を反映した結論\n"
        );
        let record = fixture
            .vault
            .read_note_from_db(&fixture.conn, &fixture.record)
            .unwrap();
        assert_eq!(record.body, "原証拠\n");
        assert_eq!(record.front.relations[0].kind, RelationKind::Supports);

        let duplicate = execute(
            &fixture.vault,
            &fixture.conn,
            fixture.request.clone(),
            "test/executor",
        )
        .unwrap_err();
        assert!(duplicate.to_string().contains("実行済み"));

        let rollback_report = rollback(
            &fixture.vault,
            &fixture.conn,
            &report.execution_id,
            "test/executor",
        )
        .unwrap();
        assert_eq!(rollback_report.status, "rolled_back");
        assert_eq!(rollback_report.pending_exports, 0);
        for (note, expected) in &fixture.before_documents {
            assert_eq!(&document(&fixture.conn, note), expected);
        }
        let restored_plan = crate::distillation::plan(&fixture.conn).unwrap();
        assert_eq!(restored_plan.plan_id, original_plan.plan_id);
        assert_eq!(restored_plan.snapshot.digest, original_plan.snapshot.digest);
        let repeated = rollback(
            &fixture.vault,
            &fixture.conn,
            &report.execution_id,
            "test/executor",
        )
        .unwrap_err();
        assert!(repeated.to_string().contains("rollback済み"));
    }

    #[test]
    fn second_write_failure_rolls_back_every_note_and_audit_row() {
        let fixture = fixture();
        fixture
            .conn
            .execute_batch(&format!(
                "CREATE TRIGGER fail_record BEFORE UPDATE ON notes
                 WHEN NEW.id = '{}'
                 BEGIN SELECT RAISE(FAIL, 'fixture failure'); END;",
                fixture.record
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
        for (note, expected) in &fixture.before_documents {
            assert_eq!(&document(&fixture.conn, note), expected);
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

    /// 2026-08-20、rollbackも複数noteを順番に戻すため、2件目の失敗で1件目だけ
    /// 復元済みになるとatomic waveの回復境界を破る。
    #[test]
    fn rollback_write_failure_keeps_every_note_and_run_applied() {
        let fixture = fixture();
        let report = execute(
            &fixture.vault,
            &fixture.conn,
            fixture.request.clone(),
            "test/executor",
        )
        .unwrap();
        let after_documents = [fixture.canonical.clone(), fixture.record.clone()]
            .into_iter()
            .map(|note| (note.clone(), document(&fixture.conn, &note)))
            .collect::<BTreeMap<_, _>>();
        fixture
            .conn
            .execute_batch(&format!(
                "CREATE TRIGGER fail_record_rollback BEFORE UPDATE ON notes
                 WHEN NEW.id = '{}'
                 BEGIN SELECT RAISE(FAIL, 'fixture rollback failure'); END;",
                fixture.record
            ))
            .unwrap();

        assert!(
            rollback(
                &fixture.vault,
                &fixture.conn,
                &report.execution_id,
                "test/executor"
            )
            .is_err()
        );
        for (note, expected) in &after_documents {
            assert_eq!(&document(&fixture.conn, note), expected);
        }
        let status: String = fixture
            .conn
            .query_row(
                "SELECT status FROM distillation_runs WHERE execution_id=?1",
                [&report.execution_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "applied");
        assert_eq!(crate::note_store::pending_count(&fixture.conn).unwrap(), 0);
    }

    /// 2026-08-20、対象外noteを含む後続変更を見逃すと、その変更が参照したsemantic
    /// waveだけを巻き戻してDB全体の整合を壊せるため、全snapshotを照合する。
    #[test]
    fn rollback_rejects_any_later_snapshot_change() {
        let fixture = fixture();
        let report = execute(
            &fixture.vault,
            &fixture.conn,
            fixture.request.clone(),
            "test/executor",
        )
        .unwrap();
        let canonical_after = document(&fixture.conn, &fixture.canonical);
        fixture
            .vault
            .propose(
                &fixture.conn,
                NoteProposal {
                    judgment: None,
                    title: "later unrelated note",
                    body: "後続変更",
                    description: Some("rollback境界を進める対象外note"),
                    tags: &["test".to_string()],
                    authority: authority(
                        NoteNamespace::Knowledge,
                        AuthorityRole::Canonical,
                        "test/later",
                    ),
                    relations: Vec::new(),
                    allow_new_tags: false,
                    client: "test/other",
                    actor: None,
                    revision: None,
                },
            )
            .unwrap();

        let error = rollback(
            &fixture.vault,
            &fixture.conn,
            &report.execution_id,
            "test/executor",
        )
        .unwrap_err();
        assert!(error.to_string().contains("snapshotから変更されている"));
        assert_eq!(document(&fixture.conn, &fixture.canonical), canonical_after);
    }

    #[test]
    fn stale_plan_rejects_the_whole_wave_before_writing() {
        let fixture = fixture();
        fixture
            .vault
            .agent_update_note(
                &fixture.conn,
                NoteUpdate {
                    judgment: None,
                    id: &fixture.canonical,
                    title: None,
                    body: Some("別経路の更新"),
                    description: None,
                    tags: None,
                    authority: None,
                    relations: None,
                    allow_new_tags: false,
                    client: "test/other",
                    actor: None,
                    revision: None,
                },
            )
            .unwrap();
        let record_before = document(&fixture.conn, &fixture.record);

        let error = execute(
            &fixture.vault,
            &fixture.conn,
            fixture.request,
            "test/executor",
        )
        .unwrap_err();
        assert!(error.to_string().contains("plan_distillationからやり直す"));
        assert_eq!(document(&fixture.conn, &fixture.record), record_before);
        let runs: i64 = fixture
            .conn
            .query_row("SELECT count(*) FROM distillation_runs", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(runs, 0);
    }

    #[test]
    fn extract_cannot_rewrite_original_record_content() {
        let fixture = fixture();
        let mut request = fixture.request;
        request
            .changes
            .iter_mut()
            .find(|change| change.note == fixture.record)
            .unwrap()
            .target
            .body = "改変された証拠".into();

        let error = execute(&fixture.vault, &fixture.conn, request, "test/executor").unwrap_err();
        assert!(error.to_string().contains("recordのtitle・body・tags"));
        for (note, expected) in &fixture.before_documents {
            assert_eq!(&document(&fixture.conn, note), expected);
        }
    }

    #[test]
    fn normalize_can_only_add_a_non_empty_description() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let tags = vec!["test".to_string()];
        let note_id = vault
            .propose(
                &conn,
                NoteProposal {
                    judgment: None,
                    title: "normalize target",
                    body: "本文",
                    description: None,
                    tags: &tags,
                    authority: authority(
                        NoteNamespace::Knowledge,
                        AuthorityRole::Canonical,
                        "test/normalize",
                    ),
                    relations: Vec::new(),
                    allow_new_tags: true,
                    client: "test/client",
                    actor: None,
                    revision: None,
                },
            )
            .unwrap();
        let plan = crate::distillation::plan(&conn).unwrap();
        let planned = entry(&plan, &note_id);
        assert_eq!(planned.operation, DistillationOperation::Normalize);
        let mut normalized = target(&vault.read_note_from_db(&conn, &note_id).unwrap());
        normalized.description = NullableString(Some("検索結果で用途が分かる要約".into()));
        let request = DistillationExecutionRequest {
            schema: EXECUTION_REQUEST_SCHEMA.into(),
            plan_schema: plan.schema.into(),
            planner_profile: plan.planner_profile.into(),
            plan_id: plan.plan_id.clone(),
            snapshot_digest: plan.snapshot.digest.clone(),
            snapshot_note_count: plan.snapshot.note_count,
            changes: vec![DistillationChange {
                note: note_id.clone(),
                input_hash: planned.input_hash.clone(),
                operation: ExecutableOperation::Normalize,
                reason: "description欠落を補う".into(),
                target: normalized,
            }],
        };
        execute(&vault, &conn, request, "test/executor").unwrap();
        assert_eq!(
            vault
                .read_note_from_db(&conn, &note_id)
                .unwrap()
                .front
                .description
                .as_deref(),
            Some("検索結果で用途が分かる要約")
        );
    }

    #[test]
    fn targeted_keep_to_revise_is_plan_bound_atomic_and_rollbackable() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let note_id = vault
            .propose(
                &conn,
                NoteProposal {
                    judgment: None,
                    title: "targeted canonical",
                    body: "欠落参照に依存する本文",
                    description: Some("機械plannerではkeepになる正本"),
                    tags: &["test".to_string()],
                    authority: authority(
                        NoteNamespace::Knowledge,
                        AuthorityRole::Canonical,
                        "test/targeted-executor",
                    ),
                    relations: Vec::new(),
                    allow_new_tags: true,
                    client: "test/client",
                    actor: None,
                    revision: None,
                },
            )
            .unwrap();
        let mechanical = crate::distillation::plan(&conn).unwrap();
        assert_eq!(
            entry(&mechanical, &note_id).operation,
            DistillationOperation::Keep
        );
        let reason = "全文監査で欠落参照への意味依存を検出した";
        let targeted = crate::distillation::plan_targeted(
            &conn,
            TargetedDistillationArguments {
                changes: vec![TargetedDistillationChange {
                    note: note_id.clone(),
                    operation: TargetedDistillationOperation::Revise,
                    reason: reason.into(),
                }],
            },
        )
        .unwrap();
        let planned = entry(&targeted, &note_id);
        let mut revised = target(&vault.read_note_from_db(&conn, &note_id).unwrap());
        revised.body = "根拠を本文だけで理解できる自己完結記述".into();
        let request = DistillationExecutionRequest {
            schema: EXECUTION_REQUEST_SCHEMA.into(),
            plan_schema: targeted.schema.into(),
            planner_profile: targeted.planner_profile.into(),
            plan_id: targeted.plan_id.clone(),
            snapshot_digest: targeted.snapshot.digest.clone(),
            snapshot_note_count: targeted.snapshot.note_count,
            changes: vec![DistillationChange {
                note: note_id.clone(),
                input_hash: planned.input_hash.clone(),
                operation: ExecutableOperation::Revise,
                reason: reason.into(),
                target: revised,
            }],
        };

        let report = execute(&vault, &conn, request.clone(), "test/executor").unwrap();
        assert_eq!(
            vault.read_note_from_db(&conn, &note_id).unwrap().body,
            "根拠を本文だけで理解できる自己完結記述\n"
        );
        rollback(&vault, &conn, &report.execution_id, "test/executor").unwrap();
        assert_eq!(
            vault.read_note_from_db(&conn, &note_id).unwrap().body,
            "欠落参照に依存する本文\n"
        );

        let mut tampered = request;
        tampered.changes[0].reason = "planに無い別理由".into();
        let error = execute(&vault, &conn, tampered, "test/executor").unwrap_err();
        assert!(error.to_string().contains("plan_distillationからやり直す"));
    }

    #[test]
    fn published_request_example_matches_the_core_request_contract() {
        let request: DistillationExecutionRequest = serde_json::from_str(include_str!(
            "../../../schemas/examples/distillation-execution.example.json"
        ))
        .unwrap();
        let validated = validate_request(request).unwrap();
        assert_eq!(validated.schema, EXECUTION_REQUEST_SCHEMA);
        assert_eq!(validated.changes.len(), 1);
    }
}
