//! 配布物へ同梱する外部ツールの解決。
//!
//! macOS配布ではGitとLFSを同梱し、開発ツールの導入を求めない。
//! Gitが起動するHTTPS helperとLFS、LFSが再帰的に呼ぶGitも同じ配布物を使う。
//! 配布内の欠損をホストのGitで隠さない。開発・受入の明示overrideは維持する。

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

const GIT_LFS_OVERRIDE: &str = "KB_GIT_LFS_BIN";
const GIT_OVERRIDE: &str = "KB_GIT_BIN";

fn sidecar_name(name: &str) -> String {
    format!("{name}{}", if cfg!(windows) { ".exe" } else { "" })
}

fn macos_resources(exe: &Path) -> Option<PathBuf> {
    let bin = exe.parent()?;
    let contents = bin.parent()?;
    (bin.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && contents.parent()?.extension()? == "app")
        .then(|| contents.join("Resources"))
}

fn resolve_binary(
    name: &str,
    override_path: Option<OsString>,
    current_exe: Option<&Path>,
) -> PathBuf {
    if let Some(path) = override_path.filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    if let Some(candidate) = current_exe
        .and_then(Path::parent)
        .map(|directory| directory.join(sidecar_name(name)))
        .filter(|candidate| current_exe.and_then(macos_resources).is_some() || candidate.is_file())
    {
        return candidate;
    }
    PathBuf::from(sidecar_name(name))
}

pub(crate) fn git_lfs_binary() -> PathBuf {
    resolve_binary(
        "git-lfs",
        std::env::var_os(GIT_LFS_OVERRIDE),
        std::env::current_exe().ok().as_deref(),
    )
}

fn git_resource_paths(git: &Path) -> Option<(PathBuf, PathBuf)> {
    if let Some(resources) = macos_resources(git) {
        return Some((resources.join("git-core"), resources.join("git-templates")));
    }
    let bin = git.parent()?;
    if bin.file_name().is_some_and(|name| name == "binaries") {
        let runtime = bin.parent()?.join("git-runtime");
        return Some((runtime.join("git-core"), runtime.join("templates")));
    }
    // tauri-buildは開発時の資材を実行ファイルと同じtarget directoryへコピーする。
    bin.join("git-provenance.json")
        .is_file()
        .then(|| (bin.join("git-core"), bin.join("git-templates")))
}

fn configure_tools(
    command: &mut Command,
    git: &Path,
    lfs: &Path,
    inherited: &std::ffi::OsStr,
) -> std::io::Result<()> {
    if git.is_absolute()
        && let Some(lfs_dir) = lfs.parent().filter(|path| !path.as_os_str().is_empty())
    {
        let recursive_git = lfs_dir.join(sidecar_name("git"));
        if recursive_git.exists()
            && std::fs::canonicalize(&recursive_git)? != std::fs::canonicalize(git)?
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "GitとGit LFSの指定先で再帰実行するGitが競合しています",
            ));
        }
    }
    let mut directories = Vec::new();
    for binary in [lfs, git] {
        if let Some(parent) = binary.parent().filter(|path| !path.as_os_str().is_empty())
            && !directories.contains(&parent.to_path_buf())
        {
            directories.push(parent.to_path_buf());
        }
    }
    if !directories.is_empty() {
        let paths = directories
            .into_iter()
            .chain(std::env::split_paths(inherited));
        // PATHに表現できない配置では環境の別版へ逃げず、子process起動を失敗させる。
        command.env(
            "PATH",
            std::env::join_paths(paths)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?,
        );
    }
    if let Some((helpers, templates)) = git_resource_paths(git) {
        command.env("GIT_EXEC_PATH", helpers);
        command.env("GIT_TEMPLATE_DIR", templates);
    }
    Ok(())
}

fn validate_runtime(git: &Path, lfs: &Path) -> std::io::Result<()> {
    if let Some((helpers, templates)) = git_resource_paths(git) {
        for path in [
            git.to_path_buf(),
            lfs.to_path_buf(),
            helpers.join("git-remote-http"),
            helpers.join("git-remote-https"),
        ] {
            let metadata = std::fs::symlink_metadata(&path)?;
            if !metadata.file_type().is_file() {
                return Err(std::io::Error::other(
                    "同梱Git実行ファイルが通常ファイルではありません",
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // 所有者だけ実行不可の状態も拒否し、EACCES後のPATH再探索を防ぐ。
                if metadata.permissions().mode() & 0o111 != 0o111 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "同梱Git実行ファイルの実行権限がありません",
                    ));
                }
            }
        }
        if !templates.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "同梱Gitテンプレートがありません",
            ));
        }
    }
    Ok(())
}

