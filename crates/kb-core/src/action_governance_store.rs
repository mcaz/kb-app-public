//! Action Governanceの実行予約とreceiptを、runtime SQLiteへatomicに永続化する。
//!
//! 外部adapterは必ず [`reserve`] の成功後に副作用を実行し、[`finalize`] で結果を
//! 確定する。`pending` は再実行許可ではなく、crash後に外部状態を照会するreconcile対象。

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::action_governance::{
    ActionPolicy, ActionRequest, AuthorizationSource, DecisionKind, ExecutionOutcome,
    PolicyDecision, SignedCapabilityLease, TrustedCapabilityIssuers, evaluate, request_hash,
};

pub const PERSISTED_RECEIPT_SCHEMA: &str = "kb.action-persisted-receipt.v2";

type StoredReceiptRow = (
    String,
    String,
    String,
    Option<String>,
    String,
    i64,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    Pending,
    Succeeded,
    Failed,
}

impl ReceiptStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            _ => bail!("unknown action receipt status: {value}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedExecutionReceipt {
    pub schema: String,
    pub receipt_id: String,
    pub workspace: String,
    pub request: ActionRequest,
    pub decision: PolicyDecision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_id: Option<String>,
    pub status: ReceiptStatus,
    pub reserved_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_started_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_reference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compensation_deadline: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compensated_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReservationAttempt {
    pub decision: PolicyDecision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt: Option<PersistedExecutionReceipt>,
}

/// adapterが副作用を実行するために必要な、gateだけが生成できるpermit。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionPermit {
    receipt_id: String,
    workspace: String,
    request_hash: String,
    idempotency_key: String,
    target: String,
}

