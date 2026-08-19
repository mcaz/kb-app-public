//! snapshot固定・read-only・idempotentな蒸留planner。
//!
//! 本文の意味を推測して変更案を確定する層ではない。DBの同一read transactionから
//! 全ノートを固定し、authorityとtyped relationで機械的に証明できる候補だけを列挙する。
//! semantic executorは後段でinput hashとsnapshot digestを再照合してから実行する。

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::authority::{Authority, AuthorityRole, AuthorityStatus, NoteNamespace, RelationKind};
use crate::frontmatter::Note;

pub const PLAN_SCHEMA: &str = "kb-app.distillation-plan/v1";
pub const SNAPSHOT_SCHEMA: &str = "kb-app.distillation-snapshot/v1";
pub const PLANNER_PROFILE: &str = "mechanical-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DistillationOperation {
    Keep,
    Normalize,
    Revise,
    Extract,
    SplitCanonical,
    MergeCandidate,
    SupersedeCandidate,
    Unresolved,
}

impl DistillationOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Normalize => "normalize",
            Self::Revise => "revise",
            Self::Extract => "extract",
            Self::SplitCanonical => "split_canonical",
            Self::MergeCandidate => "merge_candidate",
            Self::SupersedeCandidate => "supersede_candidate",
            Self::Unresolved => "unresolved",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DistillationRisk {
    None,
    Low,
    Medium,
    High,
    Blocked,
}