fn resolve_tool_pair(git: PathBuf, lfs: PathBuf) -> std::io::Result<(PathBuf, PathBuf)> {
    if git.file_name() != Some(std::ffi::OsStr::new(&sidecar_name("git")))
        || lfs.file_name() != Some(std::ffi::OsStr::new(&sidecar_name("git-lfs")))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Gitの指定にはgit、Git LFSの指定にはgit-lfsという実行ファイル名が必要です",
        ));
    }
    let absolute_if_located = |path: PathBuf| -> std::io::Result<PathBuf> {
        if !path.is_absolute()
            && path
                .parent()
                .is_some_and(|parent| !parent.as_os_str().is_empty())
        {
            Ok(std::env::current_dir()?.join(path))
        } else {
            Ok(path)
        }
    };
    let mut git = absolute_if_located(git)?;
    let lfs = absolute_if_located(lfs)?;
    if !git.is_absolute()
        && let Some(directory) = lfs.parent().filter(|parent| !parent.as_os_str().is_empty())
    {
        let candidate = directory.join(sidecar_name("git"));
        if candidate.is_file() {
            // PATH追加によって実際に選ばれるGitを先に固定し、対応helperも一緒に指定する。
            git = candidate;
        }
    }
    Ok((git, lfs))
}

fn tool_command(lfs_command: bool) -> std::io::Result<Command> {
    let exe = std::env::current_exe().ok();
    let git = resolve_binary("git", std::env::var_os(GIT_OVERRIDE), exe.as_deref());
    let (git, lfs) = resolve_tool_pair(git, git_lfs_binary())?;
    // Gitはexec-pathが欠けるとPATH内のhelperも探すため、起動前に配布欠損を止める。
    validate_runtime(&git, &lfs)?;
    let mut command = Command::new(if lfs_command { &lfs } else { &git });
    configure_tools(
        &mut command,
        &git,
        &lfs,
        &std::env::var_os("PATH").unwrap_or_default(),
    )?;
    Ok(command)
}

pub(crate) fn git_command() -> std::io::Result<Command> {
    tool_command(false)
}

fn git_lfs_command() -> std::io::Result<Command> {
    tool_command(true)
}

