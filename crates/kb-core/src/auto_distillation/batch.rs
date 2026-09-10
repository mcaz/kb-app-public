//! 共有する文脈と変更案を一度だけ扱い、全入力版の確認を原子的に確定する。

use super::*;
use rusqlite::OptionalExtension;
use std::collections::{BTreeMap, BTreeSet};

// 全文の入力上限とは別に、言い換えの膨張で反映処理を際限なく大きくしない。
const MAX_DECISION_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SourceDecision {
    note: String,
    outcome: ReviewOutcome,
    reason: String,
    affected_notes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BatchDecision {
    outcome: ReviewOutcome,
    reason: String,
    read_more: Vec<String>,
    search_query: Option<String>,
    changes: Vec<DistillationChange>,
    new_canonical: Option<CanonicalCreation>,
    new_canonical_sources: Vec<String>,
    results: Vec<SourceDecision>,
}

impl BatchDecision {
    pub(super) fn from_single(note: &str, decision: ReviewDecision) -> Self {
        Self {
            new_canonical_sources: if decision.new_canonical.is_some() {
                vec![note.into()]
            } else {
                vec![]
            },
            results: if matches!(
                decision.outcome,
                ReviewOutcome::Applied | ReviewOutcome::NoChange
            ) {
                vec![SourceDecision {
                    note: note.into(),
                    outcome: decision.outcome.clone(),
                    reason: decision.reason.clone(),
                    affected_notes: decision
                        .changes
                        .iter()
                        .map(|change| change.note.clone())
                        .collect(),
                }]
            } else {
                vec![]
            },
            outcome: decision.outcome,
            reason: decision.reason,
            read_more: decision.read_more,
            search_query: decision.search_query,
            changes: decision.changes,
            new_canonical: decision.new_canonical,
        }
    }

    fn validate_final_shape(&self) -> Result<()> {
        validate_reason(&self.reason)?;
        if !self.read_more.is_empty()
            || self.search_query.is_some()
            || self.changes.len() > MAX_CHANGES
        {
            bail!("未完了の全文確認または変更数上限超過");
        }
        let changes = !self.changes.is_empty() || self.new_canonical.is_some();
        if !matches!(
            (&self.outcome, changes),
            (ReviewOutcome::Applied, true) | (ReviewOutcome::NoChange, false)
        ) {
            bail!("確認結果と変更内容が一致しない");
        }
        Ok(())
    }
}

fn schema() -> Value {
    let mut schema = decision_schema();
    schema["required"]
        .as_array_mut()
        .expect("固定schemaのrequired")
        .extend([json!("results"), json!("new_canonical_sources")]);
    schema["properties"]["new_canonical_sources"] =
        json!({"type":"array","items":{"type":"string"}});
    schema["properties"]["results"] = json!({"type":"array","items":{
        "type":"object","additionalProperties":false,
        "required":["note","outcome","reason","affected_notes"],
        "properties":{
            "note":{"type":"string"},
            "outcome":{"type":"string","enum":["applied","no_change"]},
            "reason":{"type":"string"},
            "affected_notes":{"type":"array","items":{"type":"string"}}
        }
    }});
    schema
}

fn batch_prompt(context: &ReviewContext) -> Result<String> {
    // 共通の権限・原文保全規則は単件APIと同じ正本から供給する。
    let common = prompt(context)?;
    let preamble = "今回はsources全件を一括で蒸留するバッチです。sourceは互換用の先頭IDであり、対象を1件に限定しません。各sourcesの全文はdocumentsにあります。共通の正本と文脈を一度読み、同じ既存ノートの変更はchangesへ一件だけまとめてください。\n最終判断applied/no_changeではresultsにsources全件を重複なく列挙し、それぞれのnote・outcome・具体的なreason・affected_notesを返します。affected_notesはその入力の反映先となるchanges内のnote IDだけです。no_changeには変更を関連付けずaffected_notes=[]とします。バッチ全体のoutcomeは一件でも反映があればapplied、全件変更不要ならno_changeです。\nnew_canonicalは最大1件です。その根拠となるsources内のrecordだけをnew_canonical_sourcesに明記し、該当resultsはappliedにします。アプリがそれらの原記録との双方向の根拠関係を結びます。無関係な原記録を混ぜないでください。新設しない場合はnew_canonical_sources=[]です。\nneed_contextとblockedではresults=[]、changes=[]、new_canonical=null、new_canonical_sources=[]とします。未確認の対象を黙って完了扱いにしないでください。一括では判断や反映が成立しない場合はblockedで保留し、小分けの再処理へ渡してください。\n\n";
    let prompt = format!("{preamble}{common}");
    if prompt.len() > MAX_CONTEXT_BYTES {
        return Err(PermanentReviewError::ContextSizeLimit.into());
    }
    Ok(prompt)
}

fn validate(context: &ReviewContext, leases: &[JobLease], decision: &BatchDecision) -> Result<()> {
    if serde_json::to_vec(decision)?.len() > MAX_DECISION_BYTES {
        return Err(PermanentReviewError::ContextSizeLimit.into());
    }
    check_context_size(context)?;
    decision.validate_final_shape()?;
    let expected: BTreeSet<_> = leases.iter().map(|lease| &lease.note).collect();
    if expected.is_empty()
        || expected.len() != leases.len()
        || context.sources.iter().collect::<BTreeSet<_>>() != expected
        || context.sources.len() != leases.len()
        || context.source != leases[0].note
    {
        bail!("蒸留対象と入力集合が一致しない");
    }
    let results: BTreeSet<_> = decision.results.iter().map(|result| &result.note).collect();
    if results != expected || results.len() != decision.results.len() {
        bail!("全ての蒸留対象に重複のない判定が必要");
    }
    let documents: BTreeSet<_> = context
        .documents
        .iter()
        .map(|document| &document.note)
        .collect();
    if documents.len() != context.documents.len() || !expected.is_subset(&documents) {
        bail!("蒸留対象の全文確認が不足または重複している");
    }
    let changes: BTreeSet<_> = decision.changes.iter().map(|change| &change.note).collect();
    if changes.len() != decision.changes.len() {
        bail!("共有ノートへの変更案は一件にまとめる");
    }
    let canonical_sources: BTreeSet<_> = decision.new_canonical_sources.iter().collect();
    if canonical_sources.len() != decision.new_canonical_sources.len()
        || !canonical_sources.is_subset(&expected)
        || decision.new_canonical.is_some() == canonical_sources.is_empty()
    {
        bail!("新正本の根拠は今回の原記録から重複なく指定する");
    }
    let mut attributed = BTreeSet::new();
    let mut applied = false;
    for result in &decision.results {
        validate_reason(&result.reason)?;
        let affected: BTreeSet<_> = result.affected_notes.iter().collect();
        if affected.len() != result.affected_notes.len() || !affected.is_subset(&changes) {
            bail!("ノート別の反映先が共有変更と一致しない");
        }
        let extracts = canonical_sources.contains(&result.note);
        match result.outcome {
            ReviewOutcome::Applied if !affected.is_empty() || extracts => applied = true,
            ReviewOutcome::NoChange
                if affected.is_empty() && !extracts && !changes.contains(&result.note) => {}
            _ => bail!("ノート別の完了判定に対応する反映先がない、または変更不要と矛盾する"),
        }
        attributed.extend(affected);
    }
    if attributed != changes || (decision.outcome == ReviewOutcome::Applied) != applied {
        bail!("共有変更とノート別の完了判定が一致しない");
    }
    Ok(())
}

pub(super) fn complete(
    vault: &Vault,
    conn: &Connection,
    leases: &[JobLease],
    context: &ReviewContext,
    decision: BatchDecision,
    client: &str,
    timing: (i64, Option<&Recorder>, u32),
) -> Result<ReviewReceipt> {
    let (now, recorder, round) = timing;
    validate(context, leases, &decision)?;
    let leader = &leases[0];
    let tx = conn.unchecked_transaction()?;
    // 各占有tokenの監査記録を同じtransactionで残すため、一部だけの既処理は成立しない。
    let mut previous = Vec::new();
    for lease in leases {
        let receipt: Option<(String, String, String)> = tx.query_row(
            "SELECT outcome,after_documents,snapshot_digest FROM distillation_job_runs WHERE run_id=?1 AND note=?2 AND generation=?3",
            rusqlite::params![lease.token,lease.note,lease.generation], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).optional()?;
        if let Some(receipt) = receipt {
            previous.push(receipt);
        }
    }
    if !previous.is_empty() {
        if previous.len() != leases.len()
            || previous
                .iter()
                .any(|row| row.1 != previous[0].1 || row.2 != previous[0].2)
        {
            bail!("一括反映の既処理記録が一致しない");
        }
        let after: BTreeMap<String, String> = serde_json::from_str(&previous[0].1)?;
        let outcome = if previous.iter().any(|row| row.0 == "applied") {
            ReviewOutcome::Applied
        } else {
            ReviewOutcome::NoChange
        };
        tx.rollback()?;
        if let Some(recorder) = recorder {
            recorder.set_completed_notes(leases.len());
        }
        return Ok(ReviewReceipt {
            run_id: leader.token.clone(),
            outcome,
            changed_notes: after.into_keys().collect(),
            pending_exports: crate::note_store::pending_count(conn)?,
        });
    }
    let after = measured(recorder, Stage::Validate, round, || -> Result<_> {
        // 一件目の更新triggerが世代を進める前に、全件の入力版を検証する。
        for lease in leases {
            distillation_jobs::verify_lease_in_tx(&tx, lease, now)?;
        }
        require_snapshot(&tx, &context.snapshot_digest)?;
        let mut sources = BTreeMap::new();
        for lease in leases {
            let source = crate::note_store::read(&tx, &lease.note)?;
            require_reviewable(&source)?;
            sources.insert(lease.note.clone(), source);
        }
        let mut before = BTreeMap::new();
        for document in &context.documents {
            let current: String = tx.query_row(
                "SELECT document FROM notes WHERE id=?1",
                [&document.note],
                |r| r.get(0),
            )?;
            if crate::distillation::sha256(current.as_bytes()) != document.input_hash {
                return Err(ReviewInterruption::SnapshotChanged.into());
            }
            before.insert(document.note.clone(), current);
        }
        let mut after = BTreeMap::new();
        let mut prepared = BTreeMap::<String, Note>::new();
        for change in &decision.changes {
            let supplied = context
                .documents
                .iter()
                .find(|d| d.note == change.note)
                .context("全文確認していないノートは更新できない")?;
            if change.input_hash != supplied.input_hash {
                bail!("更新対象の版が不正");
            }
            let original = crate::note_store::read(&tx, &change.note)?;
            require_reviewable(&original)?;
            validate_reason(&change.reason)?;
            if change.target.body.trim().is_empty()
                || change
                    .target
                    .title
                    .0
                    .as_deref()
                    .is_none_or(|t| t.trim().is_empty())
            {
                bail!("蒸留で本文やタイトルを空にできない");
            }
            let next =
                crate::distillation_executor::prepare_target(&tx, &original, change, client)?;
            prepared.insert(change.note.clone(), next);
        }
        if let Some(creation) = &decision.new_canonical {
            let first_source = &sources[&decision.new_canonical_sources[0]];
            let (id, mut canonical) =
                prepare_canonical(vault, &tx, creation, first_source, client)?;
            let uid = canonical
                .front
                .note_uid
                .clone()
                .context("作成した正本にUIDがない")?;
            canonical.front.relations.clear();
            for source_id in &decision.new_canonical_sources {
                let source = &sources[source_id];
                if source
                    .front
                    .authority
                    .as_ref()
                    .is_none_or(|a| a.role != AuthorityRole::Record)
                {
                    bail!("新正本は原recordからだけ抽出できる");
                }
                canonical.front.relations.push(NoteRelation {
                    kind: RelationKind::DerivedFrom,
                    target: source.front.note_uid.clone().context("原記録にUIDがない")?,
                });
                let mut record = prepared.remove(source_id).unwrap_or_else(|| source.clone());
                record.front.relations.push(NoteRelation {
                    kind: RelationKind::Supports,
                    target: uid.clone(),
                });
                record.front.relations.sort();
                record.front.relations.dedup();
                record.front.generated = Some(Generated {
                    by: client.into(),
                    at: now_iso(),
                });
                prepared.insert(source_id.clone(), record);
            }
            canonical.front.relations.sort();
            canonical.front.relations.dedup();
            write_prepared(
                vault,
                &tx,
                &id,
                &canonical,
                &leader.token,
                &decision.reason,
                client,
            )?;
            after.insert(id, canonical.to_file_string()?);
        }
        for (id, note) in &prepared {
            write_prepared(
                vault,
                &tx,
                id,
                note,
                &leader.token,
                &decision.reason,
                client,
            )?;
            after.insert(id.clone(), note.to_file_string()?);
        }
        crate::index::validate_authority_index(&tx)?;
        let before_json = serde_json::to_string(&before)?;
        let after_json = serde_json::to_string(&after)?;
        for lease in leases {
            let result = decision
                .results
                .iter()
                .find(|r| r.note == lease.note)
                .expect("全対象の判定を検証済み");
            let (outcome, completion) = if result.outcome == ReviewOutcome::Applied {
                ("applied", distillation_jobs::CompletionOutcome::Applied)
            } else {
                ("no_change", distillation_jobs::CompletionOutcome::NoChange)
            };
            tx.execute("INSERT INTO distillation_job_runs(run_id,note,generation,reviewed_at,outcome,reason,snapshot_digest,before_documents,after_documents,client) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                rusqlite::params![lease.token,lease.note,lease.generation,now,outcome,result.reason,context.snapshot_digest,before_json,after_json,client])?;
            if !after.contains_key(&lease.note) {
                distillation_jobs::finish_lease_in_tx(&tx, lease, now, completion, &result.reason)?;
            }
        }
        let outputs = after
            .iter()
            .map(|(id, doc)| (id.clone(), crate::distillation::sha256(doc.as_bytes())))
            .collect::<Vec<_>>();
        distillation_jobs::settle_outputs_in_tx(&tx, &outputs, now, &decision.reason)?;
        Ok(after)
    })?;
    measured(recorder, Stage::Commit, round, || tx.commit())?;
    if let Some(recorder) = recorder {
        recorder.set_completed_notes(leases.len());
    }
    let _export = measured(recorder, Stage::Export, round, || {
        vault.flush_note_exports(conn)
    });
    Ok(ReviewReceipt {
        run_id: leader.token.clone(),
        outcome: decision.outcome,
        changed_notes: after.into_keys().collect(),
        pending_exports: crate::note_store::pending_count(conn)?,
    })
}

pub(super) fn run_next(
    vault: &Vault,
    conn: &Connection,
    settings: &crate::distillation_ai::DistillationAiSettings,
    cancelled: &impl Fn() -> bool,
) -> Result<bool> {
    let schema = schema();
    run_observed(
        vault,
        conn,
        settings,
        cancelled,
        |context, recorder, round| {
            let prompt = batch_prompt(context)?;
            recorder.record_input_bytes(prompt.len());
            Ok(crate::distillation_ai::run_with_metrics(
                settings, &prompt, &schema, cancelled, recorder, round,
            )?)
        },
    )
}

fn fail_all(conn: &Connection, leases: &[JobLease], reason: &str, blocked: bool) -> Result<bool> {
    let tx = conn.unchecked_transaction()?;
    let mut all_current = true;
    for lease in leases {
        all_current &= distillation_jobs::fail(&tx, lease, now_seconds(), reason, blocked)?;
    }
    tx.commit()?;
    Ok(all_current)
}

fn run_observed(
    vault: &Vault,
    conn: &Connection,
    settings: &crate::distillation_ai::DistillationAiSettings,
    cancelled: &impl Fn() -> bool,
    runner: impl FnMut(&ReviewContext, &Recorder, u32) -> Result<Value>,
) -> Result<bool> {
    run_observed_with_claim(vault, conn, settings, cancelled, runner, |now, seconds| {
        distillation_jobs::claim_batch(conn, now, seconds)
    })
}

// 単件互換APIの回帰テストも、本番と同じ探索・中断・計測・確定経路を通す。
// 差し替えるのはclaimだけで、従来fixtureの待機条件と対象数を保つ。
pub(super) fn run_observed_with_claim(
    vault: &Vault,
    conn: &Connection,
    settings: &crate::distillation_ai::DistillationAiSettings,
    cancelled: &impl Fn() -> bool,
    runner: impl FnMut(&ReviewContext, &Recorder, u32) -> Result<Value>,
    claim: impl FnOnce(i64, i64) -> Result<Vec<JobLease>>,
) -> Result<bool> {
    if !settings.enabled || cancelled() {
        return Ok(false);
    }
    let now = now_seconds();
    distillation_jobs::enqueue_due_reviews(conn, now, i64::from(settings.periodic_hours) * 3600)?;
    let leases = claim(now, lease_seconds(settings))?;
    let Some(first) = leases.first() else {
        return Ok(false);
    };
    let recorder = Recorder::start(vault, first, settings);
    recorder.set_batch_size(leases.len());
    match process(vault, conn, &leases, settings, cancelled, runner, &recorder) {
        Ok((outcome, pending_exports)) => recorder.finish(
            outcome,
            if pending_exports > 0 {
                Some(Failure::ExportPending)
            } else if outcome == Outcome::Blocked {
                Some(Failure::NeedsReview)
            } else {
                None
            },
        ),
        Err(error) => {
            let permanent = error.downcast_ref::<PermanentReviewError>();
            // 一括では収まらない・判断できない入力は、次回単件へ分割して義務を残す。
            let split = leases.len() > 1 && (permanent.is_some() || error.is::<BatchNeedsSplit>());
            let reason = if split {
                "batch_split"
            } else if let Some(error) = error.downcast_ref::<crate::distillation_ai::AiRunError>() {
                error.code()
            } else if let Some(error) = permanent {
                error.code()
            } else {
                "review_failed"
            };
            let (mut outcome, failure) = measurement_failure(&error);
            if split {
                outcome = Outcome::RetryWait;
            }
            match fail_all(conn, &leases, reason, permanent.is_some() && !split) {
                Ok(true) => recorder.finish(outcome, Some(failure)),
                Ok(false) => recorder.finish(Outcome::Interrupted, Some(Failure::LeaseChanged)),
                Err(error) => {
                    recorder.finish(Outcome::Interrupted, Some(Failure::ReviewFailed));
                    return Err(error);
                }
            }
        }
    }
    Ok(true)
}

#[derive(Debug)]
struct BatchNeedsSplit;
impl std::fmt::Display for BatchNeedsSplit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("一括判断を小分けにして再確認する")
    }
}
impl std::error::Error for BatchNeedsSplit {}

