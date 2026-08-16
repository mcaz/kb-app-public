//! ノートIDの型付き境界。
//!
//! IDは「Vaultからの相対パス（`.md`抜き）」だが、裸の文字列を`PathBuf::join`へ
//! 渡すと絶対パス・`..`・OSごとの区切り差でVault外へ出られる。ここで一度だけ
//! 構文を確定し、ファイルパスへ変えられる値を`NoteId`に限定する。

use std::fmt;
use std::path::{Component, Path, PathBuf};

/// 検証済みのVault相対ノートID。
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NoteId(String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoteIdError {
    reason: &'static str,
}

impl NoteIdError {
    const fn new(reason: &'static str) -> Self {
        Self { reason }
    }
}

impl fmt::Display for NoteIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ノートIDが不正: {}", self.reason)
    }
}

impl std::error::Error for NoteIdError {}

impl NoteId {
    pub fn parse(raw: &str) -> Result<Self, NoteIdError> {
        if raw.is_empty() || raw.trim() != raw {
            return Err(NoteIdError::new("空または前後に空白がある"));
        }
        if raw.contains('\\') {
            return Err(NoteIdError::new("backslashは区切りとして使えない"));
        }
        if raw.contains(':') || raw.chars().any(char::is_control) {
            return Err(NoteIdError::new("OS依存または制御文字を含む"));
        }

        let path = Path::new(raw);
        if path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(NoteIdError::new("Vault相対の通常成分だけを使う"));
        }

        let parts: Vec<&str> = raw.split('/').collect();
        if parts
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == "..")
        {
            return Err(NoteIdError::new("空成分・`.`・`..`を含む"));
        }
        if parts
            .iter()
            .any(|part| part.eq_ignore_ascii_case(".git") || part.eq_ignore_ascii_case(".kb"))
        {
            return Err(NoteIdError::new("Vault内部の予約ディレクトリを指す"));
        }
        if parts[..parts.len() - 1]
            .iter()
            .any(|part| part.to_ascii_lowercase().ends_with(".files"))
        {
            return Err(NoteIdError::new("旧添付ディレクトリの内側を指す"));
        }
        let file = parts.last().expect("空は先に拒否済み");
        if file.eq_ignore_ascii_case("index") || file.eq_ignore_ascii_case("log") {
            return Err(NoteIdError::new("OKF予約ファイルを指す"));
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn markdown_relative_path(&self) -> PathBuf {
        PathBuf::from(format!("{}.md", self.0))
    }

    pub fn attachments_relative_path(&self) -> PathBuf {
        PathBuf::from(format!("{}.files", self.0))
    }

    pub fn legacy_attachment_relative_path(&self, file_name: &str) -> Result<PathBuf, NoteIdError> {
        if file_name.is_empty()
            || file_name.trim() != file_name
            || file_name.contains(['/', '\\', ':'])
            || file_name.chars().any(char::is_control)
            || !matches!(
                Path::new(file_name)
                    .components()
                    .collect::<Vec<_>>()
                    .as_slice(),
                [Component::Normal(_)]
            )
        {
            return Err(NoteIdError::new("旧添付名は単一の通常成分にする"));
        }
        Ok(self.attachments_relative_path().join(file_name))
    }
}

impl fmt::Display for NoteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_nested_unicode_ids() {
        let id = NoteId::parse("notes/設計/同期-v2").unwrap();
        assert_eq!(id.as_str(), "notes/設計/同期-v2");
        assert_eq!(
            id.markdown_relative_path(),
            Path::new("notes/設計/同期-v2.md")
        );
    }

    #[test]
    fn rejects_every_path_escape_shape() {
        for raw in [
            "",
            " ",
            "/tmp/secret",
            "../secret",
            "notes/../../secret",
            "notes//secret",
            "notes/./secret",
            r"notes\..\secret",
            r"C:\secret",
            ".git/config",
            ".kb/index",
            "notes/a.files/secret",
            "notes/A.FILES/secret",
            "index",
            "notes/LOG",
        ] {
            assert!(NoteId::parse(raw).is_err(), "受理してはいけない: {raw:?}");
        }
    }

    #[test]
    fn legacy_attachment_name_is_one_component() {
        let id = NoteId::parse("notes/a").unwrap();
        assert_eq!(
            id.legacy_attachment_relative_path("図.png").unwrap(),
            Path::new("notes/a.files/図.png")
        );
        for name in ["../secret", "a/b", r"a\b", "/tmp/x", "C:\\x"] {
            assert!(id.legacy_attachment_relative_path(name).is_err());
        }
    }
}
