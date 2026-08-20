//! 外部副作用をサービス非依存で判定する、純粋な policy evaluator。
//!
//! このモジュールは実行も権限発行も永続化もしない。外部 adapter が実行前に
//! [`evaluate`] を呼び、実行後に [`ExecutionReceipt`] を保存するための共通語彙を提供する。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

pub const ACTION_REQUEST_SCHEMA: &str = "kb.action-request.v1";
pub const ACTION_CAPABILITY_SCHEMA: &str = "kb.action-capability.v1";
pub const ACTION_DECISION_SCHEMA: &str = "kb.action-decision.v1";
pub const ACTION_RECEIPT_SCHEMA: &str = "kb.action-receipt.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Read,
    Create,
    Update,
    Delete,
    Purchase,
    Contract,
    Publish,
    Deploy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    ReadOnly,
    ReversibleWrite,
    Destructive,
    Billable,
    LegalContractual,
    PublicSecuritySensitive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostEnvelope {
    /// ISO 4217 currency code。比較時は大文字小文字を区別しない。
    pub currency: String,
    /// 最小通貨単位（JPYなら円、USDならcent）で表す上限。
    pub max_minor_units: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionRequest {
    pub schema: String,
    pub actor: String,
    pub client_surface: String,
    pub action: ActionKind,
    pub resource_type: String,
    /// connector / account / resource を含む完全修飾対象。
    pub target: String,
    pub risk: RiskClass,
    pub reversible: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<CostEnvelope>,
    pub idempotency_key: String,
    /// Unix timestamp。期限切れ要求を adapter の手前で拒否する。
    pub expires_at: i64,
    pub justification: String,
}

impl ActionRequest {
    /// 現行のAI自律削除planを共通モデルへ写像する。
    ///
    /// `note_fingerprint` を冪等キーへ含めることで、plan発行後に内容が変わった
    /// ノートへ権限を流用できない。Gitで復旧可能でも操作自体は destructive と扱う。
    pub fn for_note_removal(
        actor: impl Into<String>,
        client_surface: impl Into<String>,
        note_id: impl Into<String>,
        note_fingerprint: impl Into<String>,
        expires_at: i64,
        justification: impl Into<String>,
    ) -> Self {
        let note_id = note_id.into();
        Self {
            schema: ACTION_REQUEST_SCHEMA.to_owned(),
            actor: actor.into(),
            client_surface: client_surface.into(),
            action: ActionKind::Delete,
            resource_type: "kb.note".to_owned(),
            target: format!("kb.note:{note_id}"),
            risk: RiskClass::Destructive,
            reversible: true,
            cost: None,
            idempotency_key: format!("kb.remove:{note_id}:{}", note_fingerprint.into()),
            expires_at,
            justification: justification.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionPolicy {
    pub allowed_actions: Vec<ActionKind>,
    /// 完全修飾対象に対するprefix allowlist。空なら全対象を拒否する。
    pub target_prefixes: Vec<String>,
    pub allow_reversible_writes: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_cost: Option<CostEnvelope>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityLease {
    pub schema: String,
    pub grant_id: String,
    pub request_hash: String,
    pub action: ActionKind,
    pub target: String,
    pub expires_at: i64,
    pub remaining_uses: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_cost: Option<CostEnvelope>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOutcome {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionReceipt {
    pub schema: String,
    pub request_hash: String,
    pub idempotency_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_id: Option<String>,
    pub outcome: ExecutionOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_reference: Option<String>,
    pub executed_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Allow,
    Deny,
    RequireHuman,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizationSource {
    StandingPolicy,
    Capability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDecision {
    pub schema: String,
    pub decision: DecisionKind,
    pub reason: String,
    pub request_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization: Option<AuthorizationSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_id: Option<String>,
}

/// 副作用を起こさず、同じ入力には常に同じ判定を返す。
pub fn evaluate(
    request: &ActionRequest,
    policy: &ActionPolicy,
    capability: Option<&CapabilityLease>,
    receipts: &[ExecutionReceipt],
    now: i64,
) -> PolicyDecision {
    let request_hash = request_hash(request);
    let deny = |reason: &str| decision(DecisionKind::Deny, reason, &request_hash, None, None);
    let require_human = |reason: &str| {
        decision(
            DecisionKind::RequireHuman,
            reason,
            &request_hash,
            None,
            None,
        )
    };

    if request.schema != ACTION_REQUEST_SCHEMA {
        return deny("unsupported_request_schema");
    }
    if request.actor.trim().is_empty()
        || request.client_surface.trim().is_empty()
        || request.resource_type.trim().is_empty()
        || request.target.trim().is_empty()
        || request.idempotency_key.trim().is_empty()
        || request.justification.trim().is_empty()
    {
        return deny("incomplete_request");
    }
    if !request.target.contains(':') {
        return deny("target_not_fully_qualified");
    }
    if request.expires_at <= now {
        return deny("request_expired");
    }
    if request.risk < minimum_risk(request.action, request.reversible) {
        return deny("risk_understated");
    }
    if receipts.iter().any(|receipt| {
        receipt.request_hash == request_hash || receipt.idempotency_key == request.idempotency_key
    }) {
        return deny("replay_detected");
    }
    if !policy.allowed_actions.contains(&request.action) {
        return deny("action_not_allowed");
    }
    if !policy
        .target_prefixes
        .iter()
        .any(|prefix| !prefix.is_empty() && request.target.starts_with(prefix))
    {
        return deny("target_out_of_scope");
    }

    if let Some(cost) = &request.cost {
        if !valid_currency(&cost.currency) {
            return deny("invalid_cost_currency");
        }
        match &policy.max_cost {
            Some(limit) if same_currency(cost, limit) => {
                if cost.max_minor_units > limit.max_minor_units {
                    return require_human("policy_cost_cap_exceeded");
                }
            }
            Some(_) => return deny("policy_cost_currency_mismatch"),
            None => return require_human("policy_has_no_cost_cap"),
        }
    } else if request.action == ActionKind::Purchase || request.risk == RiskClass::Billable {
        return deny("billable_action_without_cost");
    }

    if request.risk == RiskClass::ReadOnly {
        return decision(
            DecisionKind::Allow,
            "standing_policy_read",
            &request_hash,
            Some(AuthorizationSource::StandingPolicy),
            None,
        );
    }
    if request.risk == RiskClass::ReversibleWrite
        && request.reversible
        && policy.allow_reversible_writes
    {
        return decision(
            DecisionKind::Allow,
            "standing_policy_reversible_write",
            &request_hash,
            Some(AuthorizationSource::StandingPolicy),
            None,
        );
    }

    let Some(capability) = capability else {
        return require_human("capability_required");
    };
    if capability.schema != ACTION_CAPABILITY_SCHEMA {
        return deny("unsupported_capability_schema");
    }
    if capability.expires_at <= now {
        return deny("capability_expired");
    }
    if capability.remaining_uses == 0 {
        return deny("capability_exhausted");
    }
    if capability.request_hash != request_hash
        || capability.action != request.action
        || capability.target != request.target
    {
        return deny("capability_scope_mismatch");
    }
    if let Some(cost) = &request.cost {
        match &capability.max_cost {
            Some(limit) if same_currency(cost, limit) => {
                if cost.max_minor_units > limit.max_minor_units {
                    return deny("capability_cost_cap_exceeded");
                }
            }
            Some(_) => return deny("capability_cost_currency_mismatch"),
            None => return deny("capability_has_no_cost_cap"),
        }
    }

    decision(
        DecisionKind::Allow,
        "matching_capability",
        &request_hash,
        Some(AuthorizationSource::Capability),
        Some(capability.grant_id.clone()),
    )
}

pub fn request_hash(request: &ActionRequest) -> String {
    // serde_json::Map's default BTreeMap representation sorts object keys. Hashing the Value
    // therefore avoids binding a capability to Rust's struct field declaration order.
    let canonical =
        serde_json::to_value(request).expect("ActionRequest serialization is infallible");
    let encoded =
        serde_json::to_vec(&canonical).expect("ActionRequest serialization is infallible");
    let digest = Sha256::digest(encoded);
    let mut hash = String::with_capacity(71);
    hash.push_str("sha256:");
    for byte in digest {
        write!(&mut hash, "{byte:02x}").expect("writing to String is infallible");
    }
    hash
}

fn minimum_risk(action: ActionKind, reversible: bool) -> RiskClass {
    match action {
        ActionKind::Read => RiskClass::ReadOnly,
        ActionKind::Create | ActionKind::Update if reversible => RiskClass::ReversibleWrite,
        ActionKind::Create | ActionKind::Update | ActionKind::Delete => RiskClass::Destructive,
        ActionKind::Purchase => RiskClass::Billable,
        ActionKind::Contract => RiskClass::LegalContractual,
        ActionKind::Publish | ActionKind::Deploy => RiskClass::PublicSecuritySensitive,
    }
}

fn valid_currency(currency: &str) -> bool {
    currency.len() == 3 && currency.bytes().all(|byte| byte.is_ascii_alphabetic())
}

fn same_currency(left: &CostEnvelope, right: &CostEnvelope) -> bool {
    left.currency.eq_ignore_ascii_case(&right.currency)
}

fn decision(
    kind: DecisionKind,
    reason: &str,
    request_hash: &str,
    authorization: Option<AuthorizationSource>,
    capability_id: Option<String>,
) -> PolicyDecision {
    PolicyDecision {
        schema: ACTION_DECISION_SCHEMA.to_owned(),
        decision: kind,
        reason: reason.to_owned(),
        request_hash: request_hash.to_owned(),
        authorization,
        capability_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn request(action: ActionKind, risk: RiskClass, reversible: bool) -> ActionRequest {
        ActionRequest {
            schema: ACTION_REQUEST_SCHEMA.to_owned(),
            actor: "human:owner".to_owned(),
            client_surface: "codex".to_owned(),
            action,
            resource_type: "github.issue".to_owned(),
            target: "github:mcaz/kb-app:issue/72".to_owned(),
            risk,
            reversible,
            cost: None,
            idempotency_key: "issue-72-wave-1".to_owned(),
            expires_at: NOW + 300,
            justification: "Action Governance wave 1".to_owned(),
        }
    }

    fn policy(actions: Vec<ActionKind>) -> ActionPolicy {
        ActionPolicy {
            allowed_actions: actions,
            target_prefixes: vec!["github:mcaz/kb-app:".to_owned()],
            allow_reversible_writes: true,
            max_cost: Some(CostEnvelope {
                currency: "JPY".to_owned(),
                max_minor_units: 5_000,
            }),
        }
    }

    fn capability(request: &ActionRequest) -> CapabilityLease {
        CapabilityLease {
            schema: ACTION_CAPABILITY_SCHEMA.to_owned(),
            grant_id: "grant:wave-1".to_owned(),
            request_hash: request_hash(request),
            action: request.action,
            target: request.target.clone(),
            expires_at: NOW + 120,
            remaining_uses: 1,
            max_cost: request.cost.clone(),
        }
    }

    #[test]
    fn standing_policy_allows_read_and_explicitly_reversible_write() {
        let read = request(ActionKind::Read, RiskClass::ReadOnly, true);
        let update = request(ActionKind::Update, RiskClass::ReversibleWrite, true);
        let policy = policy(vec![ActionKind::Read, ActionKind::Update]);

        assert_eq!(
            evaluate(&read, &policy, None, &[], NOW).decision,
            DecisionKind::Allow
        );
        assert_eq!(
            evaluate(&update, &policy, None, &[], NOW).authorization,
            Some(AuthorizationSource::StandingPolicy)
        );
    }

    #[test]
    fn destructive_action_requires_a_matching_capability() {
        let request = request(ActionKind::Delete, RiskClass::Destructive, true);
        let policy = policy(vec![ActionKind::Delete]);

        assert_eq!(
            evaluate(&request, &policy, None, &[], NOW).decision,
            DecisionKind::RequireHuman
        );

        let capability = capability(&request);
        let result = evaluate(&request, &policy, Some(&capability), &[], NOW);
        assert_eq!(result.decision, DecisionKind::Allow);
        assert_eq!(result.authorization, Some(AuthorizationSource::Capability));
    }

    #[test]
    fn expired_exhausted_and_scope_mismatched_capabilities_fail_closed() {
        let request = request(ActionKind::Delete, RiskClass::Destructive, true);
        let policy = policy(vec![ActionKind::Delete]);

        let mut expired = capability(&request);
        expired.expires_at = NOW;
        assert_eq!(
            evaluate(&request, &policy, Some(&expired), &[], NOW).reason,
            "capability_expired"
        );

        let mut exhausted = capability(&request);
        exhausted.remaining_uses = 0;
        assert_eq!(
            evaluate(&request, &policy, Some(&exhausted), &[], NOW).reason,
            "capability_exhausted"
        );

        let mut wrong_target = capability(&request);
        wrong_target.target = "github:mcaz/other:issue/72".to_owned();
        assert_eq!(
            evaluate(&request, &policy, Some(&wrong_target), &[], NOW).reason,
            "capability_scope_mismatch"
        );
    }

    #[test]
    fn expired_understated_and_unqualified_requests_fail_closed() {
        let policy = policy(vec![ActionKind::Delete]);

        let mut expired = request(ActionKind::Delete, RiskClass::Destructive, true);
        expired.expires_at = NOW;
        assert_eq!(
            evaluate(&expired, &policy, None, &[], NOW).reason,
            "request_expired"
        );

        let understated = request(ActionKind::Delete, RiskClass::ReversibleWrite, true);
        assert_eq!(
            evaluate(&understated, &policy, None, &[], NOW).reason,
            "risk_understated"
        );

        let mut unqualified = request(ActionKind::Delete, RiskClass::Destructive, true);
        unqualified.target = "issue-72".to_owned();
        assert_eq!(
            evaluate(&unqualified, &policy, None, &[], NOW).reason,
            "target_not_fully_qualified"
        );
    }

    #[test]
    fn policy_and_capability_both_constrain_cost() {
        let mut purchase = request(ActionKind::Purchase, RiskClass::Billable, false);
        purchase.cost = Some(CostEnvelope {
            currency: "JPY".to_owned(),
            max_minor_units: 5_001,
        });
        let policy = policy(vec![ActionKind::Purchase]);
        assert_eq!(
            evaluate(&purchase, &policy, None, &[], NOW).reason,
            "policy_cost_cap_exceeded"
        );

        purchase.cost.as_mut().unwrap().max_minor_units = 4_000;
        let mut capability = capability(&purchase);
        capability.max_cost.as_mut().unwrap().max_minor_units = 3_000;
        assert_eq!(
            evaluate(&purchase, &policy, Some(&capability), &[], NOW).reason,
            "capability_cost_cap_exceeded"
        );
    }

    #[test]
    fn receipt_prevents_replay_even_when_capability_still_has_uses() {
        let request = request(ActionKind::Delete, RiskClass::Destructive, true);
        let policy = policy(vec![ActionKind::Delete]);
        let capability = capability(&request);
        let receipt = ExecutionReceipt {
            schema: ACTION_RECEIPT_SCHEMA.to_owned(),
            request_hash: request_hash(&request),
            idempotency_key: request.idempotency_key.clone(),
            capability_id: Some(capability.grant_id.clone()),
            outcome: ExecutionOutcome::Succeeded,
            external_reference: Some("github:request/123".to_owned()),
            executed_at: NOW - 1,
        };

        assert_eq!(
            evaluate(&request, &policy, Some(&capability), &[receipt], NOW).reason,
            "replay_detected"
        );
    }

    #[test]
    fn autonomous_note_removal_maps_to_a_target_bound_destructive_request() {
        let request = ActionRequest::for_note_removal(
            "ai:codex",
            "codex",
            "notes/example",
            "sha256:abc",
            NOW + 120,
            "重複ノートの整理",
        );

        assert_eq!(request.action, ActionKind::Delete);
        assert_eq!(request.risk, RiskClass::Destructive);
        assert_eq!(request.target, "kb.note:notes/example");
        assert!(request.idempotency_key.contains("sha256:abc"));
    }

    #[test]
    fn machine_readable_example_matches_the_rust_model() {
        let request: ActionRequest = serde_json::from_str(include_str!(
            "../../../schemas/examples/action-request.example.json"
        ))
        .unwrap();
        let result = evaluate(
            &request,
            &policy(vec![ActionKind::Update]),
            None,
            &[],
            1_800_000_000,
        );
        assert_eq!(result.decision, DecisionKind::Allow);
        assert_eq!(
            request_hash(&request),
            "sha256:13cd8a9f292351b620273a294bea706af75cd21dcc48eb2f93d973f81f6daf43"
        );
    }
}
