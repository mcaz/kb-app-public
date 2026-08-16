//! 配布物へ同梱する外部ツールの解決。
//!
//! Tauri sidecar はアプリ本体と同じディレクトリへ `git-lfs` という名前で置かれる。
//! 開発・受入では `KB_GIT_LFS_BIN` で同じ経路を明示でき、最後にだけ PATH へ
//! フォールバックする。Git 自身が filter-process として `git-lfs` を起動するため、
//! すべての Git 子processには解決した sidecar の親を PATH の先頭へ足す。

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

const GIT_LFS_OVERRIDE: &str = "KB_GIT_LFS_BIN";

fn sidecar_name() -> &'static str {
    if cfg!(windows) {
        "git-lfs.exe"
    } else {
        "git-lfs"
    }
}

fn resolve_git_lfs_binary(
    override_path: Option<OsString>,
    current_exe: Option<PathBuf>,
) -> PathBuf {
    if let Some(path) = override_path.filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    if let Some(candidate) = current_exe
        .as_deref()
        .and_then(Path::parent)
        .map(|directory| directory.join(sidecar_name()))
        .filter(|candidate| candidate.is_file())
    {
        return candidate;
    }
    PathBuf::from(sidecar_name())
}

pub(crate) fn git_lfs_binary() -> PathBuf {
    resolve_git_lfs_binary(
        std::env::var_os(GIT_LFS_OVERRIDE),
        std::env::current_exe().ok(),
    )
}

fn prepend_git_lfs_path(command: &mut Command, binary: &Path) {
    let Some(parent) = binary
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return;
    };
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let paths = std::iter::once(parent.to_path_buf()).chain(std::env::split_paths(&inherited));
    if let Ok(path) = std::env::join_paths(paths) {
        command.env("PATH", path);
    }
}

pub(crate) fn git_command() -> Command {
    let binary = git_lfs_binary();
    let mut command = Command::new("git");
    prepend_git_lfs_path(&mut command, &binary);
    command
}

pub(crate) fn git_lfs_command() -> Command {
    Command::new(git_lfs_binary())
}

pub(crate) fn git_lfs_available() -> bool {
    git_lfs_command()
        .arg("version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_override_wins_over_a_bundled_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("kb-app");
        let bundled = dir.path().join(sidecar_name());
        std::fs::write(&bundled, b"bundled").unwrap();
        let override_path = dir.path().join("acceptance-lfs");

        assert_eq!(
            resolve_git_lfs_binary(Some(override_path.clone().into_os_string()), Some(app)),
            override_path
        );
    }

    #[test]
    fn sibling_sidecar_wins_over_path_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("kb-app");
        let bundled = dir.path().join(sidecar_name());
        std::fs::write(&bundled, b"bundled").unwrap();

        assert_eq!(resolve_git_lfs_binary(None, Some(app)), bundled);
    }
}