impl DistillationRisk {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DistillationSignal {
    LegacyAuthorityMissing,
    MissingDescription,
    ProposalMatchesActiveCanonical,
    ProposalNeedsCanonicalDecision,
    RecordWithoutCanonicalLineage,
    RecordUpdatesCanonical,
    RecordContradictsCanonical,
    CanonicalHasPendingUpdate,
    CanonicalHasContradiction,
    HistoricalCanonicalHasActiveSuccessor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DistillationSnapshot {
    pub schema: &'static str,
    pub digest: String,
    pub note_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DistillationPlanEntry {
    pub note: String,
    pub title: Option<String>,
    pub note_uid: Option<String>,
    pub authority: Option<Authority>,
    /// frontmatterを含むDB document全体のSHA-256。executorのexpected revisionに使う。
    pub input_hash: String,
    pub operation: DistillationOperation,
    pub risk: DistillationRisk,
    pub signals: Vec<DistillationSignal>,
    pub reason: String,
    /// 候補理由に直接関係する相手note ID。常に辞書順で、pathはidentityには使わない。
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct OperationCounts {
    pub keep: usize,
    pub normalize: usize,
    pub revise: usize,
    pub extract: usize,
    pub split_canonical: usize,
    pub merge_candidate: usize,
    pub supersede_candidate: usize,
    pub unresolved: usize,
}

impl OperationCounts {
    fn add(&mut self, operation: DistillationOperation) {
        match operation {
            DistillationOperation::Keep => self.keep += 1,
            DistillationOperation::Normalize => self.normalize += 1,
            DistillationOperation::Revise => self.revise += 1,
            DistillationOperation::Extract => self.extract += 1,
            DistillationOperation::SplitCanonical => self.split_canonical += 1,
            DistillationOperation::MergeCandidate => self.merge_candidate += 1,
            DistillationOperation::SupersedeCandidate => self.supersede_candidate += 1,
            DistillationOperation::Unresolved => self.unresolved += 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct RiskCounts {
    pub none: usize,
    pub low: usize,
    pub medium: usize,
    pub high: usize,
    pub blocked: usize,
}

impl RiskCounts {
    fn add(&mut self, risk: DistillationRisk) {
        match risk {
            DistillationRisk::None => self.none += 1,
            DistillationRisk::Low => self.low += 1,
            DistillationRisk::Medium => self.medium += 1,
            DistillationRisk::High => self.high += 1,
            DistillationRisk::Blocked => self.blocked += 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DistillationSummary {
    pub operations: OperationCounts,
    pub risks: RiskCounts,
    pub actionable: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DistillationPlan {
    pub schema: &'static str,
    pub planner_profile: &'static str,
    pub plan_id: String,
    pub read_only: bool,
    pub snapshot: DistillationSnapshot,
    pub summary: DistillationSummary,
    pub entries: Vec<DistillationPlanEntry>,
}

#[derive(Debug)]
struct IndexedNote {
    id: String,
    input_hash: String,
    note: Note,
}

#[derive(Debug, Clone, Copy)]
struct IncidentRelation {
    kind: RelationKind,
    other: usize,
    direction: RelationDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelationDirection {
    Outgoing,
    Incoming,
}

#[derive(Serialize)]
struct SnapshotMaterial<'a> {
    schema: &'static str,
    notes: Vec<(&'a str, &'a str)>,
}

#[derive(Serialize)]
struct PlanMaterial<'a> {
    schema: &'static str,
    planner_profile: &'static str,
    snapshot: &'a DistillationSnapshot,
    entries: &'a [DistillationPlanEntry],
}

/// 同じSQLite read transactionに全SELECTを閉じ込める。commitせずrollbackし、
/// 永続状態だけでなくcare/outbox/remoteにも一切触れない。
pub fn plan(conn: &Connection) -> Result<DistillationPlan> {
    let transaction = conn.unchecked_transaction()?;
    let planned = plan_in_transaction(&transaction)?;
    transaction.rollback()?;
    Ok(planned)
}

/// executorがwrite transactionの内側でTOCTOUなしに同じplanを再計算するための入口。
pub(crate) fn plan_in_transaction(conn: &Connection) -> Result<DistillationPlan> {
    let notes = read_notes(conn)?;
    build_plan(&notes)
}

fn read_notes(conn: &Connection) -> Result<Vec<IndexedNote>> {
    let mut statement = conn.prepare("SELECT id, document FROM notes ORDER BY id")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.map(|row| {
        let (id, document) = row?;
        if document.is_empty() {
            bail!("蒸留snapshotに空のDB documentがある: {id}")
        }
        let note = Note::parse(&document)
            .with_context(|| format!("蒸留snapshotのDB documentをparseできない: {id}"))?;
        Ok(IndexedNote {
            id,
            input_hash: sha256(document.as_bytes()),
            note,
        })
    })
    .collect()
}

fn build_plan(notes: &[IndexedNote]) -> Result<DistillationPlan> {
    let mut uid_to_index = BTreeMap::new();
    let mut active_canonical = BTreeMap::new();
    for (index, note) in notes.iter().enumerate() {
        let front = &note.note.front;
        crate::authority::validate_envelope(
            front.note_uid.as_ref(),
            front.authority.as_ref(),
            &front.relations,
        )?;
        let Some(uid) = &front.note_uid else {
            continue;
        };
        if let Some(existing) = uid_to_index.insert(uid.as_str(), index) {
            bail!(
                "蒸留snapshotでnote_uidが重複している: {} ({}, {})",
                uid,
                notes[existing].id,
                note.id
            )
        }
        let authority = front.authority.as_ref().expect("envelope検証済み");
        if authority.is_active_canonical() {
            let key = (authority.namespace, authority.scope.clone());
            if let Some(existing) = active_canonical.insert(key, index) {
                bail!(
                    "蒸留snapshotでactive canonicalが重複している: {}, {}",
                    notes[existing].id,
                    note.id
                )
            }
        }
    }

    let mut incidents = vec![Vec::new(); notes.len()];
    let mut superseded_targets = BTreeSet::new();
    for (source_index, source) in notes.iter().enumerate() {
        for relation in &source.note.front.relations {
            let target_index = *uid_to_index
                .get(relation.target.as_str())
                .with_context(|| {
                    format!(
                        "蒸留snapshotでtyped relationの参照先がない: {} {} -> {}",
                        source.id,
                        relation.kind.as_str(),
                        relation.target
                    )
                })?;
            incidents[source_index].push(IncidentRelation {
                kind: relation.kind,
                other: target_index,
                direction: RelationDirection::Outgoing,
            });
            incidents[target_index].push(IncidentRelation {
                kind: relation.kind,
                other: source_index,
                direction: RelationDirection::Incoming,
            });
            if relation.kind == RelationKind::Supersedes {
                let source_authority = source
                    .note
                    .front
                    .authority
                    .as_ref()
                    .expect("envelope検証済み");
                let target_authority = notes[target_index]
                    .note
                    .front
                    .authority
                    .as_ref()
                    .expect("UID付きnoteはauthority検証済み");
                if !source_authority.is_active_canonical()
                    || target_authority.role != AuthorityRole::Canonical
                    || target_authority.status != AuthorityStatus::Superseded
                    || source_authority.namespace != target_authority.namespace
                    || source_authority.scope != target_authority.scope
                {
                    bail!(
                        "蒸留snapshotのsupersedesが同じnamespace/scopeのactive canonicalからsuperseded canonicalへ向いていない: {} -> {}",
                        source.id,
                        notes[target_index].id
                    )
                }
                superseded_targets.insert(target_index);
            }
        }
    }
    for (index, note) in notes.iter().enumerate() {
        if note.note.front.authority.as_ref().is_some_and(|authority| {
            authority.role == AuthorityRole::Canonical
                && authority.status == AuthorityStatus::Superseded
        }) && !superseded_targets.contains(&index)
        {
            bail!(
                "蒸留snapshotのsuperseded canonicalに後継のsupersedes relationがない: {}",
                note.id
            )
        }
    }

    let snapshot = snapshot(notes)?;
    let mut entries = Vec::with_capacity(notes.len());
    let mut operations = OperationCounts::default();
    let mut risks = RiskCounts::default();
    for (index, note) in notes.iter().enumerate() {
        let decision = decide(index, notes, &incidents, &active_canonical);
        operations.add(decision.operation);
        risks.add(decision.risk);
        entries.push(DistillationPlanEntry {
            note: note.id.clone(),
            title: note.note.front.title.clone(),
            note_uid: note
                .note
                .front
                .note_uid
                .as_ref()
                .map(|uid| uid.as_str().to_string()),
            authority: note.note.front.authority.clone(),
            input_hash: note.input_hash.clone(),
            operation: decision.operation,
            risk: decision.risk,
            signals: decision.signals,
            reason: decision.reason,
            depends_on: decision.depends_on,
        });
    }
    let actionable = entries
        .iter()
        .filter(|entry| {
            !matches!(
                entry.operation,
                DistillationOperation::Keep | DistillationOperation::Unresolved
            )
        })
        .count();
    let summary = DistillationSummary {
        operations,
        risks,
        actionable,
    };
    let plan_id = sha256(
        &serde_json::to_vec(&PlanMaterial {
            schema: PLAN_SCHEMA,
            planner_profile: PLANNER_PROFILE,
            snapshot: &snapshot,
            entries: &entries,
        })
        .context("蒸留plan materialのserialize")?,
    );
    Ok(DistillationPlan {
        schema: PLAN_SCHEMA,
        planner_profile: PLANNER_PROFILE,
        plan_id,
        read_only: true,
        snapshot,
        summary,
        entries,
    })
}

fn snapshot(notes: &[IndexedNote]) -> Result<DistillationSnapshot> {
    let material = SnapshotMaterial {
        schema: SNAPSHOT_SCHEMA,
        notes: notes
            .iter()
            .map(|note| (note.id.as_str(), note.input_hash.as_str()))
            .collect(),
    };
    Ok(DistillationSnapshot {
        schema: SNAPSHOT_SCHEMA,
        digest: sha256(&serde_json::to_vec(&material).context("蒸留snapshot materialのserialize")?),
        note_count: notes.len(),
    })
}

struct Decision {
    operation: DistillationOperation,
    risk: DistillationRisk,
    signals: Vec<DistillationSignal>,
    reason: String,
    depends_on: Vec<String>,
}

fn decide(
    index: usize,
    notes: &[IndexedNote],
    incidents: &[Vec<IncidentRelation>],
    active_canonical: &BTreeMap<(NoteNamespace, String), usize>,
) -> Decision {
    let note = &notes[index];
    let front = &note.note.front;
    let Some(authority) = &front.authority else {
        return Decision {
            operation: DistillationOperation::Unresolved,
            risk: DistillationRisk::Blocked,
            signals: vec![DistillationSignal::LegacyAuthorityMissing],
            reason: "legacyノートはnamespace・role・scopeを本文確認なしに確定できない".into(),
            depends_on: Vec::new(),
        };
    };
    let missing_description = front
        .description
        .as_deref()
        .is_none_or(|description| description.trim().is_empty());

    let active_match = active_canonical
        .get(&(authority.namespace, authority.scope.clone()))
        .copied()
        .filter(|candidate| *candidate != index);
    if authority.role == AuthorityRole::Proposal {
        if let Some(candidate) = active_match {
            return Decision {
                operation: DistillationOperation::MergeCandidate,
                risk: DistillationRisk::High,
                signals: vec![DistillationSignal::ProposalMatchesActiveCanonical],
                reason: "proposalと同じnamespace/scopeにactive canonicalがあるため、差分のsemantic確認が必要".into(),
                depends_on: vec![notes[candidate].id.clone()],
            };
        }
        return Decision {
            operation: DistillationOperation::Unresolved,
            risk: DistillationRisk::High,
            signals: vec![DistillationSignal::ProposalNeedsCanonicalDecision],
            reason: "proposalをcanonical化・棄却・分割する判断は機械signalだけでは確定できない"
                .into(),
            depends_on: Vec::new(),
        };
    }

    let mut related_canonical = BTreeSet::new();
    let mut has_update = false;
    let mut has_contradiction = false;
    let mut has_lineage = false;
    for incident in &incidents[index] {
        let other = &notes[incident.other];
        let other_is_canonical = other
            .note
            .front
            .authority
            .as_ref()
            .is_some_and(Authority::is_active_canonical);
        if other_is_canonical {
            related_canonical.insert(other.id.clone());
            if matches!(
                incident.kind,
                RelationKind::DerivedFrom
                    | RelationKind::Supports
                    | RelationKind::Updates
                    | RelationKind::Contradicts
            ) {
                has_lineage = true;
            }
        }
        has_update |= incident.kind == RelationKind::Updates
            && match authority.role {
                AuthorityRole::Record => incident.direction == RelationDirection::Outgoing,
                AuthorityRole::Canonical => incident.direction == RelationDirection::Incoming,
                AuthorityRole::Proposal => false,
            };
        has_contradiction |= incident.kind == RelationKind::Contradicts;
    }

    if authority.role == AuthorityRole::Record {
        if has_contradiction && !related_canonical.is_empty() {
            return Decision {
                operation: DistillationOperation::Extract,
                risk: DistillationRisk::High,
                signals: vec![DistillationSignal::RecordContradictsCanonical],
                reason: "recordがactive canonicalとcontradictsで結ばれており、原記録を保持してcanonical差分を抽出する必要がある".into(),
                depends_on: related_canonical.into_iter().collect(),
            };
        }
        if has_update && !related_canonical.is_empty() {
            return Decision {
                operation: DistillationOperation::Extract,
                risk: DistillationRisk::Medium,
                signals: vec![DistillationSignal::RecordUpdatesCanonical],
                reason: "recordがactive canonicalの更新材料として型付けされている".into(),
                depends_on: related_canonical.into_iter().collect(),
            };
        }
        if !has_lineage {
            return Decision {
                operation: DistillationOperation::Extract,
                risk: DistillationRisk::Medium,
                signals: vec![DistillationSignal::RecordWithoutCanonicalLineage],
                reason: "recordにcanonicalへの処理済みlineageがなく、未反映差分の有無を確認する必要がある".into(),
                depends_on: Vec::new(),
            };
        }
        return cosmetic_or_keep(missing_description);
    }

    if authority.role == AuthorityRole::Canonical
        && authority.status == AuthorityStatus::Historical
        && let Some(candidate) = active_match
    {
        return Decision {
            operation: DistillationOperation::SupersedeCandidate,
            risk: DistillationRisk::High,
            signals: vec![DistillationSignal::HistoricalCanonicalHasActiveSuccessor],
            reason: "同じnamespace/scopeにactive canonicalがあり、historicalと後継の関係をsemantic確認できる".into(),
            depends_on: vec![notes[candidate].id.clone()],
        };
    }

    if authority.is_active_canonical() {
        if has_contradiction {
            return Decision {
                operation: DistillationOperation::Revise,
                risk: DistillationRisk::High,
                signals: vec![DistillationSignal::CanonicalHasContradiction],
                reason: "active canonicalにcontradicts relationがあり、現在の結論と反転条件を再確認する必要がある".into(),
                depends_on: incidents[index]
                    .iter()
                    .filter(|incident| incident.kind == RelationKind::Contradicts)
                    .map(|incident| notes[incident.other].id.clone())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
            };
        }
        if has_update {
            return Decision {
                operation: DistillationOperation::Revise,
                risk: DistillationRisk::Medium,
                signals: vec![DistillationSignal::CanonicalHasPendingUpdate],
                reason: "active canonicalにupdates relationがあり、更新材料の反映状況を確認する必要がある".into(),
                depends_on: incidents[index]
                    .iter()
                    .filter(|incident| {
                        incident.kind == RelationKind::Updates
                            && incident.direction == RelationDirection::Incoming
                    })
                    .map(|incident| notes[incident.other].id.clone())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
            };
        }
    }

    // supersededは履歴として固定し、description不足だけで書き換え候補にしない。
    if authority.status == AuthorityStatus::Superseded {
        return Decision {
            operation: DistillationOperation::Keep,
            risk: DistillationRisk::None,
            signals: Vec::new(),
            reason: "後継relationを持つsuperseded canonicalは履歴として保持する".into(),
            depends_on: Vec::new(),
        };
    }
    cosmetic_or_keep(missing_description)
}

fn cosmetic_or_keep(missing_description: bool) -> Decision {
    if missing_description {
        Decision {
            operation: DistillationOperation::Normalize,
            risk: DistillationRisk::Low,
            signals: vec![DistillationSignal::MissingDescription],
            reason: "一文要約がなく、検索結果だけでは用途・現在の結論を判断しにくい".into(),
            depends_on: Vec::new(),
        }
    } else {
        Decision {
            operation: DistillationOperation::Keep,
            risk: DistillationRisk::None,
            signals: Vec::new(),
            reason: "authorityとtyped relationから機械的な蒸留候補は検出されなかった".into(),
            depends_on: Vec::new(),
        }
    }
}

pub fn render_markdown(plan: &DistillationPlan) -> String {
    let counts = &plan.summary.operations;
    let mut output = format!(
        "# Distillation plan\n\n- plan: `{}`\n- snapshot: `{}`\n- notes: {}\n- read only: yes\n\n| operation | count |\n|---|---:|\n| keep | {} |\n| normalize | {} |\n| revise | {} |\n| extract | {} |\n| split-canonical | {} |\n| merge-candidate | {} |\n| supersede-candidate | {} |\n| unresolved | {} |\n\n## Candidates\n\n",
        plan.plan_id,
        plan.snapshot.digest,
        plan.snapshot.note_count,
        counts.keep,
        counts.normalize,
        counts.revise,
        counts.extract,
        counts.split_canonical,
        counts.merge_candidate,
        counts.supersede_candidate,
        counts.unresolved,
    );
    for entry in plan
        .entries
        .iter()
        .filter(|entry| entry.operation != DistillationOperation::Keep)
    {
        output.push_str(&format!(
            "- `{}` — `{}` / `{}`: {}\n",
            entry.note,
            entry.operation.as_str(),
            entry.risk.as_str(),
            entry.reason
        ));
    }
    output
}

pub(crate) fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{NoteRelation, NoteUid};
    use crate::frontmatter::Frontmatter;
    use crate::index::{open_db, upsert};
    use crate::vault::Vault;

    fn authority(
        namespace: NoteNamespace,
        role: AuthorityRole,
        status: AuthorityStatus,
        scope: &str,
    ) -> Authority {
        Authority {
            namespace,
            role,
            status,
            scope: scope.into(),
        }
    }

    fn insert(
        vault: &Vault,
        conn: &Connection,
        id: &str,
        uid: Option<NoteUid>,
        authority: Option<Authority>,
        description: Option<&str>,
        relations: Vec<NoteRelation>,
    ) {
        let mut front = Frontmatter::new_note(id);
        front.origin = Some("agent".into());
        front.tags = vec!["test".into()];
        front.note_uid = uid;
        front.authority = authority;
        front.description = description.map(str::to_string);
        front.relations = relations;
        upsert(
            conn,
            vault,
            id,
            1,
            &Note {
                front,
                body: format!("本文 {id}"),
            },
        )
        .unwrap();
    }

    #[test]
    fn same_snapshot_produces_byte_identical_plan_without_writes() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        let conn = open_db(&vault).unwrap();
        insert(
            &vault,
            &conn,
            "notes/canonical",
            Some(NoteUid::at(1)),
            Some(authority(
                NoteNamespace::Knowledge,
                AuthorityRole::Canonical,
                AuthorityStatus::Active,
                "test/topic",
            )),
            Some("現行正本"),
            Vec::new(),
        );
        let before_changes = conn.total_changes();

        let first = plan(&conn).unwrap();
        let second = plan(&conn).unwrap();

        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&second).unwrap()
        );
        assert_eq!(conn.total_changes(), before_changes);
        assert!(first.read_only);
        assert!(first.plan_id.starts_with("sha256:"));
        assert!(first.snapshot.digest.starts_with("sha256:"));
    }

    #[test]
    fn mechanical_signals_map_to_bounded_candidate_operations() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        let conn = open_db(&vault).unwrap();
        let canonical_uid = NoteUid::at(1);
        insert(
            &vault,
            &conn,
            "notes/canonical",
            Some(canonical_uid.clone()),
            Some(authority(
                NoteNamespace::Knowledge,
                AuthorityRole::Canonical,
                AuthorityStatus::Active,
                "test/topic",
            )),
            Some("正本"),
            Vec::new(),
        );
        insert(
            &vault,
            &conn,
            "notes/proposal",
            Some(NoteUid::at(2)),
            Some(authority(
                NoteNamespace::Knowledge,
                AuthorityRole::Proposal,
                AuthorityStatus::Active,
                "test/topic",
            )),
            Some("候補"),
            Vec::new(),
        );
        insert(
            &vault,
            &conn,
            "notes/record-unlinked",
            Some(NoteUid::at(3)),
            Some(authority(
                NoteNamespace::Records,
                AuthorityRole::Record,
                AuthorityStatus::Active,
                "test/record-unlinked",
            )),
            Some("原記録"),
            Vec::new(),
        );
        insert(
            &vault,
            &conn,
            "notes/record-update",
            Some(NoteUid::at(4)),
            Some(authority(
                NoteNamespace::Records,
                AuthorityRole::Record,
                AuthorityStatus::Active,
                "test/record-update",
            )),
            Some("更新記録"),
            vec![NoteRelation {
                kind: RelationKind::Updates,
                target: canonical_uid,
            }],
        );
        insert(
            &vault,
            &conn,
            "notes/historical",
            Some(NoteUid::at(5)),
            Some(authority(
                NoteNamespace::Knowledge,
                AuthorityRole::Canonical,
                AuthorityStatus::Historical,
                "test/topic",
            )),
            Some("旧正本"),
            Vec::new(),
        );
        insert(
            &vault,
            &conn,
            "notes/normalize",
            Some(NoteUid::at(6)),
            Some(authority(
                NoteNamespace::Decisions,
                AuthorityRole::Canonical,
                AuthorityStatus::Active,
                "test/description",
            )),
            None,
            Vec::new(),
        );
        insert(
            &vault,
            &conn,
            "notes/legacy",
            None,
            None,
            Some("legacy"),
            Vec::new(),
        );

        let output = plan(&conn).unwrap();
        let operations = output
            .entries
            .iter()
            .map(|entry| (entry.note.as_str(), entry.operation))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(operations["notes/canonical"], DistillationOperation::Revise);
        assert_eq!(
            operations["notes/proposal"],
            DistillationOperation::MergeCandidate
        );
        assert_eq!(
            operations["notes/record-unlinked"],
            DistillationOperation::Extract
        );
        assert_eq!(
            operations["notes/record-update"],
            DistillationOperation::Extract
        );
        assert_eq!(
            operations["notes/historical"],
            DistillationOperation::SupersedeCandidate
        );
        assert_eq!(
            operations["notes/normalize"],
            DistillationOperation::Normalize
        );
        assert_eq!(
            operations["notes/legacy"],
            DistillationOperation::Unresolved
        );
        assert_eq!(output.summary.operations.split_canonical, 0);
    }

    #[test]
    fn changing_one_document_changes_its_revision_snapshot_and_plan_id() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        let conn = open_db(&vault).unwrap();
        insert(
            &vault,
            &conn,
            "notes/example",
            Some(NoteUid::at(1)),
            Some(authority(
                NoteNamespace::Knowledge,
                AuthorityRole::Canonical,
                AuthorityStatus::Active,
                "test/revision",
            )),
            Some("要約"),
            Vec::new(),
        );
        let before = plan(&conn).unwrap();
        let mut changed = crate::note_store::read(&conn, "notes/example").unwrap();
        changed.body.push_str("\n変更");
        upsert(&conn, &vault, "notes/example", 2, &changed).unwrap();
        let after = plan(&conn).unwrap();

        assert_ne!(before.snapshot.digest, after.snapshot.digest);
        assert_ne!(before.plan_id, after.plan_id);
        assert_ne!(before.entries[0].input_hash, after.entries[0].input_hash);
    }

    #[test]
    fn updates_direction_is_not_treated_as_a_symmetric_pending_change() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        let conn = open_db(&vault).unwrap();
        let record_uid = NoteUid::at(1);
        insert(
            &vault,
            &conn,
            "notes/record",
            Some(record_uid.clone()),
            Some(authority(
                NoteNamespace::Records,
                AuthorityRole::Record,
                AuthorityStatus::Active,
                "test/direction-record",
            )),
            Some("記録"),
            Vec::new(),
        );
        insert(
            &vault,
            &conn,
            "notes/canonical",
            Some(NoteUid::at(2)),
            Some(authority(
                NoteNamespace::Knowledge,
                AuthorityRole::Canonical,
                AuthorityStatus::Active,
                "test/direction-canonical",
            )),
            Some("正本"),
            vec![NoteRelation {
                kind: RelationKind::Updates,
                target: record_uid,
            }],
        );

        let output = plan(&conn).unwrap();
        assert!(
            output
                .entries
                .iter()
                .all(|entry| entry.operation == DistillationOperation::Keep)
        );
    }

    #[test]
    fn invalid_supersedes_in_the_document_aborts_the_whole_plan() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("vault")).unwrap();
        let conn = open_db(&vault).unwrap();
        let historical_uid = NoteUid::at(1);
        insert(
            &vault,
            &conn,
            "notes/historical",
            Some(historical_uid.clone()),
            Some(authority(
                NoteNamespace::Knowledge,
                AuthorityRole::Canonical,
                AuthorityStatus::Historical,
                "test/invalid-supersedes",
            )),
            Some("履歴"),
            Vec::new(),
        );
        insert(
            &vault,
            &conn,
            "notes/current",
            Some(NoteUid::at(2)),
            Some(authority(
                NoteNamespace::Knowledge,
                AuthorityRole::Canonical,
                AuthorityStatus::Active,
                "test/invalid-supersedes",
            )),
            Some("現行"),
            Vec::new(),
        );
        let mut current = crate::note_store::read(&conn, "notes/current").unwrap();
        current.front.relations.push(NoteRelation {
            kind: RelationKind::Supersedes,
            target: historical_uid,
        });
        conn.execute(
            "UPDATE notes SET document=?1 WHERE id='notes/current'",
            [current.to_file_string().unwrap()],
        )
        .unwrap();

        let error = plan(&conn).unwrap_err();
        assert!(error.to_string().contains("supersedes"));
    }
}
