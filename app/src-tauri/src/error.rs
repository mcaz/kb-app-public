//! GUI に返すエラー。
//!
//! これまでは `Result<T, String>` で日本語の文言をそのまま返していたため、
//! 画面側は受け取った文字列を出すことしかできず、英語表示にすると
//! **この経路だけ日本語が混じっていた**(ADR-0002 の既知の限界)。
//!
//! ここで種類を型にすると specta が TS 側へ判別可能な union として書き出すので、
//! 画面は `code` で訳し分けられる。
//!
//! **限界**: kb-core は anyhow を使っており内部のエラーは文字列のままなので、
//! 分類できるのは Tauri 層が文脈を知っている場合に限られる。それ以外は
//! `Unexpected` に落ち、コアの日本語文言をそのまま運ぶ。コア側を型付きエラーに
//! するのは別の作業(ADR-0002 の残課題)。

use serde::Serialize;

#[derive(Debug, Serialize, specta::Type)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum AppError {
    /// vault を開けない(未オンボーディング・レジストリの不整合)。
    VaultUnavailable { message: String },
    /// 指定 ID のノートが無い(消された・まだ書かれていない)。
    NoteNotFound { id: String },
    /// 添付が上限を超えている。
    AttachmentTooLarge { limit_mb: u32, actual_mb: u32 },
    /// Claude Desktop が見つからない(未インストール)。
    ClaudeDesktopNotFound,
    /// Claude Desktop を起動できなかった。
    ClaudeDesktopLaunchFailed,
    /// バックアップ(git)の失敗。
    BackupFailed { message: String },
    /// かしこい検索の準備に失敗。
    EmbedFailed { message: String },
    /// 分類できないもの。message はコアが返した文言(いまは日本語)。
    Unexpected { message: String },
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VaultUnavailable { message } | Self::Unexpected { message } => {
                write!(f, "{message}")
            }
            Self::NoteNotFound { id } => write!(f, "ノートが見つからない: {id}"),
            Self::AttachmentTooLarge {
                limit_mb,
                actual_mb,
            } => write!(f, "添付が上限を超えている({actual_mb}MB > {limit_mb}MB)"),
            Self::ClaudeDesktopNotFound => write!(f, "Claude Desktop が見つからない"),
            Self::ClaudeDesktopLaunchFailed => write!(f, "Claude Desktop を起動できなかった"),
            Self::BackupFailed { message } => write!(f, "バックアップに失敗: {message}"),
            Self::EmbedFailed { message } => write!(f, "かしこい検索の準備に失敗: {message}"),
        }
    }
}

impl std::error::Error for AppError {}

/// コア(anyhow)のエラーは分類できないので Unexpected に落とす。
impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        Self::Unexpected {
            message: e.to_string(),
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
}

pub type AppResult<T> = Result<T, AppError>;
