//! バックアップ・復元の失敗分類。
//!
//! Git / Git LFS / GitHub API は文字列や HTTP status で失敗を返すが、画面がその文言を
//! 直接表示すると認証切れ・quota・remote 欠損を区別できない。境界で一度この型へ寄せ、
//! Tauri と同期 sidecar へ同じ分類を運ぶ。

use anyhow::Error;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum BackupFailureKind {
    Authentication,
    Permission,
    PrivacyCheck,
    Network,
    InvalidRepository,
    RemoteMissing,
    Quota,
    LfsUnavailable,
    RemoteObjectMissing,
    IntegrityMismatch,
    InvalidVault,
    WorkspaceMismatch,
    DestinationExists,
    Commit,
    LfsUpload,
    GitPush,
    GitPull,
    GitConflict,
}

#[derive(Debug)]
pub struct BackupFailure {
    pub kind: BackupFailureKind,
    message: String,
}

impl std::fmt::Display for BackupFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for BackupFailure {}

pub(crate) fn failure(kind: BackupFailureKind, message: impl Into<String>) -> Error {
    BackupFailure {
        kind,
        message: message.into(),
    }
    .into()
}

pub fn failure_kind(error: &Error) -> Option<BackupFailureKind> {
    error.chain().find_map(|cause| {
        cause
            .downcast_ref::<BackupFailure>()
            .map(|failure| failure.kind)
    })
}

/// Git/Git LFS の stderr は安定した machine-readable code を持たないため、既知の
/// provider 文言を狭く分類する。該当しない場合は呼び出し操作の分類へ戻す。
pub(crate) fn classify_git_failure(stderr: &str, fallback: BackupFailureKind) -> BackupFailureKind {
    let message = stderr.to_ascii_lowercase();
    if contains_any(
        &message,
        &[
            "authentication failed",
            "could not read username",
            "terminal prompts disabled",
            "permission denied (publickey)",
            "invalid username or password",
            "bad credentials",
        ],
    ) {
        BackupFailureKind::Authentication
    } else if contains_any(
        &message,
        &[
            "repository not found",
            "does not appear to be a git repository",
            "remote repository not found",
        ],
    ) {
        BackupFailureKind::RemoteMissing
    } else if contains_any(
        &message,
        &[
            "exceeded its data quota",
            "quota exceeded",
            "data packs",
            "bandwidth quota",
            "storage quota",
        ],
    ) {
        BackupFailureKind::Quota
    } else if contains_any(
        &message,
        &["'lfs' is not a git command", "git-lfs: command not found"],
    ) {
        BackupFailureKind::LfsUnavailable
    } else if contains_any(
        &message,
        &[
            "object does not exist on the server",
            "object not found on the server",
            "missing object",
        ],
    ) {
        BackupFailureKind::RemoteObjectMissing
    } else if contains_any(
        &message,
        &[
            "could not resolve host",
            "failed to connect",
            "connection timed out",
            "network is unreachable",
            "connection reset",
            "temporary failure in name resolution",
        ],
    ) {
        BackupFailureKind::Network
    } else if contains_any(
        &message,
        &[
            "permission to ",
            "write access to repository not granted",
            "the requested url returned error: 403",
        ],
    ) {
        BackupFailureKind::Permission
    } else if contains_any(
        &message,
        &[
            "non-fast-forward",
            "(fetch first)",
            "failed to push some refs",
        ],
    ) {
        BackupFailureKind::GitConflict
    } else {
        fallback
    }
}

pub(crate) fn git_failure(operation: &str, stderr: &str, fallback: BackupFailureKind) -> Error {
    let kind = classify_git_failure(stderr, fallback);
    failure(kind, format!("{operation}: {}", stderr.trim()))
}

fn contains_any(message: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| message.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_failures_are_classified_before_falling_back_to_the_operation() {
        assert_eq!(
            classify_git_failure(
                "remote: This repository is over its data quota. Purchase more data packs",
                BackupFailureKind::LfsUpload,
            ),
            BackupFailureKind::Quota
        );
        assert_eq!(
            classify_git_failure(
                "fatal: Authentication failed for 'https://github.com/a/b.git/'",
                BackupFailureKind::GitPull,
            ),
            BackupFailureKind::Authentication
        );
        assert_eq!(
            classify_git_failure("remote: Repository not found.", BackupFailureKind::GitPush,),
            BackupFailureKind::RemoteMissing
        );
        assert_eq!(
            classify_git_failure(
                "fatal: unable to access: Could not resolve host: github.com",
                BackupFailureKind::GitPull,
            ),
            BackupFailureKind::Network
        );
        assert_eq!(
            classify_git_failure(
                "git: 'lfs' is not a git command. See 'git --help'.",
                BackupFailureKind::LfsUpload,
            ),
            BackupFailureKind::LfsUnavailable
        );
        assert_eq!(
            classify_git_failure("non-fast-forward", BackupFailureKind::GitConflict),
            BackupFailureKind::GitConflict
        );
        assert_eq!(
            classify_git_failure(
                "[rejected] main -> main (fetch first)",
                BackupFailureKind::GitPush
            ),
            BackupFailureKind::GitConflict
        );
    }

    #[test]
    fn kind_survives_anyhow_context() {
        let error = failure(BackupFailureKind::Quota, "quota").context("Full Artifact を送れない");
        assert_eq!(failure_kind(&error), Some(BackupFailureKind::Quota));
    }
}
