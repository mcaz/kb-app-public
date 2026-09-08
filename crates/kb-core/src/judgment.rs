//! 判断と行動の出典付き記録。数値の重要度や実行権限へ変換しない。
//!
//! 出典と本人由来の区分は記録者の申告であり、本人認証の証拠ではない。
//! 同じイベントの複製を実績の増加にしないため、referenceはイベント単位で固定する。

use std::collections::BTreeSet;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::authority::{
    Authority, AuthorityRole, NoteNamespace, NoteRelation, NoteUid, RelationKind,
};
use crate::write_rejection::WriteRejection;

// hookの小さい予算でも条件・例外を丸ごと判断できるよう、自由文の増殖を保存時に抑える。
pub(crate) const MAX_TEXT_CHARS: usize = 1_000;
pub(crate) const MAX_SOURCE_REFERENCE_CHARS: usize = 512;
pub(crate) const MAX_EXCEPTION_CHARS: usize = 500;
pub(crate) const MAX_EXCEPTIONS: usize = 8;
pub(crate) const MAX_DECISION_REFS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEvidence {
    /// 同じ元イベントを引用し直す場合は変えない。ノートIDやAIの再要約IDを使わない。
    pub reference: String,
    pub excerpt: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionBasis {
    UserDecision,
    UserCorrection,
    AssistantInference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionOutcome {
    Succeeded,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionEvidence {
    Observed,
    UserReport,
    AssistantReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserCorrection {
    pub source: SourceEvidence,
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Judgment {
    Decision {
        basis: DecisionBasis,
        source: SourceEvidence,
        applies_when: String,
        action: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        exceptions: Vec<String>,
    },
    Action {
        source: SourceEvidence,
        situation: String,
        action: String,
        outcome: ActionOutcome,
        evidence: ActionEvidence,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        decision_refs: Vec<NoteUid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        correction: Option<UserCorrection>,
    },
}

/// MCPの入力契約も保存時の上限と同じ値から生成する。
pub(crate) fn input_schema() -> serde_json::Value {
    use serde_json::json;
    let text = |limit| json!({"type":"string", "minLength":1, "maxLength":limit});
    let source = json!({
        "type":"object", "additionalProperties":false,
        "description":"記録者が申告した出典。本人認証や実行許可を証明しない。同じ元イベントのreferenceは再要約でも変えない。",
        "properties":{
            "reference":text(MAX_SOURCE_REFERENCE_CHARS),
            "excerpt":text(MAX_TEXT_CHARS)
        },
        "required":["reference", "excerpt"]
    });
    let correction = json!({
        "type":"object", "additionalProperties":false,
        "properties":{"source":&source, "action":text(MAX_TEXT_CHARS)},
        "required":["source", "action"]
    });
    json!({
        "oneOf":[
            {
                "type":"object", "additionalProperties":false,
                "properties":{
                    "kind":{"const":"decision", "type":"string"},
                    "basis":{"type":"string", "enum":["user_decision", "user_correction", "assistant_inference"]},
                    "source":&source,
                    "applies_when":text(MAX_TEXT_CHARS),
                    "action":text(MAX_TEXT_CHARS),
                    "exceptions":{"type":"array", "maxItems":MAX_EXCEPTIONS, "uniqueItems":true, "items":text(MAX_EXCEPTION_CHARS)}
                },
                "required":["kind", "basis", "source", "applies_when", "action"]
            },
            {
                "type":"object", "additionalProperties":false,
                "properties":{
                    "kind":{"const":"action", "type":"string"},
                    "source":source,
                    "situation":text(MAX_TEXT_CHARS),
                    "action":text(MAX_TEXT_CHARS),
                    "outcome":{"type":"string", "enum":["succeeded", "failed", "unknown"]},
                    "evidence":{"type":"string", "enum":["observed", "user_report", "assistant_report"]},
                    "decision_refs":{
                        "type":"array", "maxItems":MAX_DECISION_REFS, "uniqueItems":true,
                        "description":"採用した判断の既存note_uid。同じ対象へのsupportsまたはmentions relationも必要。",
                        "items":{"type":"string", "pattern":"^[0-9A-HJKMNP-TV-Z]{26}$"}
                    },
                    "correction":{"anyOf":[correction, {"type":"null"}]}
                },
                "required":["kind", "source", "situation", "action", "outcome", "evidence"]
            }
        ]
    })
}

pub(crate) fn validate(
    judgment: Option<&Judgment>,
    note_uid: Option<&NoteUid>,
    authority: Option<&Authority>,
    relations: &[NoteRelation],
) -> Result<()> {
    let Some(judgment) = judgment else {
        return Ok(());
    };
    let Some(authority) = authority.filter(|_| note_uid.is_some()) else {
        return invalid("judgmentにはnote_uidとauthorityが必要");
    };
    let is_record =
        authority.namespace == NoteNamespace::Records && authority.role == AuthorityRole::Record;
    match judgment {
        Judgment::Decision {
            source,
            applies_when,
            action,
            exceptions,
            ..
        } => {
            // 過去の判断を失わずにauthorityの履歴化・後継移行ができるよう、statusは制限しない。
            // 現行候補としての利用可否は読出側がactiveと適用条件を検査する。
            if !is_record
                && !(matches!(
                    authority.namespace,
                    NoteNamespace::Decisions | NoteNamespace::Procedures
                ) && authority.role == AuthorityRole::Canonical)
            {
                return invalid(
                    "decisionはrecordsのrecordまたはdecisions/proceduresのcanonicalに記録する",
                );
            }
            validate_source(source)?;
            validate_text("applies_when", applies_when, MAX_TEXT_CHARS)?;
            validate_text("action", action, MAX_TEXT_CHARS)?;
            if exceptions.len() > MAX_EXCEPTIONS {
                return invalid("exceptionsは8件以内にする");
            }
            let mut unique = BTreeSet::new();
            for exception in exceptions {
                validate_text("exception", exception, MAX_EXCEPTION_CHARS)?;
                if !unique.insert(exception.trim()) {
                    return invalid("exceptionsを重複させない");
                }
            }
        }
        Judgment::Action {
            source,
            situation,
            action,
            decision_refs,
            correction,
            ..
        } => {
            if !is_record {
                return invalid("actionはrecordsのrecordに記録する");
            }
            validate_source(source)?;
            validate_text("situation", situation, MAX_TEXT_CHARS)?;
            validate_text("action", action, MAX_TEXT_CHARS)?;
            if decision_refs.len() > MAX_DECISION_REFS {
                return invalid("decision_refsは16件以内にする");
            }
            let mut unique = BTreeSet::new();
            for target in decision_refs {
                if !unique.insert(target) {
                    return invalid("decision_refsを重複させない");
                }
                // 参照を別台帳へ分岐させず、既存の存在検証・削除保護と同じedgeへ束ねる。
                if !relations.iter().any(|relation| {
                    relation.target == *target
                        && matches!(
                            relation.kind,
                            RelationKind::Supports | RelationKind::Mentions
                        )
                }) {
                    return invalid(
                        "decision_refsには同じ対象へのsupportsまたはmentions relationが必要",
                    );
                }
            }
            if let Some(correction) = correction {
                validate_source(&correction.source)?;
                validate_text("correction.action", &correction.action, MAX_TEXT_CHARS)?;
            }
        }
    }
    Ok(())
}

fn validate_source(source: &SourceEvidence) -> Result<()> {
    validate_text(
        "source.reference",
        &source.reference,
        MAX_SOURCE_REFERENCE_CHARS,
    )?;
    if source.reference.trim() != source.reference || source.reference.chars().any(char::is_control)
    {
        return invalid("source.referenceは前後空白・制御文字を含めない");
    }
    validate_text("source.excerpt", &source.excerpt, MAX_TEXT_CHARS)
}

fn validate_text(field: &str, value: &str, limit: usize) -> Result<()> {
    if value.trim().is_empty()
        || value.chars().count() > limit
        || value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return invalid(&format!(
            "judgmentの{field}は空白だけでない1〜{limit}文字にする"
        ));
    }
    Ok(())
}

fn invalid(detail: &str) -> Result<()> {
    Err(WriteRejection::InvalidArgument.validation(detail))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::AuthorityStatus;

    fn record() -> Authority {
        Authority {
            namespace: NoteNamespace::Records,
            role: AuthorityRole::Record,
            status: AuthorityStatus::Active,
            scope: "fixture/deployment".into(),
        }
    }

    fn decision() -> Judgment {
        serde_json::from_value(serde_json::json!({
            "kind": "decision", "basis": "user_decision",
            "source": {"reference": "conversation:fixture/turn-1", "excerpt": "実行は本人が行う"},
            "applies_when": "アプリを反映するとき", "action": "検証したコマンドを提示する"
        }))
        .unwrap()
    }

    #[test]
    fn judgment_requires_attributed_shape_and_preserves_historical_decisions() {
        let uid = NoteUid::new();
        let mut authority = record();
        let judgment = decision();
        assert!(validate(Some(&judgment), None, None, &[]).is_err());
        assert!(validate(Some(&judgment), Some(&uid), Some(&authority), &[]).is_ok());
        authority.status = AuthorityStatus::Historical;
        assert!(validate(Some(&judgment), Some(&uid), Some(&authority), &[]).is_ok());
        authority.namespace = NoteNamespace::Knowledge;
        authority.role = AuthorityRole::Canonical;
        assert!(validate(Some(&judgment), Some(&uid), Some(&authority), &[]).is_err());
        let mut raw = serde_json::to_value(judgment).unwrap();
        raw["weight"] = serde_json::json!(100);
        assert!(serde_json::from_value::<Judgment>(raw).is_err());
    }

    #[test]
    fn action_references_require_existing_graph_edges_and_are_not_counted_twice() {
        let me = NoteUid::new();
        let target = NoteUid::new();
        let mut judgment = Judgment::Action {
            source: SourceEvidence {
                reference: "execution:fixture/event-1".into(),
                excerpt: "書込前に拒否された".into(),
            },
            situation: "アプリ反映の依頼".into(),
            action: "反映コマンドを実行した".into(),
            outcome: ActionOutcome::Failed,
            evidence: ActionEvidence::Observed,
            decision_refs: vec![target.clone()],
            correction: None,
        };
        assert!(validate(Some(&judgment), Some(&me), Some(&record()), &[]).is_err());
        let relations = [NoteRelation {
            kind: RelationKind::Mentions,
            target: target.clone(),
        }];
        assert!(validate(Some(&judgment), Some(&me), Some(&record()), &relations).is_ok());
        if let Judgment::Action { decision_refs, .. } = &mut judgment {
            decision_refs.push(target);
        }
        assert!(validate(Some(&judgment), Some(&me), Some(&record()), &relations).is_err());
    }

    #[test]
    fn empty_sources_and_unbounded_conditions_are_rejected() {
        let uid = NoteUid::new();
        for (field, value) in [
            ("reference", " ".to_string()),
            ("excerpt", "x".repeat(MAX_TEXT_CHARS + 1)),
        ] {
            let mut judgment = decision();
            if let Judgment::Decision { source, .. } = &mut judgment {
                match field {
                    "reference" => source.reference = value,
                    _ => source.excerpt = value,
                }
            }
            assert!(validate(Some(&judgment), Some(&uid), Some(&record()), &[]).is_err());
        }
        let mut judgment = decision();
        if let Judgment::Decision { exceptions, .. } = &mut judgment {
            *exceptions = vec!["同じ例外".into(); 2];
        }
        assert!(validate(Some(&judgment), Some(&uid), Some(&record()), &[]).is_err());
    }
}
