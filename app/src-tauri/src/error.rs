//! GUI に返すエラー。
//!
//! これまでは `Result<T, String>` で日本語の文言をそのまま返していたため、
//! 画面側は受け取った文字列を出すことしかできず、英語表示にすると
//! **この経路だけ日本語が混じっていた**(ADR-0002 の既知の限界)。
//!
//! ここで種類を型にすると specta が TS 側へ判別可能な union として書き出すので、
//! 画面は `code` で訳し分けられる。
//!
//! `CoreError` は診断詳細をコア側に保持し、この境界では安定した kind だけへ
//! 変換する。`From<anyhow::Error>` は意図的に実装しない。新しいコア呼び出しを
//! 分類せず追加するとコンパイルで止まり、内部文言が画面へ漏れない。

use kb_core::artifact::ArtifactError;
use kb_core::error::{CoreError, CoreErrorKind};
use serde::Serialize;

#[derive(Debug, Serialize, specta::Type)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum AppError {
    /// vault を開けない(未オンボーディング・レジストリの不整合)。
    VaultUnavailable,
    /// 指定 ID のノートが無い(消された・まだ書かれていない)。
    NoteNotFound { id: String },
    /// 「本体も同期」の上限を超えている。quota 不足とは別物(ADR-0003 決定8)。
    FileTooLarge { size: u64, limit: u64 },
    /// 別の場所で更新された。**自動再試行も強制上書きもしない**(決定7)。
    FileConflict { expected: u64, current: u64 },
    /// 端末固有の場所は指せない(他の端末から辿れないため)。
    FileLocationUnstable,
    /// 仕事のリポジトリ由来なので、この変更は認めない。
    FileClientRepoLocked,
    /// 持ち出しを広げる変更なので、確認を経ていない限り通さない(決定10)。
    FileNeedsConfirm,
    /// 識別子・参照名の形が不正。
    FileMalformed { field: String },
    /// 手元に無い(または方針で閉じている)ので開けない。
    FileNotHere,
    /// クリップボード画像が大きすぎる。**ファイルの上限ではない** —
    /// この経路だけ streaming できず全量がメモリに載るため(決定8)。
    ClipboardImageTooLarge,
    /// Claude Desktop が見つからない(未インストール)。
    ClaudeDesktopNotFound,
    /// Claude Desktop を起動できなかった。
    ClaudeDesktopLaunchFailed,
    /// バックアップ・復元の失敗。既知の理由は画面が翻訳して次の行動を案内する。
    BackupFailed {
        kind: Option<kb_core::backup::BackupFailureKind>,
    },
    /// かしこい検索の準備に失敗。
    EmbedFailed,
    /// コアの失敗。診断詳細は画面へ運ばず、kindだけを翻訳する。
    CoreFailed { kind: CoreErrorKind },
    /// Tauri / OS 層で分類できないもの。画面はmessageを表示せずログだけに使う。
    Unexpected { message: String },
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VaultUnavailable => write!(f, "vault unavailable"),
            Self::Unexpected { message } => write!(f, "{message}"),
            Self::NoteNotFound { id } => write!(f, "ノートが見つからない: {id}"),
            Self::FileTooLarge { size, limit } => write!(f, "大きすぎる({size} > {limit})"),
            Self::FileConflict { expected, current } => {
                write!(f, "別の場所で更新された(手元 {expected} / 最新 {current})")
            }
            Self::FileLocationUnstable => {
                write!(f, "この場所は端末固有で、他の端末から辿れない")
            }
            Self::FileClientRepoLocked => write!(f, "仕事のリポジトリ由来なので変更できない"),
            Self::FileNeedsConfirm => write!(f, "持ち出しを広げる変更には明示確認が要る"),
            Self::FileMalformed { field } => write!(f, "形式が不正: {field}"),
            Self::FileNotHere => write!(f, "この端末にファイルが無い"),
            Self::ClipboardImageTooLarge => write!(f, "クリップボードの画像が大きすぎる"),
            Self::ClaudeDesktopNotFound => write!(f, "Claude Desktop が見つからない"),
            Self::ClaudeDesktopLaunchFailed => write!(f, "Claude Desktop を起動できなかった"),
            Self::BackupFailed { kind } => write!(f, "backup failed: {kind:?}"),
            Self::EmbedFailed => write!(f, "embedding failed"),
            Self::CoreFailed { kind } => write!(f, "core failed: {}", kind.code()),
        }
    }
}

