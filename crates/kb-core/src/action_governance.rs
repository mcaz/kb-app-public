//! 外部副作用をサービス非依存で判定する、純粋な policy evaluator。
//!
//! このモジュールは実行も権限発行も永続化もしない。外部 adapter が実行前に
//! [`evaluate`] を呼び、実行後に [`ExecutionReceipt`] を保存するための共通語彙を提供する。

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

pub const ACTION_REQUEST_SCHEMA: &str = "kb.action-request.v1";
pub const ACTION_CAPABILITY_SCHEMA: &str = "kb.action-capability.v1";
pub const ACTION_DECISION_SCHEMA: &str = "kb.action-decision.v1";
pub const ACTION_RECEIPT_SCHEMA: &str = "kb.action-receipt.v1";
pub const SIGNED_CAPABILITY_SCHEMA: &str = "kb.action-signed-capability.v1";

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
#[serde(deny_unknown_fields)]
pub struct CostEnvelope {
    /// ISO 4217 currency code。比較時は大文字小文字を区別しない。
    pub currency: String,
    /// 最小通貨単位（JPYなら円、USDならcent）で表す上限。
    pub max_minor_units: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct ActionPolicy {
    pub allowed_actions: Vec<ActionKind>,
    /// 完全修飾対象に対するprefix allowlist。空なら全対象を拒否する。
    pub target_prefixes: Vec<String>,
    pub allow_reversible_writes: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_cost: Option<CostEnvelope>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

/// secretを含まず、issuerと内容へHMAC-SHA-256で固定したlocal capability。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedCapabilityLease {
    pub schema: String,
    pub workspace: String,
    pub actor: String,
    pub client_surface: String,
    pub issuer: String,
    pub issued_at: i64,
    pub nonce: String,
    pub lease: CapabilityLease,
    pub signature: String,
}

#[derive(Serialize)]
struct CapabilitySignaturePayload<'a> {
    schema: &'a str,
    workspace: &'a str,
    actor: &'a str,
    client_surface: &'a str,
    issuer: &'a str,
    issued_at: i64,
    nonce: &'a str,
    lease: &'a CapabilityLease,
}

/// verifierへ明示的に渡したissuerだけを信頼する。鍵はcapabilityへserializeしない。
#[derive(Clone, Default)]
pub struct TrustedCapabilityIssuers {
    hmac_sha256_keys: BTreeMap<String, Vec<u8>>,
}

impl TrustedCapabilityIssuers {
    pub fn trust_hmac_sha256(&mut self, issuer: &str, key: &[u8]) -> Result<(), &'static str> {
        if issuer.trim().is_empty() {
            return Err("issuer must not be empty");
        }
        if key.len() < 32 {
            return Err("HMAC-SHA-256 key must be at least 32 bytes");
        }
        self.hmac_sha256_keys
            .insert(issuer.to_owned(), key.to_vec());
        Ok(())
    }

    pub fn verify(
        &self,
        signed: &SignedCapabilityLease,
        now: i64,
    ) -> Result<(), CapabilityVerificationFailure> {
        if signed.schema != SIGNED_CAPABILITY_SCHEMA {
            return Err(CapabilityVerificationFailure::UnsupportedSchema);
        }
        if signed.issuer.trim().is_empty() {
            return Err(CapabilityVerificationFailure::InvalidIssuer);
        }
        if signed.nonce.trim().is_empty() {
            return Err(CapabilityVerificationFailure::InvalidNonce);
        }
        if signed.workspace.trim().is_empty()
            || signed.actor.trim().is_empty()
            || signed.client_surface.trim().is_empty()
        {
            return Err(CapabilityVerificationFailure::InvalidScope);
        }
        if signed.issued_at > now {
            return Err(CapabilityVerificationFailure::IssuedInFuture);
        }
        if signed.lease.expires_at <= signed.issued_at || signed.lease.expires_at <= now {
            return Err(CapabilityVerificationFailure::Expired);
        }
        // exact request hashとidempotency keyへ固定したv1では、複数useはreplay契約と矛盾する。
        if signed.lease.remaining_uses != 1 {
            return Err(CapabilityVerificationFailure::NotSingleUse);
        }
        let Some(key) = self.hmac_sha256_keys.get(&signed.issuer) else {
            return Err(CapabilityVerificationFailure::UnknownIssuer);
        };
        let signature = decode_hmac_signature(&signed.signature)
            .ok_or(CapabilityVerificationFailure::InvalidSignature)?;
        let payload = capability_signature_bytes(signed);
        let expected = hmac_sha256(key, &payload);
        let difference = expected
            .iter()
            .zip(signature.iter())
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            });
        if difference == 0 {
            Ok(())
        } else {
            Err(CapabilityVerificationFailure::InvalidSignature)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityVerificationFailure {
    UnsupportedSchema,
    InvalidIssuer,
    UnknownIssuer,
    InvalidNonce,
    InvalidScope,
    IssuedInFuture,
    Expired,
    NotSingleUse,
    InvalidSignature,
}

impl CapabilityVerificationFailure {
    pub const fn reason(self) -> &'static str {
        match self {
            Self::UnsupportedSchema => "unsupported_signed_capability_schema",
            Self::InvalidIssuer => "invalid_capability_issuer",
            Self::UnknownIssuer => "unknown_capability_issuer",
            Self::InvalidNonce => "invalid_capability_nonce",
            Self::InvalidScope => "invalid_capability_scope",
            Self::IssuedInFuture => "capability_issued_in_future",
            Self::Expired => "capability_expired",
            Self::NotSingleUse => "capability_must_be_single_use",
            Self::InvalidSignature => "invalid_capability_signature",
        }
    }
}

