//! kb-core の公開境界で使う型付きエラー。
//!
//! 詳細文は CLI / MCP の診断には必要だが、GUI がそのまま表示すると言語を
//! 切り替えられない。`CoreError` は診断用の source を保持し、Tauri へは
//! `CoreErrorKind` だけを渡せるようにする。

use crate::artifact::ArtifactError;
use crate::backup::{self, BackupFailureKind};
use serde::Serialize;

/// 画面が次の行動を訳し分けるための安定した分類。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum CoreErrorKind {
    VaultUnavailable,
    InvalidInput,
    Storage,
    Index,
    Configuration,
    Embedding,
    Unexpected,
}

impl CoreErrorKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::VaultUnavailable => "vault_unavailable",
            Self::InvalidInput => "invalid_input",
            Self::Storage => "storage",
            Self::Index => "index",
            Self::Configuration => "configuration",
            Self::Embedding => "embedding",
            Self::Unexpected => "unexpected",
        }
    }
}

/// 診断詳細を境界の外へ漏らさず、意味だけを型として運ぶ。
#[derive(Debug)]
pub enum CoreError {
    Operation {
        kind: CoreErrorKind,
        source: anyhow::Error,
    },
    NoteNotFound {
        id: String,
    },
    Backup {
        kind: Option<BackupFailureKind>,
        source: anyhow::Error,
    },
    Artifact(ArtifactError),
}

impl CoreError {
    pub fn operation(kind: CoreErrorKind, error: impl Into<anyhow::Error>) -> Self {
        let source = error.into();
        if let Some(artifact) = source.downcast_ref::<ArtifactError>() {
            return Self::Artifact(artifact.clone());
        }
        Self::Operation { kind, source }
    }

    pub fn vault(error: impl Into<anyhow::Error>) -> Self {
        Self::operation(CoreErrorKind::VaultUnavailable, error)
    }

    pub fn invalid_input(error: impl Into<anyhow::Error>) -> Self {
        Self::operation(CoreErrorKind::InvalidInput, error)
    }

    pub fn storage(error: impl Into<anyhow::Error>) -> Self {
        Self::operation(CoreErrorKind::Storage, error)
    }

    pub fn index(error: impl Into<anyhow::Error>) -> Self {
        Self::operation(CoreErrorKind::Index, error)
    }

    pub fn configuration(error: impl Into<anyhow::Error>) -> Self {
        Self::operation(CoreErrorKind::Configuration, error)
    }

    pub fn embedding(error: impl Into<anyhow::Error>) -> Self {
        Self::operation(CoreErrorKind::Embedding, error)
    }

    pub fn unexpected(error: impl Into<anyhow::Error>) -> Self {
        Self::operation(CoreErrorKind::Unexpected, error)
    }

    pub fn note_not_found(id: impl Into<String>) -> Self {
        Self::NoteNotFound { id: id.into() }
    }

    pub fn backup(error: impl Into<anyhow::Error>) -> Self {
        let source = error.into();
        Self::Backup {
            kind: backup::failure_kind(&source),
            source,
        }
    }

    pub const fn kind(&self) -> Option<CoreErrorKind> {
        match self {
            Self::Operation { kind, .. } => Some(*kind),
            Self::NoteNotFound { .. } | Self::Backup { .. } | Self::Artifact(_) => None,
        }
    }

    pub fn detail(&self) -> String {
        match self {
            Self::Operation { source, .. } | Self::Backup { source, .. } => source.to_string(),
            Self::NoteNotFound { id } => format!("note not found: {id}"),
            Self::Artifact(error) => error.to_string(),
        }
    }
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Operation { kind, source } => write!(f, "{}: {source}", kind.code()),
            Self::NoteNotFound { id } => write!(f, "note not found: {id}"),
            Self::Backup { kind, source } => write!(f, "backup {kind:?}: {source}"),
            Self::Artifact(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for CoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Operation { source, .. } | Self::Backup { source, .. } => Some(source.as_ref()),
            Self::Artifact(error) => Some(error),
            Self::NoteNotFound { .. } => None,
        }
    }
}

impl From<ArtifactError> for CoreError {
    fn from(error: ArtifactError) -> Self {
        Self::Artifact(error)
    }
}

pub type Result<T> = std::result::Result<T, CoreError>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::ArtifactError;
    use crate::backup::BackupFailureKind;

    #[test]
    fn kind_codes_are_stable() {
        assert_eq!(CoreErrorKind::VaultUnavailable.code(), "vault_unavailable");
        assert_eq!(CoreErrorKind::InvalidInput.code(), "invalid_input");
        assert_eq!(CoreErrorKind::Storage.code(), "storage");
        assert_eq!(CoreErrorKind::Index.code(), "index");
        assert_eq!(CoreErrorKind::Configuration.code(), "configuration");
        assert_eq!(CoreErrorKind::Embedding.code(), "embedding");
        assert_eq!(CoreErrorKind::Unexpected.code(), "unexpected");
    }

    #[test]
    fn artifact_error_survives_operation_wrapping() {
        let error = CoreError::storage(ArtifactError::TooLarge { size: 2, limit: 1 });
        assert!(matches!(
            error,
            CoreError::Artifact(ArtifactError::TooLarge { size: 2, limit: 1 })
        ));
    }

    #[test]
    fn backup_kind_survives_core_boundary() {
        let error = CoreError::backup(backup::failure(
            BackupFailureKind::Authentication,
            "token expired",
        ));
        assert!(matches!(
            error,
            CoreError::Backup {
                kind: Some(BackupFailureKind::Authentication),
                ..
            }
        ));
    }
}