impl std::error::Error for AppError {}

impl From<CoreError> for AppError {
    fn from(error: CoreError) -> Self {
        match error {
            CoreError::Operation { kind, .. } => match kind {
                CoreErrorKind::VaultUnavailable => Self::VaultUnavailable,
                CoreErrorKind::Embedding => Self::EmbedFailed,
                kind => Self::CoreFailed { kind },
            },
            CoreError::NoteNotFound { id } => Self::NoteNotFound { id },
            CoreError::Backup { kind, .. } => Self::BackupFailed { kind },
            CoreError::Artifact(error) => error.into(),
        }
    }
}

impl From<ArtifactError> for AppError {
    fn from(e: ArtifactError) -> Self {
        match e {
            ArtifactError::TooLarge { size, limit } => Self::FileTooLarge { size, limit },
            ArtifactError::Conflict { expected, current } => {
                Self::FileConflict { expected, current }
            }
            ArtifactError::UnstableLocator => Self::FileLocationUnstable,
            ArtifactError::ClientRepoLocked => Self::FileClientRepoLocked,
            ArtifactError::RelaxationNeedsConfirm => Self::FileNeedsConfirm,
            ArtifactError::Malformed { field } => Self::FileMalformed {
                field: field.to_string(),
            },
        }
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        Self::Unexpected {
            message: e.to_string(),
        }
    }
}

impl AppError {
    /// 文脈が分かっている箇所で明示的に包むための補助。
    pub fn unexpected(e: impl std::fmt::Display) -> Self {
        Self::Unexpected {
            message: e.to_string(),
        }
    }

    pub fn vault(error: impl Into<anyhow::Error>) -> Self {
        CoreError::vault(error).into()
    }

    pub fn invalid_input(error: impl Into<anyhow::Error>) -> Self {
        CoreError::invalid_input(error).into()
    }

    pub fn note_not_found(id: impl Into<String>) -> Self {
        CoreError::note_not_found(id).into()
    }

    pub fn storage(error: impl Into<anyhow::Error>) -> Self {
        CoreError::storage(error).into()
    }

    pub fn index(error: impl Into<anyhow::Error>) -> Self {
        CoreError::index(error).into()
    }

    pub fn configuration(error: impl Into<anyhow::Error>) -> Self {
        CoreError::configuration(error).into()
    }

    pub fn embed(error: impl Into<anyhow::Error>) -> Self {
        CoreError::embedding(error).into()
    }

    pub fn backup(error: impl Into<anyhow::Error>) -> Self {
        CoreError::backup(error).into()
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_reason_reaches_the_serialized_app_error() {
        let error =
            kb_core::github::parse_repository_url("https://example.com/not-github").unwrap_err();
        match AppError::backup(error) {
            AppError::BackupFailed { kind } => assert_eq!(
                kind,
                Some(kb_core::backup::BackupFailureKind::InvalidRepository)
            ),
            other => panic!("別のエラーへ変換された: {other}"),
        }
    }

    /// 2026-08-16まではcoreの日本語detailがUnexpected.messageとして英語UIにも届いた。
    #[test]
    fn core_diagnostic_detail_never_reaches_serialized_app_error() {
        let error = AppError::from(CoreError::index(anyhow::anyhow!(
            "画面へ出してはいけない診断"
        )));
        let json = serde_json::to_string(&error).unwrap();
        assert_eq!(json, r#"{"code":"core_failed","kind":"index"}"#);
        assert!(!json.contains("画面へ出してはいけない診断"));
    }
}