pub fn sign_capability_hmac_sha256(
    lease: CapabilityLease,
    request: &ActionRequest,
    workspace: &str,
    issuer: &str,
    issued_at: i64,
    nonce: &str,
    key: &[u8],
) -> Result<SignedCapabilityLease, &'static str> {
    if workspace.trim().is_empty() {
        return Err("workspace must not be empty");
    }
    if request.actor.trim().is_empty() || request.client_surface.trim().is_empty() {
        return Err("request actor and client surface must not be empty");
    }
    if lease.request_hash != request_hash(request)
        || lease.action != request.action
        || lease.target != request.target
    {
        return Err("capability lease does not match request");
    }
    if issuer.trim().is_empty() {
        return Err("issuer must not be empty");
    }
    if nonce.trim().is_empty() {
        return Err("nonce must not be empty");
    }
    if key.len() < 32 {
        return Err("HMAC-SHA-256 key must be at least 32 bytes");
    }
    let mut signed = SignedCapabilityLease {
        schema: SIGNED_CAPABILITY_SCHEMA.to_owned(),
        workspace: workspace.to_owned(),
        actor: request.actor.clone(),
        client_surface: request.client_surface.clone(),
        issuer: issuer.to_owned(),
        issued_at,
        nonce: nonce.to_owned(),
        lease,
        signature: String::new(),
    };
    signed.signature =
        encode_hmac_signature(&hmac_sha256(key, &capability_signature_bytes(&signed)));
    Ok(signed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOutcome {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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

impl PolicyDecision {
    pub fn denied(request: &ActionRequest, reason: impl Into<String>) -> Self {
        Self {
            schema: ACTION_DECISION_SCHEMA.to_owned(),
            decision: DecisionKind::Deny,
            reason: reason.into(),
            request_hash: request_hash(request),
            authorization: None,
            capability_id: None,
        }
    }
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

fn capability_signature_bytes(signed: &SignedCapabilityLease) -> Vec<u8> {
    let payload = CapabilitySignaturePayload {
        schema: &signed.schema,
        workspace: &signed.workspace,
        actor: &signed.actor,
        client_surface: &signed.client_surface,
        issuer: &signed.issuer,
        issued_at: signed.issued_at,
        nonce: &signed.nonce,
        lease: &signed.lease,
    };
    let canonical =
        serde_json::to_value(payload).expect("capability payload serialization is infallible");
    serde_json::to_vec(&canonical).expect("capability payload serialization is infallible")
}

fn encode_hmac_signature(bytes: &[u8]) -> String {
    let mut signature = String::with_capacity(76);
    signature.push_str("hmac-sha256:");
    for byte in bytes {
        write!(&mut signature, "{byte:02x}").expect("writing to String is infallible");
    }
    signature
}

fn decode_hmac_signature(signature: &str) -> Option<[u8; 32]> {
    let hex = signature.strip_prefix("hmac-sha256:")?;
    if hex.len() != 64 {
        return None;
    }
    let mut decoded = [0_u8; 32];
    for (output, pair) in decoded.iter_mut().zip(hex.as_bytes().as_chunks::<2>().0) {
        *output = hex_value(pair[0])? * 16 + hex_value(pair[1])?;
    }
    Some(decoded)
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// RFC 2104 HMAC。追加crypto依存を持ち込まず、SHA-256の標準block sizeへ固定する。
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 64;
    let mut normalized = [0_u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        normalized[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36_u8; BLOCK_SIZE];
    let mut outer_pad = [0x5c_u8; BLOCK_SIZE];
    for ((inner, outer), key_byte) in inner_pad
        .iter_mut()
        .zip(outer_pad.iter_mut())
        .zip(normalized)
    {
        *inner ^= key_byte;
        *outer ^= key_byte;
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    outer.finalize().into()
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

    #[test]
    fn hmac_sha256_matches_rfc_4231_test_case_1() {
        let key = [0x0b_u8; 20];
        assert_eq!(
            encode_hmac_signature(&hmac_sha256(&key, b"Hi There")),
            "hmac-sha256:b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn signed_capability_serializes_bound_scope_and_rejects_a_mismatched_lease() {
        let request = request(ActionKind::Delete, RiskClass::Destructive, true);
        let lease = capability(&request);
        let signed = sign_capability_hmac_sha256(
            lease.clone(),
            &request,
            "workspace:test",
            "local:owner",
            NOW - 1,
            "nonce:test",
            b"0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        let document = serde_json::to_value(&signed).unwrap();
        assert_eq!(document["workspace"], "workspace:test");
        assert_eq!(document["actor"], request.actor);
        assert_eq!(document["client_surface"], request.client_surface);

        let mut mismatched = lease;
        mismatched.target = "github:mcaz/other:issue/72".to_owned();
        assert!(
            sign_capability_hmac_sha256(
                mismatched,
                &request,
                "workspace:test",
                "local:owner",
                NOW - 1,
                "nonce:test",
                b"0123456789abcdef0123456789abcdef",
            )
            .is_err()
        );
    }
}
