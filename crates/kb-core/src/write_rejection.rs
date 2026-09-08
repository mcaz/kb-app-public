//! 既存の書込拒否を、診断文に依存しない安定codeへ分類する。
//!
//! validationは保存済みノートのparse/exportでも走る。検証エラーだけで未保存と
//! 断定せず、commit前と確定できる境界でだけ拒否マーカーへ昇格する。

use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};

/// UNIQUE拒否を判定したtransaction内で固定した競合相手。後から再検索して推測しない。
#[derive(Debug, Serialize)]
pub(crate) struct ScopeConflict {
    pub namespace: String,
    pub scope: String,
    pub note_id: String,
    pub note_uid: Option<String>,
    pub title: Option<String>,
}

impl fmt::Display for ScopeConflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "同じnamespace/scopeのactive canonicalが既にある: {} ({} / {})",
            self.note_id, self.namespace, self.scope
        )
    }
}

impl Error for ScopeConflict {}

pub(crate) fn scope_conflict(error: &anyhow::Error) -> Option<&ScopeConflict> {
    // RejectedWriteはanyhowのcontextではなく独自のError境界なので、内側を明示して読む。
    error.downcast_ref::<RejectedWrite>()?.error.downcast_ref()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum WriteRejection {
    TagCount,
    TagShape,
    TagVocabulary,
    AuthorityScope,
    AuthorityShape,
    RelationIntegrity,
    ActiveCanonicalConflict,
    LegacyReadOnly,
    MissingArgument,
    InvalidArgument,
    McpCapability,
}

impl WriteRejection {
    pub fn from_error(error: &anyhow::Error) -> Option<Self> {
        error
            .downcast_ref::<RejectedWrite>()
            .map(|value| value.code)
    }

    pub(crate) fn validation(self, detail: impl Into<String>) -> anyhow::Error {
        ValidationFailure {
            code: self,
            detail: detail.into(),
        }
        .into()
    }

    pub(crate) fn reject(self, detail: impl Into<String>) -> anyhow::Error {
        confirm_before_write(self.validation(detail))
    }
}

#[derive(Debug)]
struct ValidationFailure {
    code: WriteRejection,
    detail: String,
}

impl fmt::Display for ValidationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl Error for ValidationFailure {}

#[derive(Debug)]
struct RejectedWrite {
    code: WriteRejection,
    error: anyhow::Error,
}

impl fmt::Display for RejectedWrite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.error, formatter)
    }
}

impl Error for RejectedWrite {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.error.as_ref())
    }
}

pub(crate) fn confirm_before_write(error: anyhow::Error) -> anyhow::Error {
    match error.downcast_ref::<ValidationFailure>() {
        Some(failure) => RejectedWrite {
            code: failure.code,
            error,
        }
        .into(),
        None => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-09-05: 同じ検証が保存後にも動くため、文言や原因型だけで未保存を主張しない。
    #[test]
    fn only_confirmed_prewrite_validation_gets_a_rejection_code() {
        let validation = WriteRejection::AuthorityScope.validation("scopeの診断");
        assert_eq!(WriteRejection::from_error(&validation), None);
        let rejection = confirm_before_write(validation.context("入力検証"));
        assert_eq!(
            WriteRejection::from_error(&rejection.context("呼出元")),
            Some(WriteRejection::AuthorityScope)
        );
        let unrelated = confirm_before_write(anyhow::anyhow!("scopeの診断"));
        assert_eq!(WriteRejection::from_error(&unrelated), None);
    }

    #[test]
    fn rejection_codes_serialize_without_diagnostics_and_reject_unknown_codes() {
        let error = WriteRejection::TagVocabulary.reject("秘密のノート名・タグ候補");
        let code = WriteRejection::from_error(&error).unwrap();
        assert_eq!(serde_json::to_string(&code).unwrap(), "\"tag_vocabulary\"");
        assert!(serde_json::from_str::<WriteRejection>("\"future_code\"").is_err());
    }
}