fn process(
    vault: &Vault,
    conn: &Connection,
    leases: &[JobLease],
    settings: &crate::distillation_ai::DistillationAiSettings,
    cancelled: &impl Fn() -> bool,
    mut runner: impl FnMut(&ReviewContext, &Recorder, u32) -> Result<Value>,
    recorder: &Recorder,
) -> Result<(Outcome, usize)> {
    let mut context = recorder.measure(Stage::Prepare, 0, || {
        prepare_many(conn, leases, now_seconds())
    })?;
    for round in 1..=MAX_AI_CALLS as u32 {
        if cancelled() {
            return Err(ReviewInterruption::Cancelled.into());
        }
        let output = runner(&context, recorder, round)?;
        if serde_json::to_vec(&output)?.len() > MAX_DECISION_BYTES {
            return Err(PermanentReviewError::ContextSizeLimit.into());
        }
        let decision: BatchDecision =
            serde_json::from_value(output).context("AIの一括蒸留結果が指定形式に一致しない")?;
        validate_reason(&decision.reason)?;
        if cancelled() {
            return Err(ReviewInterruption::Cancelled.into());
        }
        match decision.outcome {
            ReviewOutcome::NeedContext | ReviewOutcome::Blocked => {
                if !decision.results.is_empty()
                    || !decision.changes.is_empty()
                    || decision.new_canonical.is_some()
                    || !decision.new_canonical_sources.is_empty()
                {
                    bail!("未完了の確認に変更や完了判定を混在させない");
                }
                if decision.outcome == ReviewOutcome::Blocked {
                    if !decision.read_more.is_empty() || decision.search_query.is_some() {
                        bail!("保留に追加取得を混在させない");
                    }
                    if leases.len() > 1 {
                        return Err(BatchNeedsSplit.into());
                    }
                    if !fail_all(
                        conn,
                        leases,
                        &format!("review_blocked: {}", decision.reason),
                        true,
                    )? {
                        return Err(distillation_jobs::LeaseChanged.into());
                    }
                    return Ok((Outcome::Blocked, 0));
                }
                if context.remaining_explorations == 0 {
                    return Err(match context.final_review_reason {
                        Some(FinalReviewReason::NoNewEvidence) => PermanentReviewError::NoProgress,
                        _ => PermanentReviewError::RoundLimit,
                    }
                    .into());
                }
                let progressed = recorder.measure(Stage::Search, round, || {
                    extend_context_many(
                        conn,
                        leases,
                        &mut context,
                        &decision.read_more,
                        decision.search_query.as_deref(),
                        now_seconds(),
                    )
                })?;
                if !progressed {
                    context.remaining_explorations = 0;
                    context.final_review_reason = Some(FinalReviewReason::NoNewEvidence);
                    check_context_size(&context)?;
                }
            }
            ReviewOutcome::Applied | ReviewOutcome::NoChange => {
                let client = format!(
                    "kb-app-distillation/requested/{:?}/{}/effort={}",
                    settings.provider,
                    settings.model.as_deref().unwrap_or("default"),
                    settings.reasoning_effort.as_deref().unwrap_or("default")
                );
                let receipt = complete(
                    vault,
                    conn,
                    leases,
                    &context,
                    decision,
                    &client,
                    (now_seconds(), Some(recorder), round),
                )?;
                return Ok((
                    if receipt.outcome == ReviewOutcome::Applied {
                        Outcome::Applied
                    } else {
                        Outcome::NoChange
                    },
                    receipt.pending_exports,
                ));
            }
        }
    }
    Err(PermanentReviewError::RoundLimit.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto_distillation::context::read_document;
    use crate::distillation_executor::{DistillationTarget, ExecutableOperation, NullableString};
    use crate::vault::NoteProposal;

    fn add(vault: &Vault, conn: &Connection, title: &str, canonical: bool) -> String {
        vault
            .propose(
                conn,
                NoteProposal {
                    judgment: None,
                    title,
                    body: "条件Aの場合だけ有効。例外Bでは実施しない。原証拠を残す。",
                    description: Some("条件付きの判断記録"),
                    tags: &["test".into()],
                    authority: Authority {
                        namespace: if canonical {
                            NoteNamespace::Knowledge
                        } else {
                            NoteNamespace::Records
                        },
                        role: if canonical {
                            AuthorityRole::Canonical
                        } else {
                            AuthorityRole::Record
                        },
                        status: if canonical {
                            AuthorityStatus::Active
                        } else {
                            AuthorityStatus::Historical
                        },
                        scope: "batch/tests/topic".into(),
                    },
                    relations: vec![],
                    allow_new_tags: true,
                    client: "codex-cli/test",
                    actor: None,
                    revision: None,
                },
            )
            .unwrap()
    }

    fn fixture(count: usize) -> (tempfile::TempDir, Vault, Connection, Vec<String>) {
        let temp = tempfile::tempdir().unwrap();
        let vault = Vault::create(temp.path().join("vault")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let ids = (0..count)
            .map(|i| add(&vault, &conn, &format!("原記録{i}"), false))
            .collect();
        (temp, vault, conn, ids)
    }

    fn claim(conn: &Connection) -> (Vec<JobLease>, ReviewContext) {
        let leases = distillation_jobs::claim_batch(conn, now_seconds(), 600).unwrap();
        assert!(!leases.is_empty());
        let context = prepare_many(conn, &leases, now_seconds()).unwrap();
        (leases, context)
    }

    fn no_change(context: &ReviewContext) -> BatchDecision {
        BatchDecision {
            outcome: ReviewOutcome::NoChange,
            reason: "全件の原証拠と既存正本を確認し、追加反映は不要".into(),
            read_more: vec![],
            search_query: None,
            changes: vec![],
            new_canonical: None,
            new_canonical_sources: vec![],
            results: context
                .sources
                .iter()
                .map(|id| SourceDecision {
                    note: id.clone(),
                    outcome: ReviewOutcome::NoChange,
                    reason: "この記録の条件と例外は既存正本に反映済み".into(),
                    affected_notes: vec![],
                })
                .collect(),
        }
    }

    fn creation(context: &ReviewContext, count: usize) -> BatchDecision {
        let mut decision = no_change(context);
        decision.outcome = ReviewOutcome::Applied;
        decision.new_canonical = Some(CanonicalCreation {
            title: "原記録から抽出した条件付き手順".into(),
            body: "条件Aでのみ実施し、例外Bは除外する。".into(),
            description: "根拠と例外を保持する手順".into(),
            tags: vec!["test".into()],
            namespace: NoteNamespace::Procedures,
            scope: "batch/tests/new-procedure".into(),
        });
        decision.new_canonical_sources = context.sources.iter().take(count).cloned().collect();
        for result in decision.results.iter_mut().take(count) {
            result.outcome = ReviewOutcome::Applied;
            result.reason = "条件付き手順の根拠として抽出する".into();
        }
        decision
    }

    fn settings() -> crate::distillation_ai::DistillationAiSettings {
        crate::distillation_ai::DistillationAiSettings {
            enabled: true,
            provider: Some(crate::distillation_ai::DistillationAiProvider::Codex),
            model: Some("test-model".into()),
            ..Default::default()
        }
    }

    fn run_fake(
        vault: &Vault,
        conn: &Connection,
        cancelled: &impl Fn() -> bool,
        mut runner: impl FnMut(&ReviewContext) -> Result<Value>,
    ) -> Result<bool> {
        run_observed(
            vault,
            conn,
            &settings(),
            cancelled,
            |context, recorder, round| {
                recorder.record_input_bytes(batch_prompt(context)?.len());
                recorder.measure(Stage::AiResponse, round, || runner(context))
            },
        )
    }

    /// 2026-09-07: バッチ化しても、AIが一部の入力を見落としたまま完了してはいけない。
    #[test]
    fn missing_duplicate_unknown_or_unread_sources_cannot_complete_any_member() {
        let (_temp, vault, conn, _) = fixture(3);
        let (leases, context) = claim(&conn);
        for variant in 0..5 {
            let mut decision = no_change(&context);
            let mut context = context.clone();
            match variant {
                0 => {
                    decision.results.pop();
                }
                1 => {
                    decision.results[1] = decision.results[0].clone();
                }
                2 => {
                    decision.results[0].note = "notes/unknown".into();
                }
                3 => {
                    context.documents.remove(0);
                }
                _ => {
                    decision.results[0].outcome = ReviewOutcome::Applied;
                }
            }
            assert!(
                complete(
                    &vault,
                    &conn,
                    &leases,
                    &context,
                    decision,
                    "test",
                    (now_seconds(), None, 1)
                )
                .is_err()
            );
            assert_eq!(distillation_jobs::status(&conn).unwrap().running, 3);
            assert_eq!(
                conn.query_row::<i64, _, _>(
                    "SELECT count(*) FROM distillation_job_runs",
                    [],
                    |r| r.get(0)
                )
                .unwrap(),
                0
            );
            assert_eq!(crate::note_store::pending_count(&conn).unwrap(), 0);
        }
    }

    #[test]
    fn one_ai_response_completes_six_versions_and_records_batch_measurements() {
        let (_temp, vault, conn, _) = fixture(6);
        let mut calls = 0;
        assert!(
            run_fake(&vault, &conn, &|| false, |context| {
                calls += 1;
                assert_eq!(context.sources.len(), 6);
                assert_eq!(context.documents.len(), 6);
                Ok(serde_json::to_value(no_change(context))?)
            })
            .unwrap()
        );
        assert_eq!(calls, 1);
        assert_eq!(distillation_jobs::status(&conn).unwrap().completed, 6);
        let measurements = crate::distillation_metrics::read(&vault, &conn).unwrap();
        let run = &measurements.runs[0];
        assert_eq!(run.batch_size, 6);
        assert_eq!(run.completed_notes, Some(6));
        assert!(run.input_bytes.unwrap() > 0);
        assert_eq!(run.outcome, Some(Outcome::NoChange));
        assert_eq!(
            run.stages
                .iter()
                .filter(|s| s.stage == Stage::AiResponse)
                .count(),
            1
        );
        assert_eq!(
            conn.query_row::<i64, _, _>("SELECT count(*) FROM distillation_job_runs", [], |r| r
                .get(0))
                .unwrap(),
            6
        );
    }

    /// 2026-09-07: 単件指示をバッチ向けに整える際も、同じ語句を含む本文は書き換えない。
    #[test]
    fn batch_prompt_preserves_serialized_source_data_verbatim() {
        let (_temp, _vault, conn, _) = fixture(3);
        let (_leases, mut context) = claim(&conn);
        context.documents[0]
            .body
            .push_str("対象sourceを全文確認し、sourceとの根拠関係を結びます。");
        let input = batch_prompt(&context).unwrap();
        assert!(input.ends_with(&serde_json::to_string(&context).unwrap()));
        assert_eq!(input.matches("\"snapshot_digest\"").count(), 1);
    }

    #[test]
    fn shared_canonical_is_written_once_with_individual_results_and_originals_preserved() {
        let (_temp, vault, conn, ids) = fixture(3);
        let canonical = add(&vault, &conn, "共通する既存正本", true);
        conn.execute(
            "UPDATE distillation_jobs SET state='completed',last_reviewed_at=?1 WHERE note=?2",
            rusqlite::params![now_seconds(), canonical],
        )
        .unwrap();
        let (leases, context) = claim(&conn);
        let doc = context
            .documents
            .iter()
            .find(|d| d.note == canonical)
            .unwrap();
        assert_eq!(
            context
                .documents
                .iter()
                .filter(|d| d.note == canonical)
                .count(),
            1
        );
        let mut decision = no_change(&context);
        decision.outcome = ReviewOutcome::Applied;
        decision.changes = vec![DistillationChange {
            note: canonical.clone(),
            input_hash: doc.input_hash.clone(),
            operation: ExecutableOperation::Revise,
            reason: "3件の原記録から条件と例外を統合".into(),
            target: DistillationTarget {
                title: NullableString(doc.title.clone()),
                body: "条件Aの場合だけ有効。例外Bでは実施しない。原記録3件が同じ条件を支持する。"
                    .into(),
                description: NullableString(doc.description.clone()),
                tags: doc.tags.clone(),
                relations: doc.relations.clone(),
            },
        }];
        for result in &mut decision.results {
            result.outcome = ReviewOutcome::Applied;
            result.affected_notes = vec![canonical.clone()];
        }
        let receipt = complete(
            &vault,
            &conn,
            &leases,
            &context,
            decision.clone(),
            "test",
            (now_seconds(), None, 1),
        )
        .unwrap();
        assert_eq!(receipt.changed_notes, vec![canonical.clone()]);
        assert_eq!(distillation_jobs::status(&conn).unwrap().completed, 4);
        for id in ids {
            let original = context.documents.iter().find(|d| d.note == id).unwrap();
            let current = crate::note_store::read(&conn, &id).unwrap();
            assert_eq!(current.body, original.body);
            assert_eq!(current.front.tags, original.tags);
        }
        let again = complete(
            &vault,
            &conn,
            &leases,
            &context,
            decision,
            "test",
            (now_seconds(), None, 1),
        )
        .unwrap();
        assert_eq!(again.changed_notes, receipt.changed_notes);
        assert_eq!(
            conn.query_row::<i64, _, _>("SELECT count(*) FROM distillation_job_runs", [], |r| r
                .get(0))
                .unwrap(),
            3
        );
        assert!(
            distillation_jobs::claim_batch(&conn, now_seconds(), 600)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn new_canonical_links_only_declared_records_and_other_source_is_explicit_no_change() {
        let (_temp, vault, conn, _) = fixture(3);
        let (leases, context) = claim(&conn);
        let decision = creation(&context, 2);
        let receipt = complete(
            &vault,
            &conn,
            &leases,
            &context,
            decision,
            "test",
            (now_seconds(), None, 1),
        )
        .unwrap();
        assert_eq!(receipt.changed_notes.len(), 3);
        let id = receipt
            .changed_notes
            .iter()
            .find(|id| !context.sources.contains(id))
            .unwrap();
        let canonical = crate::note_store::read(&conn, id).unwrap();
        assert_eq!(canonical.front.relations.len(), 2);
        for (index, source) in context.sources.iter().enumerate() {
            let current = crate::note_store::read(&conn, source).unwrap();
            let original = context
                .documents
                .iter()
                .find(|d| &d.note == source)
                .unwrap();
            assert_eq!(current.body, original.body);
            assert_eq!(current.front.title, original.title);
            assert_eq!(current.front.tags, original.tags);
            if index < 2 {
                assert!(
                    current
                        .front
                        .relations
                        .iter()
                        .any(|r| r.kind == RelationKind::Supports
                            && Some(&r.target) == canonical.front.note_uid.as_ref())
                );
                assert!(
                    canonical
                        .front
                        .relations
                        .iter()
                        .any(|r| r.kind == RelationKind::DerivedFrom
                            && Some(&r.target) == current.front.note_uid.as_ref())
                );
            } else {
                assert_eq!(current.front.relations, original.relations);
                assert_eq!(
                    distillation_jobs::note_status(&conn, source)
                        .unwrap()
                        .unwrap()
                        .state,
                    "completed"
                );
                let outcome: String = conn
                    .query_row(
                        "SELECT outcome FROM distillation_job_runs WHERE note=?1",
                        [source],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(outcome, "no_change");
            }
        }
        assert!(
            distillation_jobs::claim_batch(&conn, now_seconds(), 600)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn late_member_receipt_failure_rolls_back_all_notes_exports_and_completions() {
        let (_temp, vault, conn, _) = fixture(3);
        let (leases, context) = claim(&conn);
        conn.execute_batch("CREATE TRIGGER fail_second BEFORE INSERT ON distillation_job_runs WHEN (SELECT count(*) FROM distillation_job_runs)=1 BEGIN SELECT RAISE(ABORT,'fixture failure'); END;").unwrap();
        assert!(
            complete(
                &vault,
                &conn,
                &leases,
                &context,
                creation(&context, 3),
                "test",
                (now_seconds(), None, 1)
            )
            .is_err()
        );
        assert_eq!(distillation_jobs::status(&conn).unwrap().running, 3);
        assert_eq!(
            conn.query_row::<i64, _, _>("SELECT count(*) FROM notes", [], |r| r.get(0))
                .unwrap(),
            3
        );
        assert_eq!(
            conn.query_row::<i64, _, _>("SELECT count(*) FROM distillation_job_runs", [], |r| r
                .get(0))
                .unwrap(),
            0
        );
        assert_eq!(crate::note_store::pending_count(&conn).unwrap(), 0);
        for document in &context.documents {
            assert_eq!(
                read_document(&conn, &document.note).unwrap().input_hash,
                document.input_hash
            );
        }
    }

    #[test]
    fn changed_member_aborts_whole_batch_and_retains_latest_pending_version() {
        let (_temp, vault, conn, _) = fixture(3);
        let (leases, context) = claim(&conn);
        let changed = &leases[1];
        let mut note = crate::note_store::read(&conn, &changed.note).unwrap();
        note.body.push_str("新たな例外Cを追記。");
        crate::note_store::put(
            &vault,
            &conn,
            &changed.note,
            &note,
            crate::note_store::WriteAttribution::new(
                "fixture update",
                "test",
                &crate::provenance::test_context(),
            ),
        )
        .unwrap();
        let error = complete(
            &vault,
            &conn,
            &leases,
            &context,
            creation(&context, 3),
            "test",
            (now_seconds(), None, 1),
        )
        .unwrap_err();
        assert!(error.is::<distillation_jobs::LeaseChanged>());
        assert!(!fail_all(&conn, &leases, "review_failed", false).unwrap());
        let status = distillation_jobs::status(&conn).unwrap();
        assert_eq!(status.pending, 1);
        assert_eq!(status.retry_wait, 2);
        assert_eq!(status.completed, 0);
        assert_eq!(
            distillation_jobs::note_status(&conn, &changed.note)
                .unwrap()
                .unwrap()
                .generation,
            changed.generation + 1
        );
        assert_eq!(
            crate::note_store::read(&conn, &changed.note).unwrap().body,
            Note::parse(&note.to_file_string().unwrap()).unwrap().body
        );
    }

    #[test]
    fn duplicate_shared_changes_and_false_no_change_are_rejected() {
        let (_temp, vault, conn, _) = fixture(3);
        let (leases, context) = claim(&conn);
        let mut decision = creation(&context, 2);
        decision.results[0].outcome = ReviewOutcome::NoChange;
        assert!(
            complete(
                &vault,
                &conn,
                &leases,
                &context,
                decision,
                "test",
                (now_seconds(), None, 1)
            )
            .is_err()
        );
        let doc = &context.documents[0];
        let change = DistillationChange {
            note: doc.note.clone(),
            input_hash: doc.input_hash.clone(),
            operation: ExecutableOperation::Normalize,
            reason: "要約を補正".into(),
            target: DistillationTarget {
                title: NullableString(doc.title.clone()),
                body: doc.body.clone(),
                description: NullableString(Some("補正後の要約".into())),
                tags: doc.tags.clone(),
                relations: doc.relations.clone(),
            },
        };
        let mut decision = no_change(&context);
        decision.outcome = ReviewOutcome::Applied;
        decision.results[0].outcome = ReviewOutcome::Applied;
        decision.results[0].affected_notes = vec![doc.note.clone()];
        decision.changes = vec![change.clone(), change];
        assert!(
            complete(
                &vault,
                &conn,
                &leases,
                &context,
                decision,
                "test",
                (now_seconds(), None, 1)
            )
            .is_err()
        );
        assert_eq!(distillation_jobs::status(&conn).unwrap().running, 3);
    }

    #[test]
    fn manual_flush_during_ai_preserves_active_batch_and_queues_following_work() {
        let (_temp, vault, conn, _) = fixture(9);
        let mut calls = 0;
        while run_fake(&vault, &conn, &|| false, |context| {
            calls += 1;
            let receipt = distillation_jobs::request_now(
                &conn,
                distillation_jobs::ImmediateDistillationScope::All,
                now_seconds(),
            )?;
            assert_eq!(receipt.jobs.running as usize, context.sources.len());
            Ok(serde_json::to_value(no_change(context))?)
        })
        .unwrap()
        {
            // 2回目に全体見直しを押すと先の完了6件を再登録するため、初回だけを検証する。
            if calls == 1 {
                break;
            }
        }
        assert_eq!(distillation_jobs::status(&conn).unwrap().pending, 3);
        assert!(
            run_fake(&vault, &conn, &|| false, |context| {
                calls += 1;
                assert_eq!(context.sources.len(), 3);
                Ok(serde_json::to_value(no_change(context))?)
            })
            .unwrap()
        );
        assert_eq!(calls, 2);
        assert_eq!(distillation_jobs::status(&conn).unwrap().completed, 9);
    }

    #[test]
    fn blocked_batch_retries_as_singletons_without_losing_any_member() {
        let (_temp, vault, conn, _) = fixture(3);
        assert!(
            run_fake(&vault, &conn, &|| false, |context| {
                let mut decision = no_change(context);
                decision.outcome = ReviewOutcome::Blocked;
                decision.results.clear();
                Ok(serde_json::to_value(decision)?)
            })
            .unwrap()
        );
        assert_eq!(distillation_jobs::status(&conn).unwrap().retry_wait, 3);
        distillation_jobs::request_now(
            &conn,
            distillation_jobs::ImmediateDistillationScope::Unreviewed,
            now_seconds(),
        )
        .unwrap();
        let mut calls = 0;
        while run_fake(&vault, &conn, &|| false, |context| {
            calls += 1;
            assert_eq!(context.sources.len(), 1);
            Ok(serde_json::to_value(no_change(context))?)
        })
        .unwrap()
        {}
        assert_eq!(calls, 3);
        assert_eq!(distillation_jobs::status(&conn).unwrap().completed, 3);
    }

    #[test]
    fn cancellation_after_ai_leaves_entire_batch_unapplied() {
        let (_temp, vault, conn, _) = fixture(3);
        let stopped = std::cell::Cell::new(false);
        assert!(
            run_fake(&vault, &conn, &|| stopped.get(), |context| {
                stopped.set(true);
                Ok(serde_json::to_value(creation(context, 3))?)
            })
            .unwrap()
        );
        assert_eq!(distillation_jobs::status(&conn).unwrap().retry_wait, 3);
        assert_eq!(
            conn.query_row::<i64, _, _>("SELECT count(*) FROM notes", [], |r| r.get(0))
                .unwrap(),
            3
        );
        let run = &crate::distillation_metrics::read(&vault, &conn)
            .unwrap()
            .runs[0];
        assert_eq!(run.completed_notes, Some(0));
        assert_eq!(run.outcome, Some(Outcome::Cancelled));
    }

    #[test]
    fn batched_context_search_gets_one_shared_final_judgment() {
        let (_temp, vault, conn, _) = fixture(3);
        let extra = add(&vault, &conn, "追加で読む補足資料", false);
        conn.execute(
            "UPDATE distillation_jobs SET state='completed',last_reviewed_at=?1 WHERE note=?2",
            rusqlite::params![now_seconds(), extra],
        )
        .unwrap();
        let mut calls = 0;
        assert!(
            run_fake(&vault, &conn, &|| false, |context| {
                calls += 1;
                let mut decision = no_change(context);
                if calls == 1 {
                    decision.outcome = ReviewOutcome::NeedContext;
                    decision.results.clear();
                    decision.read_more = vec![extra.clone()];
                } else {
                    assert!(context.documents.iter().any(|d| d.note == extra));
                    assert_eq!(context.sources.len(), 3);
                }
                Ok(serde_json::to_value(decision)?)
            })
            .unwrap()
        );
        assert_eq!(calls, 2);
        assert_eq!(distillation_jobs::status(&conn).unwrap().completed, 4);
    }
}