pub(crate) fn git_lfs_available() -> bool {
    git_lfs_command()
        .and_then(|mut command| command.arg("version").output())
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
        let bundled = dir.path().join(sidecar_name("git-lfs"));
        std::fs::write(&bundled, b"bundled").unwrap();
        let override_path = dir.path().join("acceptance-lfs");

        assert_eq!(
            resolve_binary(
                "git-lfs",
                Some(override_path.clone().into_os_string()),
                Some(&app)
            ),
            override_path
        );
    }

    #[test]
    fn sibling_sidecar_wins_over_path_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("kb-app");
        let bundled = dir.path().join(sidecar_name("git-lfs"));
        std::fs::write(&bundled, b"bundled").unwrap();

        assert_eq!(resolve_binary("git-lfs", None, Some(&app)), bundled);
    }

    /// 2026-09-08: CLTがないMacで同梱欠損を環境のGitで隠さない。
    #[test]
    fn macos_bundle_keeps_missing_sidecars_as_explicit_paths() {
        let app = Path::new("/Applications/日本語 アプリ.app/Contents/MacOS/kb-app");
        for name in ["git", "git-lfs"] {
            assert_eq!(
                resolve_binary(name, None, Some(app)),
                app.parent().unwrap().join(sidecar_name(name))
            );
        }
    }

    #[test]
    fn development_without_sidecars_keeps_path_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("kb");
        assert_eq!(
            resolve_binary("git", None, Some(&exe)),
            PathBuf::from(sidecar_name("git"))
        );
    }

    #[test]
    fn git_and_lfs_children_share_bundled_helpers_and_path() {
        let root = Path::new("/Applications/日本語 アプリ.app/Contents");
        let git = root.join("MacOS/git");
        let lfs = root.join("MacOS/git-lfs");
        for binary in [&git, &lfs] {
            let mut command = Command::new(binary);
            configure_tools(&mut command, &git, &lfs, std::ffi::OsStr::new("/usr/bin")).unwrap();
            let env: std::collections::BTreeMap<_, _> = command.get_envs().collect();
            assert_eq!(
                env[std::ffi::OsStr::new("GIT_EXEC_PATH")],
                Some(root.join("Resources/git-core").as_os_str())
            );
            assert_eq!(
                env[std::ffi::OsStr::new("GIT_TEMPLATE_DIR")],
                Some(root.join("Resources/git-templates").as_os_str())
            );
            assert_eq!(
                std::env::split_paths(env[std::ffi::OsStr::new("PATH")].unwrap())
                    .collect::<Vec<_>>(),
                vec![root.join("MacOS"), PathBuf::from("/usr/bin")]
            );
        }
    }

    #[test]
    fn explicit_development_git_uses_prepared_helpers() {
        let root = tempfile::tempdir().unwrap();
        let git = root.path().join("binaries/git");
        let mut command = Command::new(&git);
        configure_tools(
            &mut command,
            &git,
            Path::new("git-lfs"),
            std::ffi::OsStr::new(""),
        )
        .unwrap();
        let env: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        assert_eq!(
            env[std::ffi::OsStr::new("GIT_EXEC_PATH")],
            Some(root.path().join("git-runtime/git-core").as_os_str())
        );
    }

    /// 2026-09-08: GIT_EXEC_PATHだけではPATHにある別版helperへのfallbackを止められない。
    #[test]
    fn missing_bundled_https_helper_is_rejected_before_git_can_search_path() {
        let dir = tempfile::tempdir().unwrap();
        let contents = dir.path().join("日本語.app/Contents");
        let bin = contents.join("MacOS");
        let resources = contents.join("Resources");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(resources.join("git-core")).unwrap();
        std::fs::create_dir_all(resources.join("git-templates")).unwrap();
        let git = bin.join("git");
        let lfs = bin.join("git-lfs");
        for path in [&git, &lfs, &resources.join("git-core/git-remote-http")] {
            std::fs::write(path, b"fixture").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        assert_eq!(
            validate_runtime(&git, &lfs).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        std::fs::write(resources.join("git-core/git-remote-https"), b"fixture").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                validate_runtime(&git, &lfs).unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied
            );
            std::fs::set_permissions(
                resources.join("git-core/git-remote-https"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        assert!(validate_runtime(&git, &lfs).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn unrepresentable_path_is_rejected_instead_of_searching_the_working_directory() {
        let git = Path::new("/Applications/with:colon.app/Contents/MacOS/git");
        let mut command = Command::new(git);
        let error = configure_tools(
            &mut command,
            git,
            Path::new("git-lfs"),
            std::ffi::OsStr::new("/usr/bin"),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn separate_lfs_override_is_first_but_cannot_shadow_the_selected_git() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("git-bin");
        let lfs_dir = root.path().join("lfs-bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&lfs_dir).unwrap();
        let git = bin.join(sidecar_name("git"));
        let lfs = lfs_dir.join(sidecar_name("git-lfs"));
        std::fs::write(&git, b"git").unwrap();
        let mut command = Command::new(&git);
        configure_tools(&mut command, &git, &lfs, std::ffi::OsStr::new("")).unwrap();
        let path = command
            .get_envs()
            .find(|(key, _)| *key == "PATH")
            .unwrap()
            .1
            .unwrap();
        assert_eq!(std::env::split_paths(path).next(), Some(lfs_dir.clone()));
        std::fs::write(lfs_dir.join(sidecar_name("git")), b"other-git").unwrap();
        assert_eq!(
            configure_tools(&mut command, &git, &lfs, std::ffi::OsStr::new(""))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn lfs_override_with_prepared_git_also_selects_its_helpers() {
        let root = tempfile::tempdir().unwrap();
        let binaries = root.path().join("binaries");
        std::fs::create_dir(&binaries).unwrap();
        let bundled_git = binaries.join(sidecar_name("git"));
        std::fs::write(&bundled_git, b"git").unwrap();
        let (git, lfs) = resolve_tool_pair(
            PathBuf::from(sidecar_name("git")),
            binaries.join(sidecar_name("git-lfs")),
        )
        .unwrap();
        assert_eq!(git, bundled_git);
        let mut command = Command::new(&git);
        configure_tools(&mut command, &git, &lfs, std::ffi::OsStr::new("/usr/bin")).unwrap();
        assert!(command.get_envs().any(|(key, _)| key == "GIT_EXEC_PATH"));
        assert!(resolve_tool_pair(git, binaries.join("different-lfs-name")).is_err());
    }

    #[test]
    fn tauri_development_output_uses_flat_resource_layout() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("git-provenance.json"), b"{}").unwrap();
        assert_eq!(
            git_resource_paths(&root.path().join("git")),
            Some((
                root.path().join("git-core"),
                root.path().join("git-templates")
            ))
        );
    }
}
