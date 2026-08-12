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

use kb_core::artifact::ArtifactError;
use serde::Serialize;

#[derive(Debug, Serialize, specta::Type)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum AppError {
    /// vault を開けない(未オンボーディング・レジストリの不整合)。
    VaultUnavailable { message: String },
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
    /// クリップボード画像が大きすぎる。**ファイルの上限ではない** —
    /// この経路だけ streaming できず全量がメモリに載るため(決定8)。
    ClipboardImageTooLarge,
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
            Self::ClipboardImageTooLarge => write!(f, "クリップボードの画像が大きすぎる"),
            Self::ClaudeDesktopNotFound => write!(f, "Claude Desktop が見つからない"),
            Self::ClaudeDesktopLaunchFailed => write!(f, "Claude Desktop を起動できなかった"),
            Self::BackupFailed { message } => write!(f, "バックアップに失敗: {message}"),
            Self::EmbedFailed { message } => write!(f, "かしこい検索の準備に失敗: {message}"),
        }
    }
}

impl std::error::Error for AppError {}

/// コア(anyhow)のエラーは基本的に分類できないので Unexpected に落とす。
/// ただし Artifact 層だけは型付きなので、包まれていても取り出して訳せるようにする。
impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        match e.downcast_ref::<ArtifactError>() {
            Some(artifact) => artifact.clone().into(),
            None => Self::Unexpected {
                message: e.to_string(),
            },
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
}

pub type AppResult<T> = Result<T, AppError>;