impl ExecutionPermit {
    pub fn receipt_id(&self) -> &str {
        &self.receipt_id
    }

    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    pub fn request_hash(&self) -> &str {
        &self.request_hash
    }

    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }

    pub fn target(&self) -> &str {
        &self.target
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterExecution {
    pub outcome: ExecutionOutcome,
    pub external_reference: Option<String>,
    pub external_target: Option<String>,
    pub completed_at: i64,
    pub compensation_deadline: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconciliationState {
    Succeeded,
    Failed,
    StillUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciliationQuery {
    receipt_id: String,
    workspace: String,
    request_hash: String,
    idempotency_key: String,
    target: String,
}

impl ReconciliationQuery {
    pub fn receipt_id(&self) -> &str {
        &self.receipt_id
    }

    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    pub fn request_hash(&self) -> &str {
        &self.request_hash
    }

    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }

    pub fn target(&self) -> &str {
        &self.target
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterReconciliation {
    pub state: ReconciliationState,
    pub external_reference: Option<String>,
    pub external_target: Option<String>,
    pub observed_at: i64,
    pub compensation_deadline: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompensationPermit {
    receipt_id: String,
    workspace: String,
    request_hash: String,
    external_reference: String,
    external_target: String,
    compensation_deadline: i64,
}

impl CompensationPermit {
    pub fn receipt_id(&self) -> &str {
        &self.receipt_id
    }

    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    pub fn request_hash(&self) -> &str {
        &self.request_hash
    }

    pub fn external_reference(&self) -> &str {
        &self.external_reference
    }

    pub fn external_target(&self) -> &str {
        &self.external_target
    }

    pub const fn compensation_deadline(&self) -> i64 {
        self.compensation_deadline
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterCompensation {
    pub external_reference: String,
    pub external_target: String,
    pub completed_at: i64,
}

/// 実在adapterはこの境界を実装し、低水準副作用をpermitなしで公開しない。
pub trait GovernedActionAdapter {
    fn execute(&mut self, permit: &ExecutionPermit) -> AdapterExecution;
}

pub trait ReconciliableActionAdapter: GovernedActionAdapter {
    fn reconcile(&mut self, query: &ReconciliationQuery) -> AdapterReconciliation;
}

pub trait GovernedReversibleActionAdapter: ReconciliableActionAdapter {
    fn compensate(&mut self, permit: &CompensationPermit) -> Result<AdapterCompensation>;
}

impl<F> GovernedActionAdapter for F
where
    F: FnMut(&ExecutionPermit) -> AdapterExecution,
{
    fn execute(&mut self, permit: &ExecutionPermit) -> AdapterExecution {
        self(permit)
    }
}

pub struct ActionGovernanceGate<'a> {
    conn: &'a mut Connection,
    workspace: &'a str,
    trusted_issuers: &'a TrustedCapabilityIssuers,
}

impl<'a> ActionGovernanceGate<'a> {
    pub fn new(
        conn: &'a mut Connection,
        workspace: &'a str,
        trusted_issuers: &'a TrustedCapabilityIssuers,
    ) -> Self {
        Self {
            conn,
            workspace,
            trusted_issuers,
        }
    }

    /// reserveとexecution-start記録を終えたpermitだけをadapterへ渡し、結果を確定する。
    pub fn run<A: GovernedActionAdapter>(
        &mut self,
        request: &ActionRequest,
        policy: &ActionPolicy,
        signed_capability: Option<&SignedCapabilityLease>,
        reserved_at: i64,
        adapter: &mut A,
    ) -> Result<ReservationAttempt> {
        let ReservationAttempt { decision, receipt } = reserve(
            self.conn,
            self.workspace,
            request,
            policy,
            signed_capability,
            self.trusted_issuers,
            reserved_at,
        )?;
        let Some(receipt) = receipt else {
            return Ok(ReservationAttempt {
                decision,
                receipt: None,
            });
        };
        let executing = start_execution(self.conn, &receipt.receipt_id, reserved_at)?;
        let permit = ExecutionPermit {
            receipt_id: executing.receipt_id.clone(),
            workspace: executing.workspace.clone(),
            request_hash: executing.decision.request_hash.clone(),
            idempotency_key: executing.request.idempotency_key.clone(),
            target: executing.request.target.clone(),
        };
        let result = adapter.execute(&permit);
        let finalized = finalize(
            self.conn,
            &executing.receipt_id,
            result.outcome,
            result.external_reference.as_deref(),
            result.external_target.as_deref(),
            result.completed_at,
            result.compensation_deadline,
        )?;
        Ok(ReservationAttempt {
            decision,
            receipt: Some(finalized),
        })
    }

    /// crash後のpendingを外部照会へ収束する。unknownは再実行せずpendingのまま返す。
    pub fn reconcile<A: ReconciliableActionAdapter>(
        &mut self,
        receipt_id: &str,
        observed_at: i64,
        adapter: &mut A,
    ) -> Result<PersistedExecutionReceipt> {
        let current = get(self.conn, receipt_id)?.context("action receiptがない")?;
        ensure_receipt_workspace(&current, self.workspace)?;
        if current.status != ReceiptStatus::Pending {
            bail!("action receiptは確定済み: {receipt_id}");
        }
        if current.execution_started_at.is_none() {
            return fail_before_execution(self.conn, receipt_id, observed_at);
        }
        let query = ReconciliationQuery {
            receipt_id: current.receipt_id.clone(),
            workspace: current.workspace.clone(),
            request_hash: current.decision.request_hash.clone(),
            idempotency_key: current.request.idempotency_key.clone(),
            target: current.request.target.clone(),
        };
        let result = adapter.reconcile(&query);
        if result.observed_at != observed_at {
            bail!("adapter observed_at mismatch");
        }
        match result.state {
            ReconciliationState::StillUnknown => {
                if result.external_reference.is_some()
                    || result.external_target.is_some()
                    || result.compensation_deadline.is_some()
                {
                    bail!("unknown reconciliation must not assert external state");
                }
                get(self.conn, receipt_id)?.context("pending receiptが消えた")
            }
            ReconciliationState::Succeeded => finalize(
                self.conn,
                receipt_id,
                ExecutionOutcome::Succeeded,
                result.external_reference.as_deref(),
                result.external_target.as_deref(),
                result.observed_at,
                result.compensation_deadline,
            ),
            ReconciliationState::Failed => finalize(
                self.conn,
                receipt_id,
                ExecutionOutcome::Failed,
                result.external_reference.as_deref(),
                result.external_target.as_deref(),
                result.observed_at,
                result.compensation_deadline,
            ),
        }
    }

    /// 成功receiptへ固定された外部objectだけを、期限内に一度だけ補償する。
    pub fn compensate<A: GovernedReversibleActionAdapter>(
        &mut self,
        receipt_id: &str,
        now: i64,
        adapter: &mut A,
    ) -> Result<PersistedExecutionReceipt> {
        let current = get(self.conn, receipt_id)?.context("action receiptがない")?;
        ensure_receipt_workspace(&current, self.workspace)?;
        if current.status != ReceiptStatus::Succeeded || current.compensated_at.is_some() {
            bail!("action receiptは補償できない: {receipt_id}");
        }
        if !current.request.reversible {
            bail!("action requestはreversibleではない");
        }
        let external_reference = current
            .external_reference
            .clone()
            .context("external referenceがない")?;
        let external_target = current
            .external_target
            .clone()
            .context("external targetがない")?;
        if external_target != current.request.target {
            bail!("external target mismatch");
        }
        let compensation_deadline = current
            .compensation_deadline
            .context("compensation deadlineがない")?;
        if now > compensation_deadline {
            bail!("compensation deadline expired");
        }
        let permit = CompensationPermit {
            receipt_id: current.receipt_id.clone(),
            workspace: current.workspace.clone(),
            request_hash: current.decision.request_hash.clone(),
            external_reference: external_reference.clone(),
            external_target: external_target.clone(),
            compensation_deadline,
        };
        let result = adapter.compensate(&permit)?;
        if result.external_reference != external_reference {
            bail!("compensation external reference mismatch");
        }
        if result.external_target != external_target {
            bail!("compensation external target mismatch");
        }
        if result.completed_at < now || result.completed_at > compensation_deadline {
            bail!("compensation completion is outside the permitted window");
        }
        mark_compensated(self.conn, receipt_id, result.completed_at)
    }
}

/// capability消費とpending receipt確保を同じIMMEDIATE transactionで行う。
fn reserve(
    conn: &mut Connection,
    workspace: &str,
    request: &ActionRequest,
    policy: &ActionPolicy,
    signed_capability: Option<&SignedCapabilityLease>,
    trusted_issuers: &TrustedCapabilityIssuers,
    now: i64,
) -> Result<ReservationAttempt> {
    if workspace.trim().is_empty() {
        return Ok(denied(request, "invalid_workspace"));
    }
    if let Some(signed) = signed_capability {
        if let Err(failure) = trusted_issuers.verify(signed, now) {
            return Ok(denied(request, failure.reason()));
        }
        if signed.workspace != workspace
            || signed.actor != request.actor
            || signed.client_surface != request.client_surface
            || signed.lease.request_hash != request_hash(request)
            || signed.lease.action != request.action
            || signed.lease.target != request.target
        {
            return Ok(denied(request, "capability_scope_mismatch"));
        }
    }

    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let hash = request_hash(request);
    let replay: Option<String> = transaction
        .query_row(
            "SELECT receipt_id FROM action_receipts
             WHERE request_hash=?1 OR idempotency_key=?2 LIMIT 1",
            rusqlite::params![hash, request.idempotency_key],
            |row| row.get(0),
        )
        .optional()?;
    if replay.is_some() {
        return Ok(denied(request, "replay_detected"));
    }

    let lease = signed_capability.map(|signed| &signed.lease);
    let decision = evaluate(request, policy, lease, &[], now);
    if decision.decision != DecisionKind::Allow {
        return Ok(ReservationAttempt {
            decision,
            receipt: None,
        });
    }

    let capability_id = match decision.authorization {
        Some(AuthorizationSource::Capability) => {
            let signed = signed_capability.context("allow判定に署名capabilityがない")?;
            let consumed: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM action_capability_uses WHERE capability_id=?1)",
                [&signed.lease.grant_id],
                |row| row.get(0),
            )?;
            if consumed {
                return Ok(denied(request, "capability_consumed"));
            }
            Some(signed.lease.grant_id.clone())
        }
        Some(AuthorizationSource::StandingPolicy) => None,
        None => bail!("allow判定にauthorization sourceがない"),
    };

    let receipt_id = receipt_id(&hash, &request.idempotency_key);
    transaction.execute(
        "INSERT INTO action_receipts(
            receipt_id, workspace, request_hash, idempotency_key, capability_id,
            request_json, decision_json, status, reserved_at
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8)",
        rusqlite::params![
            receipt_id,
            workspace,
            hash,
            request.idempotency_key,
            capability_id,
            serde_json::to_string(request)?,
            serde_json::to_string(&decision)?,
            now,
        ],
    )?;
    if let (Some(capability_id), Some(signed)) = (&capability_id, signed_capability) {
        transaction.execute(
            "INSERT INTO action_capability_uses(capability_id, issuer, receipt_id)
             VALUES(?1, ?2, ?3)",
            rusqlite::params![capability_id, signed.issuer, receipt_id],
        )?;
    }
    transaction.commit()?;
    let receipt = get(conn, &receipt_id)?.context("予約直後のreceiptがない")?;
    Ok(ReservationAttempt {
        decision,
        receipt: Some(receipt),
    })
}

/// adapter呼出し直前に開始境界を記録する。ここから先のcrashは外部照会が必要。
fn start_execution(
    conn: &mut Connection,
    receipt_id: &str,
    execution_started_at: i64,
) -> Result<PersistedExecutionReceipt> {
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current = get(&transaction, receipt_id)?.context("action receiptがない")?;
    if current.status != ReceiptStatus::Pending || current.execution_started_at.is_some() {
        bail!("action receiptは実行開始済み: {receipt_id}");
    }
    if execution_started_at < current.reserved_at {
        bail!("execution_started_at is before reserved_at");
    }
    let changed = transaction.execute(
        "UPDATE action_receipts
         SET execution_started_at=?2
         WHERE receipt_id=?1 AND status='pending' AND execution_started_at IS NULL",
        rusqlite::params![receipt_id, execution_started_at],
    )?;
    if changed != 1 {
        bail!("action receiptの実行開始をatomicに記録できない: {receipt_id}");
    }
    transaction.commit()?;
    get(conn, receipt_id)?.context("実行開始直後のreceiptがない")
}

/// 外部実行結果をpending receiptへ一度だけ確定する。
fn finalize(
    conn: &mut Connection,
    receipt_id: &str,
    outcome: ExecutionOutcome,
    external_reference: Option<&str>,
    external_target: Option<&str>,
    completed_at: i64,
    compensation_deadline: Option<i64>,
) -> Result<PersistedExecutionReceipt> {
    if external_reference.is_some_and(|reference| reference.trim().is_empty()) {
        bail!("external reference must not be empty");
    }
    if outcome == ExecutionOutcome::Succeeded && external_reference.is_none() {
        bail!("successful action requires an external reference");
    }
    if outcome == ExecutionOutcome::Succeeded && external_target.is_none() {
        bail!("successful action requires an external target");
    }
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current = get(&transaction, receipt_id)?.context("action receiptがない")?;
    if current.status != ReceiptStatus::Pending {
        bail!("action receiptは確定済み: {receipt_id}");
    }
    let execution_started_at = current
        .execution_started_at
        .context("action receiptは外部実行を開始していない")?;
    if completed_at < execution_started_at {
        bail!("completed_at is before execution_started_at");
    }
    if external_target.is_some_and(|target| target != current.request.target) {
        bail!("external target mismatch");
    }
    if current.request.reversible
        && outcome == ExecutionOutcome::Succeeded
        && compensation_deadline.is_none()
    {
        bail!("reversible success requires a compensation deadline");
    }
    if compensation_deadline.is_some_and(|deadline| deadline < completed_at) {
        bail!("compensation deadline is before completion");
    }
    if outcome == ExecutionOutcome::Failed
        && (external_reference.is_some()
            || external_target.is_some()
            || compensation_deadline.is_some())
    {
        bail!("failed action must not assert successful external state");
    }
    let status = match outcome {
        ExecutionOutcome::Succeeded => ReceiptStatus::Succeeded,
        ExecutionOutcome::Failed => ReceiptStatus::Failed,
    };
    let changed = transaction.execute(
        "UPDATE action_receipts
         SET status=?2, external_reference=?3, external_target=?4,
             completed_at=?5, compensation_deadline=?6
         WHERE receipt_id=?1 AND status='pending'",
        rusqlite::params![
            receipt_id,
            status.as_str(),
            external_reference,
            external_target,
            completed_at,
            compensation_deadline,
        ],
    )?;
    if changed != 1 {
        bail!("action receiptをatomicに確定できない: {receipt_id}");
    }
    transaction.commit()?;
    get(conn, receipt_id)?.context("確定直後のreceiptがない")
}

fn fail_before_execution(
    conn: &mut Connection,
    receipt_id: &str,
    completed_at: i64,
) -> Result<PersistedExecutionReceipt> {
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current = get(&transaction, receipt_id)?.context("action receiptがない")?;
    if current.status != ReceiptStatus::Pending || current.execution_started_at.is_some() {
        bail!("action receiptは未開始pendingではない: {receipt_id}");
    }
    if completed_at < current.reserved_at {
        bail!("completed_at is before reserved_at");
    }
    let changed = transaction.execute(
        "UPDATE action_receipts SET status='failed', completed_at=?2
         WHERE receipt_id=?1 AND status='pending' AND execution_started_at IS NULL",
        rusqlite::params![receipt_id, completed_at],
    )?;
    if changed != 1 {
        bail!("未開始action receiptをatomicに確定できない: {receipt_id}");
    }
    transaction.commit()?;
    get(conn, receipt_id)?.context("確定直後のreceiptがない")
}

fn mark_compensated(
    conn: &mut Connection,
    receipt_id: &str,
    compensated_at: i64,
) -> Result<PersistedExecutionReceipt> {
    let changed = conn.execute(
        "UPDATE action_receipts SET compensated_at=?2
         WHERE receipt_id=?1 AND status='succeeded' AND compensated_at IS NULL",
        rusqlite::params![receipt_id, compensated_at],
    )?;
    if changed != 1 {
        bail!("action receiptの補償をatomicに確定できない: {receipt_id}");
    }
    get(conn, receipt_id)?.context("補償直後のreceiptがない")
}

fn ensure_receipt_workspace(receipt: &PersistedExecutionReceipt, workspace: &str) -> Result<()> {
    if receipt.workspace != workspace {
        bail!("action receipt workspace mismatch");
    }
    Ok(())
}

pub fn pending(conn: &Connection) -> Result<Vec<PersistedExecutionReceipt>> {
    let mut statement = conn.prepare(
        "SELECT receipt_id FROM action_receipts
         WHERE status='pending' ORDER BY reserved_at, receipt_id",
    )?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ids.into_iter()
        .map(|id| get(conn, &id)?.context("pending receiptが消えた"))
        .collect()
}

pub fn get(conn: &Connection, receipt_id: &str) -> Result<Option<PersistedExecutionReceipt>> {
    let stored: Option<StoredReceiptRow> = conn
        .query_row(
            "SELECT workspace, request_json, decision_json, capability_id, status,
                    reserved_at, execution_started_at, completed_at, external_reference,
                    external_target, compensation_deadline, compensated_at
             FROM action_receipts WHERE receipt_id=?1",
            [receipt_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                ))
            },
        )
        .optional()?;
    stored
        .map(
            |(
                workspace,
                request_json,
                decision_json,
                capability_id,
                status,
                reserved_at,
                execution_started_at,
                completed_at,
                external_reference,
                external_target,
                compensation_deadline,
                compensated_at,
            )| {
                Ok(PersistedExecutionReceipt {
                    schema: PERSISTED_RECEIPT_SCHEMA.to_owned(),
                    receipt_id: receipt_id.to_owned(),
                    workspace,
                    request: serde_json::from_str(&request_json)
                        .context("stored action requestをparseできない")?,
                    decision: serde_json::from_str(&decision_json)
                        .context("stored action decisionをparseできない")?,
                    capability_id,
                    status: ReceiptStatus::parse(&status)?,
                    reserved_at,
                    execution_started_at,
                    completed_at,
                    external_reference,
                    external_target,
                    compensation_deadline,
                    compensated_at,
                })
            },
        )
        .transpose()
}

fn denied(request: &ActionRequest, reason: &str) -> ReservationAttempt {
    ReservationAttempt {
        decision: PolicyDecision::denied(request, reason),
        receipt: None,
    }
}

fn receipt_id(request_hash: &str, idempotency_key: &str) -> String {
    let digest =
        Sha256::digest(format!("action-receipt:v1\0{request_hash}\0{idempotency_key}").as_bytes());
    format!("sha256:{digest:x}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FixtureObject {
    external_reference: String,
    target: String,
    completed_at: i64,
    compensation_deadline: i64,
    active: bool,
}

/// 外部reversible writeを決定的に模す受入fixture。
///
/// production serviceへ接続せず、idempotency・reconcile・compensationの共通境界だけを
/// 固定する。副作用操作はpermitを要求するtrait経由に限定する。
#[derive(Debug, Default)]
pub struct ReversibleFixtureAdapter {
    objects: BTreeMap<String, FixtureObject>,
    hidden_from_reconcile: BTreeSet<String>,
    now: i64,
    compensation_window: i64,
}

impl ReversibleFixtureAdapter {
    pub fn new(now: i64, compensation_window: i64) -> Result<Self> {
        if compensation_window <= 0 {
            bail!("compensation window must be positive");
        }
        Ok(Self {
            objects: BTreeMap::new(),
            hidden_from_reconcile: BTreeSet::new(),
            now,
            compensation_window,
        })
    }

    pub fn set_now(&mut self, now: i64) {
        self.now = now;
    }

    pub fn hide_from_reconcile(&mut self, idempotency_key: &str, hidden: bool) {
        if hidden {
            self.hidden_from_reconcile
                .insert(idempotency_key.to_owned());
        } else {
            self.hidden_from_reconcile.remove(idempotency_key);
        }
    }

    pub fn object_count(&self) -> usize {
        self.objects.len()
    }

    pub fn is_active(&self, idempotency_key: &str) -> bool {
        self.objects
            .get(idempotency_key)
            .is_some_and(|object| object.active)
    }
}

impl GovernedActionAdapter for ReversibleFixtureAdapter {
    fn execute(&mut self, permit: &ExecutionPermit) -> AdapterExecution {
        let object = self
            .objects
            .entry(permit.idempotency_key().to_owned())
            .or_insert_with(|| {
                let digest = Sha256::digest(
                    format!(
                        "fixture-object:v1\0{}\0{}",
                        permit.idempotency_key(),
                        permit.target()
                    )
                    .as_bytes(),
                );
                FixtureObject {
                    external_reference: format!("fixture:object:{digest:x}"),
                    target: permit.target().to_owned(),
                    completed_at: self.now,
                    compensation_deadline: self.now + self.compensation_window,
                    active: true,
                }
            });
        AdapterExecution {
            outcome: ExecutionOutcome::Succeeded,
            external_reference: Some(object.external_reference.clone()),
            external_target: Some(object.target.clone()),
            completed_at: object.completed_at,
            compensation_deadline: Some(object.compensation_deadline),
        }
    }
}

impl ReconciliableActionAdapter for ReversibleFixtureAdapter {
    fn reconcile(&mut self, query: &ReconciliationQuery) -> AdapterReconciliation {
        if self.hidden_from_reconcile.contains(query.idempotency_key()) {
            return AdapterReconciliation {
                state: ReconciliationState::StillUnknown,
                external_reference: None,
                external_target: None,
                observed_at: self.now,
                compensation_deadline: None,
            };
        }
        let Some(object) = self.objects.get(query.idempotency_key()) else {
            return AdapterReconciliation {
                state: ReconciliationState::Failed,
                external_reference: None,
                external_target: None,
                observed_at: self.now,
                compensation_deadline: None,
            };
        };
        AdapterReconciliation {
            state: ReconciliationState::Succeeded,
            external_reference: Some(object.external_reference.clone()),
            external_target: Some(object.target.clone()),
            observed_at: self.now,
            compensation_deadline: Some(object.compensation_deadline),
        }
    }
}

impl GovernedReversibleActionAdapter for ReversibleFixtureAdapter {
    fn compensate(&mut self, permit: &CompensationPermit) -> Result<AdapterCompensation> {
        let object = self
            .objects
            .values_mut()
            .find(|object| object.external_reference == permit.external_reference())
            .context("compensation対象のfixture objectがない")?;
        if object.target != permit.external_target() {
            bail!("fixture compensation target mismatch");
        }
        object.active = false;
        Ok(AdapterCompensation {
            external_reference: object.external_reference.clone(),
            external_target: object.target.clone(),
            completed_at: self.now,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    use super::*;
    use crate::action_governance::{
        ACTION_CAPABILITY_SCHEMA, ActionKind, CapabilityLease, RiskClass,
        sign_capability_hmac_sha256,
    };
    use crate::index::{open_db, open_db_recovery};
    use crate::vault::Vault;

    const NOW: i64 = 1_800_000_000;
    const KEY: &[u8; 32] = b"0123456789abcdef0123456789abcdef";
    const WORKSPACE: &str = "workspace:test";

    fn request() -> ActionRequest {
        ActionRequest {
            schema: crate::action_governance::ACTION_REQUEST_SCHEMA.to_owned(),
            actor: "human:owner".to_owned(),
            client_surface: "codex".to_owned(),
            action: ActionKind::Delete,
            resource_type: "github.issue".to_owned(),
            target: "github:mcaz/kb-app:issue/74".to_owned(),
            risk: RiskClass::Destructive,
            reversible: true,
            cost: None,
            idempotency_key: "issue-74-reservation".to_owned(),
            expires_at: NOW + 300,
            justification: "atomic receipt test".to_owned(),
        }
    }

    fn policy() -> ActionPolicy {
        ActionPolicy {
            allowed_actions: vec![ActionKind::Delete],
            target_prefixes: vec!["github:mcaz/kb-app:".to_owned()],
            allow_reversible_writes: false,
            max_cost: None,
        }
    }

    fn signed(request: &ActionRequest) -> SignedCapabilityLease {
        sign_capability_hmac_sha256(
            CapabilityLease {
                schema: ACTION_CAPABILITY_SCHEMA.to_owned(),
                grant_id: format!("grant:{}", request.idempotency_key),
                request_hash: request_hash(request),
                action: request.action,
                target: request.target.clone(),
                expires_at: NOW + 120,
                remaining_uses: 1,
                max_cost: None,
            },
            request,
            WORKSPACE,
            "local:owner",
            NOW - 1,
            "nonce:issue-74",
            KEY,
        )
        .unwrap()
    }

    fn trusted() -> TrustedCapabilityIssuers {
        let mut trusted = TrustedCapabilityIssuers::default();
        trusted.trust_hmac_sha256("local:owner", KEY).unwrap();
        trusted
    }

    fn vault() -> (tempfile::TempDir, Vault) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        drop(open_db(&vault).unwrap());
        (dir, vault)
    }

    #[test]
    fn invalid_unknown_and_mutated_signatures_fail_closed_without_a_receipt() {
        let (_dir, vault) = vault();
        let mut conn = open_db_recovery(&vault).unwrap();
        let request = request();
        let valid_signed = signed(&request);
        let unknown = TrustedCapabilityIssuers::default();
        let result = reserve(
            &mut conn,
            WORKSPACE,
            &request,
            &policy(),
            Some(&valid_signed),
            &unknown,
            NOW,
        )
        .unwrap();
        assert_eq!(result.decision.reason, "unknown_capability_issuer");
        assert!(result.receipt.is_none());

        let mut mutated = valid_signed;
        mutated.lease.target = "github:mcaz/other:issue/74".to_owned();
        let result = reserve(
            &mut conn,
            WORKSPACE,
            &request,
            &policy(),
            Some(&mutated),
            &trusted(),
            NOW,
        )
        .unwrap();
        assert_eq!(result.decision.reason, "invalid_capability_signature");

        let mut zero_use_lease = signed(&request).lease;
        zero_use_lease.remaining_uses = 0;
        let zero_use = sign_capability_hmac_sha256(
            zero_use_lease,
            &request,
            WORKSPACE,
            "local:owner",
            NOW - 1,
            "nonce:zero-use",
            KEY,
        )
        .unwrap();
        let result = reserve(
            &mut conn,
            WORKSPACE,
            &request,
            &policy(),
            Some(&zero_use),
            &trusted(),
            NOW,
        )
        .unwrap();
        assert_eq!(result.decision.reason, "capability_must_be_single_use");

        let mut expired_lease = signed(&request).lease;
        expired_lease.expires_at = NOW;
        let expired = sign_capability_hmac_sha256(
            expired_lease,
            &request,
            WORKSPACE,
            "local:owner",
            NOW - 1,
            "nonce:expired",
            KEY,
        )
        .unwrap();
        let result = reserve(
            &mut conn,
            WORKSPACE,
            &request,
            &policy(),
            Some(&expired),
            &trusted(),
            NOW,
        )
        .unwrap();
        assert_eq!(result.decision.reason, "capability_expired");
        assert!(pending(&conn).unwrap().is_empty());
    }

    #[test]
    fn signed_capability_cannot_cross_workspace_actor_or_client_surface() {
        let (_dir, vault) = vault();
        let mut conn = open_db_recovery(&vault).unwrap();
        let original = request();
        let signed = signed(&original);

        let wrong_workspace = reserve(
            &mut conn,
            "workspace:other",
            &original,
            &policy(),
            Some(&signed),
            &trusted(),
            NOW,
        )
        .unwrap();
        assert_eq!(wrong_workspace.decision.reason, "capability_scope_mismatch");

        let mut wrong_actor = original.clone();
        wrong_actor.actor = "ai:other".to_owned();
        let wrong_actor = reserve(
            &mut conn,
            WORKSPACE,
            &wrong_actor,
            &policy(),
            Some(&signed),
            &trusted(),
            NOW,
        )
        .unwrap();
        assert_eq!(wrong_actor.decision.reason, "capability_scope_mismatch");

        let mut wrong_surface = original.clone();
        wrong_surface.client_surface = "other-client".to_owned();
        let wrong_surface = reserve(
            &mut conn,
            WORKSPACE,
            &wrong_surface,
            &policy(),
            Some(&signed),
            &trusted(),
            NOW,
        )
        .unwrap();
        assert_eq!(wrong_surface.decision.reason, "capability_scope_mismatch");

        let mut changed_action = original.clone();
        changed_action.action = ActionKind::Update;
        let mut changed_target = original.clone();
        changed_target.target = "github:mcaz/kb-app:issue/75".to_owned();
        let mut changed_cost = original.clone();
        changed_cost.cost = Some(crate::action_governance::CostEnvelope {
            currency: "JPY".to_owned(),
            max_minor_units: 1,
        });
        let mut changed_expiry = original;
        changed_expiry.expires_at += 1;
        for changed_request in [changed_action, changed_target, changed_cost, changed_expiry] {
            let result = reserve(
                &mut conn,
                WORKSPACE,
                &changed_request,
                &policy(),
                Some(&signed),
                &trusted(),
                NOW,
            )
            .unwrap();
            assert_eq!(result.decision.reason, "capability_scope_mismatch");
        }
        assert!(pending(&conn).unwrap().is_empty());
    }

    #[test]
    fn concurrent_single_use_capability_creates_exactly_one_pending_receipt() {
        let (_dir, vault) = vault();
        let path = vault.index_db_path();
        let request = request();
        let signed = signed(&request);
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let path = path.clone();
            let request = request.clone();
            let signed = signed.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let mut conn = Connection::open(path).unwrap();
                conn.busy_timeout(Duration::from_secs(5)).unwrap();
                barrier.wait();
                reserve(
                    &mut conn,
                    WORKSPACE,
                    &request,
                    &policy(),
                    Some(&signed),
                    &trusted(),
                    NOW,
                )
                .unwrap()
            }));
        }
        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            results
                .iter()
                .filter(|result| result.decision.decision == DecisionKind::Allow)
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| result.receipt.is_some())
                .count(),
            1
        );
        let conn = open_db_recovery(&vault).unwrap();
        assert_eq!(pending(&conn).unwrap().len(), 1);
    }

    #[test]
    fn restart_keeps_pending_for_reconcile_and_blocks_replay() {
        let (_dir, vault) = vault();
        let request = request();
        let signed = signed(&request);
        let receipt_id = {
            let mut conn = open_db_recovery(&vault).unwrap();
            reserve(
                &mut conn,
                WORKSPACE,
                &request,
                &policy(),
                Some(&signed),
                &trusted(),
                NOW,
            )
            .unwrap()
            .receipt
            .unwrap()
            .receipt_id
        };

        let mut reopened = open_db_recovery(&vault).unwrap();
        let before_external_start = &pending(&reopened).unwrap()[0];
        assert_eq!(before_external_start.receipt_id, receipt_id);
        assert_eq!(before_external_start.execution_started_at, None);
        start_execution(&mut reopened, &receipt_id, NOW + 1).unwrap();
        drop(reopened);

        let mut reopened = open_db_recovery(&vault).unwrap();
        assert_eq!(
            pending(&reopened).unwrap()[0].execution_started_at,
            Some(NOW + 1)
        );
        let replay = reserve(
            &mut reopened,
            WORKSPACE,
            &request,
            &policy(),
            Some(&signed),
            &trusted(),
            NOW + 1,
        )
        .unwrap();
        assert_eq!(replay.decision.reason, "replay_detected");
    }

    #[test]
    fn gate_records_execution_start_before_adapter_and_skips_adapter_on_replay() {
        let (_dir, vault) = vault();
        let path = vault.index_db_path();
        let request = request();
        let signed = signed(&request);
        let trusted = trusted();
        let mut conn = open_db_recovery(&vault).unwrap();
        let calls = Cell::new(0);
        let mut adapter = |permit: &ExecutionPermit| {
            calls.set(calls.get() + 1);
            assert_eq!(permit.workspace(), WORKSPACE);
            assert_eq!(permit.request_hash(), request_hash(&request));
            assert_eq!(permit.idempotency_key(), request.idempotency_key);
            let observer = Connection::open(&path).unwrap();
            let observed = get(&observer, permit.receipt_id()).unwrap().unwrap();
            assert_eq!(observed.status, ReceiptStatus::Pending);
            assert_eq!(observed.execution_started_at, Some(NOW));
            AdapterExecution {
                outcome: ExecutionOutcome::Succeeded,
                external_reference: Some("github:issue/74".to_owned()),
                external_target: Some(request.target.clone()),
                completed_at: NOW + 1,
                compensation_deadline: Some(NOW + 60),
            }
        };

        let succeeded = ActionGovernanceGate::new(&mut conn, WORKSPACE, &trusted)
            .run(&request, &policy(), Some(&signed), NOW, &mut adapter)
            .unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(succeeded.receipt.unwrap().status, ReceiptStatus::Succeeded);

        let replay = ActionGovernanceGate::new(&mut conn, WORKSPACE, &trusted)
            .run(&request, &policy(), Some(&signed), NOW + 2, &mut adapter)
            .unwrap();
        assert_eq!(replay.decision.reason, "replay_detected");
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn failed_execution_is_final_and_never_restores_the_capability() {
        let (_dir, vault) = vault();
        let request = request();
        let signed = signed(&request);
        let mut conn = open_db_recovery(&vault).unwrap();
        let receipt = reserve(
            &mut conn,
            WORKSPACE,
            &request,
            &policy(),
            Some(&signed),
            &trusted(),
            NOW,
        )
        .unwrap()
        .receipt
        .unwrap();
        let receipt = start_execution(&mut conn, &receipt.receipt_id, NOW).unwrap();
        let failed = finalize(
            &mut conn,
            &receipt.receipt_id,
            ExecutionOutcome::Failed,
            None,
            None,
            NOW + 1,
            None,
        )
        .unwrap();
        assert_eq!(failed.status, ReceiptStatus::Failed);
        assert!(pending(&conn).unwrap().is_empty());
        assert!(
            finalize(
                &mut conn,
                &receipt.receipt_id,
                ExecutionOutcome::Succeeded,
                Some("github:issue/74"),
                Some(&request.target),
                NOW + 2,
                Some(NOW + 60),
            )
            .is_err()
        );
        let replay = reserve(
            &mut conn,
            WORKSPACE,
            &request,
            &policy(),
            Some(&signed),
            &trusted(),
            NOW + 2,
        )
        .unwrap();
        assert_eq!(replay.decision.reason, "replay_detected");
    }

    #[test]
    fn fixture_is_idempotent_and_compensation_is_permit_scoped_once() {
        let (_dir, vault) = vault();
        let request = request();
        let signed = signed(&request);
        let trusted = trusted();
        let mut conn = open_db_recovery(&vault).unwrap();
        let mut adapter = ReversibleFixtureAdapter::new(NOW + 1, 60).unwrap();

        let succeeded = ActionGovernanceGate::new(&mut conn, WORKSPACE, &trusted)
            .run(&request, &policy(), Some(&signed), NOW, &mut adapter)
            .unwrap()
            .receipt
            .unwrap();
        assert_eq!(succeeded.status, ReceiptStatus::Succeeded);
        assert_eq!(
            succeeded.external_target.as_deref(),
            Some(request.target.as_str())
        );
        assert_eq!(succeeded.compensation_deadline, Some(NOW + 61));
        assert_eq!(adapter.object_count(), 1);
        assert!(adapter.is_active(&request.idempotency_key));

        let replay = ActionGovernanceGate::new(&mut conn, WORKSPACE, &trusted)
            .run(&request, &policy(), Some(&signed), NOW + 2, &mut adapter)
            .unwrap();
        assert_eq!(replay.decision.reason, "replay_detected");
        assert_eq!(adapter.object_count(), 1);

        adapter.set_now(NOW + 3);
        let compensated = ActionGovernanceGate::new(&mut conn, WORKSPACE, &trusted)
            .compensate(&succeeded.receipt_id, NOW + 3, &mut adapter)
            .unwrap();
        assert_eq!(compensated.compensated_at, Some(NOW + 3));
        assert!(!adapter.is_active(&request.idempotency_key));
        assert!(
            ActionGovernanceGate::new(&mut conn, WORKSPACE, &trusted)
                .compensate(&succeeded.receipt_id, NOW + 4, &mut adapter)
                .is_err()
        );
    }

    #[test]
    fn post_execution_crash_reconciles_without_creating_a_second_object() {
        let (_dir, vault) = vault();
        let request = request();
        let signed = signed(&request);
        let trusted = trusted();
        let mut adapter = ReversibleFixtureAdapter::new(NOW + 1, 60).unwrap();
        let receipt_id = {
            let mut conn = open_db_recovery(&vault).unwrap();
            let receipt = reserve(
                &mut conn,
                WORKSPACE,
                &request,
                &policy(),
                Some(&signed),
                &trusted,
                NOW,
            )
            .unwrap()
            .receipt
            .unwrap();
            let executing = start_execution(&mut conn, &receipt.receipt_id, NOW).unwrap();
            let permit = ExecutionPermit {
                receipt_id: executing.receipt_id.clone(),
                workspace: executing.workspace.clone(),
                request_hash: executing.decision.request_hash.clone(),
                idempotency_key: executing.request.idempotency_key.clone(),
                target: executing.request.target.clone(),
            };
            let external = adapter.execute(&permit);
            assert_eq!(external.outcome, ExecutionOutcome::Succeeded);
            executing.receipt_id
        };
        assert_eq!(adapter.object_count(), 1);

        adapter.set_now(NOW + 2);
        let mut reopened = open_db_recovery(&vault).unwrap();
        let reconciled = ActionGovernanceGate::new(&mut reopened, WORKSPACE, &trusted)
            .reconcile(&receipt_id, NOW + 2, &mut adapter)
            .unwrap();
        assert_eq!(reconciled.status, ReceiptStatus::Succeeded);
        assert_eq!(adapter.object_count(), 1);
        let replay = reserve(
            &mut reopened,
            WORKSPACE,
            &request,
            &policy(),
            Some(&signed),
            &trusted,
            NOW + 3,
        )
        .unwrap();
        assert_eq!(replay.decision.reason, "replay_detected");
        assert_eq!(adapter.object_count(), 1);
    }

    #[test]
    fn unknown_reconcile_stays_pending_and_unstarted_pending_fails_without_execution() {
        let (_dir, vault) = vault();
        let original_request = request();
        let original_signed = signed(&original_request);
        let trusted = trusted();
        let mut conn = open_db_recovery(&vault).unwrap();
        let receipt = reserve(
            &mut conn,
            WORKSPACE,
            &original_request,
            &policy(),
            Some(&original_signed),
            &trusted,
            NOW,
        )
        .unwrap()
        .receipt
        .unwrap();
        let mut adapter = ReversibleFixtureAdapter::new(NOW + 1, 60).unwrap();
        let failed = ActionGovernanceGate::new(&mut conn, WORKSPACE, &trusted)
            .reconcile(&receipt.receipt_id, NOW + 1, &mut adapter)
            .unwrap();
        assert_eq!(failed.status, ReceiptStatus::Failed);
        assert_eq!(adapter.object_count(), 0);

        let mut second = request();
        second.idempotency_key = "issue-78-unknown".to_owned();
        let signed = signed(&second);
        let receipt = reserve(
            &mut conn,
            WORKSPACE,
            &second,
            &policy(),
            Some(&signed),
            &trusted,
            NOW + 2,
        )
        .unwrap()
        .receipt
        .unwrap();
        start_execution(&mut conn, &receipt.receipt_id, NOW + 2).unwrap();
        adapter.hide_from_reconcile(&second.idempotency_key, true);
        adapter.set_now(NOW + 3);
        let pending = ActionGovernanceGate::new(&mut conn, WORKSPACE, &trusted)
            .reconcile(&receipt.receipt_id, NOW + 3, &mut adapter)
            .unwrap();
        assert_eq!(pending.status, ReceiptStatus::Pending);
        assert_eq!(pending.completed_at, None);
        assert_eq!(adapter.object_count(), 0);
    }

    #[test]
    fn target_and_expired_compensation_mismatches_fail_closed() {
        let (_dir, vault) = vault();
        let request = request();
        let signed = signed(&request);
        let trusted = trusted();
        let mut conn = open_db_recovery(&vault).unwrap();
        let mut adapter = ReversibleFixtureAdapter::new(NOW + 1, 2).unwrap();
        let succeeded = ActionGovernanceGate::new(&mut conn, WORKSPACE, &trusted)
            .run(&request, &policy(), Some(&signed), NOW, &mut adapter)
            .unwrap()
            .receipt
            .unwrap();

        conn.execute(
            "UPDATE action_receipts SET external_target='fixture:wrong-target'
             WHERE receipt_id=?1",
            [&succeeded.receipt_id],
        )
        .unwrap();
        adapter.set_now(NOW + 2);
        assert!(
            ActionGovernanceGate::new(&mut conn, WORKSPACE, &trusted)
                .compensate(&succeeded.receipt_id, NOW + 2, &mut adapter)
                .is_err()
        );
        assert!(adapter.is_active(&request.idempotency_key));

        conn.execute(
            "UPDATE action_receipts SET external_target=?2 WHERE receipt_id=?1",
            rusqlite::params![succeeded.receipt_id, request.target],
        )
        .unwrap();
        adapter.set_now(NOW + 4);
        assert!(
            ActionGovernanceGate::new(&mut conn, WORKSPACE, &trusted)
                .compensate(&succeeded.receipt_id, NOW + 4, &mut adapter)
                .is_err()
        );
        assert!(adapter.is_active(&request.idempotency_key));
    }

    #[test]
    fn concurrent_reconcile_finalizes_the_pending_receipt_once() {
        struct SuccessfulReconciler {
            barrier: Arc<Barrier>,
            target: String,
        }

        impl GovernedActionAdapter for SuccessfulReconciler {
            fn execute(&mut self, _permit: &ExecutionPermit) -> AdapterExecution {
                unreachable!("reconcile test does not execute")
            }
        }

        impl ReconciliableActionAdapter for SuccessfulReconciler {
            fn reconcile(&mut self, _query: &ReconciliationQuery) -> AdapterReconciliation {
                self.barrier.wait();
                AdapterReconciliation {
                    state: ReconciliationState::Succeeded,
                    external_reference: Some("fixture:object:concurrent".to_owned()),
                    external_target: Some(self.target.clone()),
                    observed_at: NOW + 2,
                    compensation_deadline: Some(NOW + 60),
                }
            }
        }

        let (_dir, vault) = vault();
        let path = vault.index_db_path();
        let request = request();
        let signed = signed(&request);
        let receipt_id = {
            let mut conn = open_db_recovery(&vault).unwrap();
            let receipt = reserve(
                &mut conn,
                WORKSPACE,
                &request,
                &policy(),
                Some(&signed),
                &trusted(),
                NOW,
            )
            .unwrap()
            .receipt
            .unwrap();
            start_execution(&mut conn, &receipt.receipt_id, NOW)
                .unwrap()
                .receipt_id
        };
        let barrier = Arc::new(Barrier::new(2));
        let handles = (0..2)
            .map(|_| {
                let path = path.clone();
                let receipt_id = receipt_id.clone();
                let barrier = Arc::clone(&barrier);
                let target = request.target.clone();
                std::thread::spawn(move || {
                    let mut conn = Connection::open(path).unwrap();
                    conn.busy_timeout(Duration::from_secs(5)).unwrap();
                    let trusted = trusted();
                    let mut adapter = SuccessfulReconciler { barrier, target };
                    ActionGovernanceGate::new(&mut conn, WORKSPACE, &trusted).reconcile(
                        &receipt_id,
                        NOW + 2,
                        &mut adapter,
                    )
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        let receipt = get(&open_db_recovery(&vault).unwrap(), &receipt_id)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.status, ReceiptStatus::Succeeded);
        assert_eq!(
            receipt.external_reference.as_deref(),
            Some("fixture:object:concurrent")
        );
    }
}
