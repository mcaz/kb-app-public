//! AI クライアントから Vault の生ファイルを隠し、kb-app の取次口だけを残す。
//!
//! MCP の ON/OFF だけでは、汎用 shell を持つクライアントが同じユーザー権限で
//! Vault を読める。Codex / Claude Code の管理者ポリシーへ同じ拒否パスを配り、
//! ポリシーが欠けた状態では MCP も fail-closed にする。

use std::collections::BTreeSet;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

const CODEX_MARKER: &str = "# Managed by kb-app: AI raw-vault access guard";
const CLAUDE_MARKER: &str = "Read(//.kb-app-ai-raw-vault-guard-v1)";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum GuardTargetState {
    Enforced,
    Missing,
    Outdated,
    Conflict,
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct AiGuardStatus {
    pub ready: bool,
    pub codex: GuardTargetState,
    pub claude: GuardTargetState,
    pub guarded_paths: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuardPolicy {
    pub codex_requirements: String,
    pub claude_settings: String,
    pub guarded_paths: Vec<String>,
}

#[derive(Debug)]
pub enum AiGuardInstallError {
    Conflict,
    Unsupported,
    Failed(anyhow::Error),
}

impl std::fmt::Display for AiGuardInstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict => write!(f, "existing administrator policy conflicts with kb-app"),
            Self::Unsupported => write!(f, "AI access guard installation is unsupported"),
            Self::Failed(error) => write!(f, "AI access guard installation failed: {error}"),
        }
    }
}

impl std::error::Error for AiGuardInstallError {}

pub fn status() -> Result<AiGuardStatus> {
    let policy = policy()?;
    let mut status = status_for(
        &policy,
        &codex_requirements_path(),
        &claude_settings_path(),
        true,
    )?;
    if legacy_claude_hooks_present_at(&claude_user_settings_path()?)? {
        status.claude = GuardTargetState::Outdated;
        status.ready = false;
    }
    Ok(status)
}

/// MCP は、対応クライアントの OS ガードを確認できたときだけ Vault を開く。
/// 未知クライアントを推測で許可すると、同じ問題を新しい連携で再導入するため閉じる。
pub fn client_is_enforced(client: &str) -> bool {
    let Ok(status) = status() else {
        return false;
    };
    let client = client.to_ascii_lowercase();
    if client.contains("claude") {
        status.claude == GuardTargetState::Enforced
    } else if ["gpt", "codex", "chatgpt", "openai"]
        .iter()
        .any(|name| client.contains(name))
    {
        status.codex == GuardTargetState::Enforced
    } else {
        false
    }
}

pub fn policy() -> Result<GuardPolicy> {
    let guarded_paths = guarded_paths()?;
    let hook_executable = hook_executable()?;
    Ok(GuardPolicy {
        codex_requirements: codex_requirements(&guarded_paths, &hook_executable)?,
        claude_settings: claude_settings(&guarded_paths, &hook_executable)?,
        guarded_paths,
    })
}

fn hook_executable() -> Result<PathBuf> {
    let current = std::env::current_exe()
        .context("kb-app 実行ファイルが特定できない")
        .map_err(CoreError::configuration)?;
    if current
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|name| name == "kb-app")
    {
        return Ok(current);
    }

    #[cfg(target_os = "macos")]
    {
        let installed = PathBuf::from("/Applications/kb-app.app/Contents/MacOS/kb-app");
        if installed.is_file() {
            return Ok(installed);
        }
    }

    Err(CoreError::configuration(anyhow::anyhow!(
        "自動retrievalを実行できる kb-app デスクトップ実行ファイルが見つからない"
    )))
}

pub fn install() -> std::result::Result<AiGuardStatus, AiGuardInstallError> {
    #[cfg(not(target_os = "macos"))]
    {
        Err(AiGuardInstallError::Unsupported)
    }

    #[cfg(target_os = "macos")]
    {
        install_macos()
    }
}

#[cfg(target_os = "macos")]
fn install_macos() -> std::result::Result<AiGuardStatus, AiGuardInstallError> {
    let current = status().map_err(|error| AiGuardInstallError::Failed(error.into()))?;
    if current.codex == GuardTargetState::Conflict || current.claude == GuardTargetState::Conflict {
        return Err(AiGuardInstallError::Conflict);
    }
    if current.ready {
        return Ok(current);
    }

    let policy = policy().map_err(|error| AiGuardInstallError::Failed(error.into()))?;
    let temp = tempfile::tempdir().map_err(|error| AiGuardInstallError::Failed(error.into()))?;
    let codex_source = temp.path().join("requirements.toml");
    let claude_source = temp.path().join("90-kb-app-ai-guard.json");
    fs::write(&codex_source, policy.codex_requirements)
        .map_err(|error| AiGuardInstallError::Failed(error.into()))?;
    fs::write(&claude_source, policy.claude_settings)
        .map_err(|error| AiGuardInstallError::Failed(error.into()))?;

    let codex_target = codex_requirements_path();
    let claude_target = claude_settings_path();
    let codex_dir = codex_target.parent().ok_or_else(|| {
        AiGuardInstallError::Failed(anyhow::anyhow!("Codex policy has no parent"))
    })?;
    let claude_dir = claude_target.parent().ok_or_else(|| {
        AiGuardInstallError::Failed(anyhow::anyhow!("Claude policy has no parent"))
    })?;
    let command = format!(
        "/bin/mkdir -p {} {} && /usr/bin/install -m 0644 {} {} && /usr/bin/install -m 0644 {} {}",
        shell_quote(codex_dir),
        shell_quote(claude_dir),
        shell_quote(&codex_source),
        shell_quote(&codex_target),
        shell_quote(&claude_source),
        shell_quote(&claude_target),
    );
    let output = Command::new("/usr/bin/osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "do shell script (item 1 of argv) with prompt \"kb-app の完全保護を設定します。\" with administrator privileges",
            "-e",
            "end run",
            "--",
            &command,
        ])
        .output()
        .map_err(|error| AiGuardInstallError::Failed(error.into()))?;
    if !output.status.success() {
        return Err(AiGuardInstallError::Failed(anyhow::anyhow!(
            "administrator authorization was denied or installation failed"
        )));
    }

    remove_legacy_claude_hooks_at(
        &claude_user_settings_path().map_err(|error| AiGuardInstallError::Failed(error.into()))?,
    )
    .map_err(|error| AiGuardInstallError::Failed(error.into()))?;

    let installed = status().map_err(|error| AiGuardInstallError::Failed(error.into()))?;
    if !installed.ready {
        return Err(AiGuardInstallError::Failed(anyhow::anyhow!(
            "installed policies did not pass verification"
        )));
    }
    Ok(installed)
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
}

fn claude_user_settings_path() -> Result<PathBuf> {
    dirs::home_dir()
        .context("home が特定できない")
        .map(|home| home.join(".claude/settings.json"))
        .map_err(CoreError::configuration)
}

fn legacy_claude_hooks_present_at(path: &Path) -> Result<bool> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(CoreError::configuration(error)),
    };
    let value: serde_json::Value = serde_json::from_str(&text).map_err(CoreError::configuration)?;
    Ok(value
        .pointer("/hooks")
        .into_iter()
        .flat_map(|hooks| hooks.as_object().into_iter().flat_map(|map| map.values()))
        .flat_map(|groups| groups.as_array().into_iter().flatten())
        .flat_map(|group| {
            group
                .get("hooks")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|hook| hook.get("command").and_then(serde_json::Value::as_str))
        .any(is_legacy_claude_hook_command))
}

#[cfg(any(target_os = "macos", test))]
fn remove_legacy_claude_hooks_at(path: &Path) -> Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(CoreError::configuration(error)),
    };
    let mut value: serde_json::Value =
        serde_json::from_str(&text).map_err(CoreError::configuration)?;
    let Some(events) = value
        .get_mut("hooks")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Ok(());
    };
    let mut removed = false;
    for groups in events.values_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        for group in groups.iter_mut() {
            if let Some(hooks) = group
                .get_mut("hooks")
                .and_then(serde_json::Value::as_array_mut)
            {
                hooks.retain(|hook| {
                    let legacy = hook
                        .get("command")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(is_legacy_claude_hook_command);
                    removed |= legacy;
                    !legacy
                });
            }
        }
        groups.retain(|group| {
            group
                .get("hooks")
                .and_then(serde_json::Value::as_array)
                .is_none_or(|hooks| !hooks.is_empty())
        });
    }
    events.retain(|_, groups| groups.as_array().is_none_or(|groups| !groups.is_empty()));
    if !removed {
        return Ok(());
    }

    let parent = path.parent().ok_or_else(|| {
        CoreError::configuration(anyhow::anyhow!("Claude settings has no parent"))
    })?;
    fs::create_dir_all(parent).map_err(CoreError::configuration)?;
    let temp = parent.join(".settings.json.kb-app.tmp");
    let mut updated = serde_json::to_string_pretty(&value).map_err(CoreError::configuration)?;
    updated.push('\n');
    fs::write(&temp, updated).map_err(CoreError::configuration)?;
    fs::rename(&temp, path).map_err(CoreError::configuration)?;
    Ok(())
}

fn is_legacy_claude_hook_command(command: &str) -> bool {
    command.contains("kb-hook-preprompt.py") || command.contains("kb-hook-stop.py")
}

pub fn codex_requirements_path() -> PathBuf {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        PathBuf::from("/etc/codex/requirements.toml")
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        PathBuf::new()
    }
}

pub fn claude_settings_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        PathBuf::from(
            "/Library/Application Support/ClaudeCode/managed-settings.d/90-kb-app-ai-guard.json",
        )
    }
    #[cfg(target_os = "linux")]
    {
        PathBuf::from("/etc/claude-code/managed-settings.d/90-kb-app-ai-guard.json")
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        PathBuf::new()
    }
}

pub fn codex_policy_is_owned(path: &Path) -> bool {
    fs::read_to_string(path)
        .map(|text| text.starts_with(CODEX_MARKER))
        .unwrap_or(false)
}

fn guarded_paths() -> Result<Vec<String>> {
    let home = dirs::home_dir()
        .context("home が特定できない")
        .map_err(CoreError::configuration)?;
    let config = dirs::config_dir()
        .context("設定ディレクトリが特定できない")
        .map_err(CoreError::configuration)?
        .join("kb-app");
    let registry = crate::registry::Registry::load().map_err(CoreError::configuration)?;

    let mut paths = BTreeSet::new();
    add_guarded_path(&mut paths, &home.join("kb"))?;
    add_guarded_path(&mut paths, &config)?;
    for vault in registry.vaults {
        add_guarded_path(&mut paths, &vault.path)?;
    }
    Ok(paths.into_iter().collect())
}

fn add_guarded_path(paths: &mut BTreeSet<String>, path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(CoreError::configuration(anyhow::anyhow!(
            "AI guard path is not absolute: {}",
            path.display()
        )));
    }
    paths.insert(path.to_string_lossy().into_owned());
    if let Ok(canonical) = path.canonicalize() {
        paths.insert(canonical.to_string_lossy().into_owned());
    }
    Ok(())
}

fn codex_requirements(paths: &[String], hook_executable: &Path) -> Result<String> {
    let managed_dir = hook_executable
        .parent()
        .context("kb-app 実行ファイルに親ディレクトリがない")
        .map_err(CoreError::configuration)?;
    let hook_command = format!(
        "{} --hook-auto-retrieve --client codex-cli/gpt-5-codex",
        shell_quote(hook_executable)
    );
    let mut text = format!(
        "{CODEX_MARKER}\n\
allowed_sandbox_modes = [\"read-only\", \"workspace-write\"]\n\
default_permissions = \"kb_app_workspace\"\n\n\
[features]\n\
hooks = true\n\n\
[hooks]\n\
managed_dir = {}\n\n\
[[hooks.UserPromptSubmit]]\n\n\
[[hooks.UserPromptSubmit.hooks]]\n\
type = \"command\"\n\
command = {}\n\
timeout = 30\n\
statusMessage = \"kb-appをMCP検索中…\"\n\
additionalContextLimit = 12000\n\n\
[allowed_permission_profiles]\n\
kb_app_read_only = true\n\
kb_app_workspace = true\n\n\
[permissions.filesystem]\n\
deny_read = [\n",
        toml_key(&managed_dir.to_string_lossy()),
        toml_key(&hook_command)
    );
    for path in paths {
        text.push_str(&format!("  {},\n", toml_key(path)));
    }
    text.push_str(
        "]\n\n\
[permissions.kb_app_read_only]\n\
description = \"Read-only workspace with kb-app raw data denied.\"\n\
extends = \":read-only\"\n\n\
[permissions.kb_app_read_only.filesystem]\n",
    );
    for path in paths {
        text.push_str(&format!("{} = \"deny\"\n", toml_key(path)));
    }
    text.push_str(
        "\n[permissions.kb_app_workspace]\n\
description = \"Workspace access with kb-app raw data denied.\"\n\
extends = \":workspace\"\n\n\
[permissions.kb_app_workspace.filesystem]\n",
    );
    for path in paths {
        text.push_str(&format!("{} = \"deny\"\n", toml_key(path)));
    }
    Ok(text)
}

fn toml_key(value: &str) -> String {
    serde_json::to_string(value).expect("path string is JSON serializable")
}

fn claude_settings(paths: &[String], hook_executable: &Path) -> Result<String> {
    let read_rules: Vec<String> = std::iter::once(CLAUDE_MARKER.to_string())
        .chain(
            paths
                .iter()
                .map(|path| format!("Read(/{}/**)", path.trim_end_matches('/'))),
        )
        .collect();
    let edit_rules: Vec<String> = paths
        .iter()
        .map(|path| format!("Edit(/{}/**)", path.trim_end_matches('/')))
        .collect();
    let value = serde_json::json!({
        "permissions": {
            "deny": read_rules.into_iter().chain(edit_rules).collect::<Vec<_>>(),
            "disableBypassPermissionsMode": "disable"
        },
        "sandbox": {
            "enabled": true,
            "failIfUnavailable": true,
            "allowUnsandboxedCommands": false,
            "filesystem": {
                "disabled": false,
                "allowManagedReadPathsOnly": true,
                "denyRead": paths,
                "denyWrite": paths
            }
        },
        "hooks": {
            "UserPromptSubmit": [{
                "hooks": [{
                    "type": "command",
                    "command": hook_executable,
                    "args": [
                        "--hook-auto-retrieve",
                        "--client",
                        "claude-code/claude"
                    ],
                    "timeout": 30,
                    "statusMessage": "kb-appをMCP検索中…"
                }]
            }]
        }
    });
    let mut text = serde_json::to_string_pretty(&value).map_err(CoreError::configuration)?;
    text.push('\n');
    Ok(text)
}

fn status_for(
    policy: &GuardPolicy,
    codex_path: &Path,
    claude_path: &Path,
    require_managed_owner: bool,
) -> Result<AiGuardStatus> {
    if codex_path.as_os_str().is_empty() || claude_path.as_os_str().is_empty() {
        return Ok(AiGuardStatus {
            ready: false,
            codex: GuardTargetState::Unsupported,
            claude: GuardTargetState::Unsupported,
            guarded_paths: policy.guarded_paths.clone(),
        });
    }

    let codex = file_state(
        codex_path,
        &policy.codex_requirements,
        Some(CODEX_MARKER),
        require_managed_owner,
    )?;
    let claude = file_state(
        claude_path,
        &policy.claude_settings,
        Some(CLAUDE_MARKER),
        require_managed_owner,
    )?;
    Ok(AiGuardStatus {
        ready: codex == GuardTargetState::Enforced && claude == GuardTargetState::Enforced,
        codex,
        claude,
        guarded_paths: policy.guarded_paths.clone(),
    })
}

fn file_state(
    path: &Path,
    expected: &str,
    owned_marker: Option<&str>,
    require_managed_owner: bool,
) -> Result<GuardTargetState> {
    match fs::read_to_string(path) {
        Ok(actual)
            if actual == expected && managed_ownership_is_secure(path, require_managed_owner)? =>
        {
            Ok(GuardTargetState::Enforced)
        }
        Ok(actual) if owned_marker.is_some_and(|marker| actual.contains(marker)) => {
            Ok(GuardTargetState::Outdated)
        }
        Ok(_) => Ok(GuardTargetState::Conflict),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(GuardTargetState::Missing),
        Err(error) => Err(CoreError::configuration(error)),
    }
}

fn managed_ownership_is_secure(path: &Path, required: bool) -> Result<bool> {
    if !required {
        return Ok(true);
    }
    #[cfg(unix)]
    {
        // file だけが root-owned でも、途中の directory を一般 user が差し替えられる
        // なら永続的な管理境界にならない。root までの全経路を同じ条件で検査する。
        for component in path.ancestors() {
            let metadata = fs::metadata(component).map_err(CoreError::configuration)?;
            if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
                return Ok(false);
            }
        }
        Ok(true)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example_policy() -> GuardPolicy {
        let guarded_paths = vec![
            "/Users/example/Library/Application Support/kb-app".into(),
            "/Users/example/kb".into(),
        ];
        let hook_executable = Path::new("/Applications/kb-app.app/Contents/MacOS/kb-app");
        GuardPolicy {
            codex_requirements: codex_requirements(&guarded_paths, hook_executable).unwrap(),
            claude_settings: claude_settings(&guarded_paths, hook_executable).unwrap(),
            guarded_paths,
        }
    }

    #[test]
    fn policies_deny_raw_data_for_builtins_and_subprocesses() {
        let policy = example_policy();
        assert!(
            policy
                .codex_requirements
                .contains("extends = \":workspace\"")
        );
        assert!(
            policy
                .codex_requirements
                .contains("\"/Users/example/kb\" = \"deny\"")
        );
        assert!(policy.codex_requirements.contains("deny_read = ["));
        assert!(
            policy
                .codex_requirements
                .contains("  \"/Users/example/kb\",\n")
        );
        assert!(
            policy
                .codex_requirements
                .contains("allowed_sandbox_modes = [\"read-only\", \"workspace-write\"]")
        );
        assert!(!policy.codex_requirements.contains(":danger-full-access"));
        assert!(
            policy
                .codex_requirements
                .contains("[[hooks.UserPromptSubmit]]")
        );
        assert!(policy.codex_requirements.contains("--hook-auto-retrieve"));
        assert!(policy.codex_requirements.contains("hooks = true"));

        let claude: serde_json::Value = serde_json::from_str(&policy.claude_settings).unwrap();
        assert_eq!(claude["sandbox"]["enabled"], true);
        assert_eq!(claude["sandbox"]["failIfUnavailable"], true);
        assert_eq!(claude["sandbox"]["allowUnsandboxedCommands"], false);
        assert_eq!(
            claude["sandbox"]["filesystem"]["allowManagedReadPathsOnly"],
            true
        );
        assert!(
            claude["permissions"]["deny"]
                .as_array()
                .unwrap()
                .iter()
                .any(|rule| rule == "Read(//Users/example/kb/**)")
        );
        assert_eq!(
            claude["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"],
            "/Applications/kb-app.app/Contents/MacOS/kb-app"
        );
        assert_eq!(
            claude["hooks"]["UserPromptSubmit"][0]["hooks"][0]["args"][0],
            "--hook-auto-retrieve"
        );
    }

    #[test]
    fn status_detects_missing_outdated_conflicting_and_enforced_files() {
        let dir = tempfile::tempdir().unwrap();
        let codex = dir.path().join("requirements.toml");
        let claude = dir.path().join("managed.json");
        let policy = example_policy();

        let missing = status_for(&policy, &codex, &claude, false).unwrap();
        assert_eq!(missing.codex, GuardTargetState::Missing);
        assert_eq!(missing.claude, GuardTargetState::Missing);

        fs::write(&codex, format!("{CODEX_MARKER}\nold")).unwrap();
        fs::write(&claude, "{}").unwrap();
        let changed = status_for(&policy, &codex, &claude, false).unwrap();
        assert_eq!(changed.codex, GuardTargetState::Outdated);
        assert_eq!(changed.claude, GuardTargetState::Conflict);

        fs::write(&codex, &policy.codex_requirements).unwrap();
        fs::write(&claude, &policy.claude_settings).unwrap();
        let enforced = status_for(&policy, &codex, &claude, false).unwrap();
        assert!(enforced.ready);
    }

    #[test]
    fn legacy_claude_hooks_are_removed_without_touching_other_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        fs::write(
            &settings,
            serde_json::to_vec_pretty(&serde_json::json!({
                "hooks": {
                    "UserPromptSubmit": [{"hooks": [
                        {"type": "command", "command": "python3 /x/kb-hook-preprompt.py"},
                        {"type": "command", "command": "python3 /x/other.py"}
                    ]}],
                    "Stop": [{"hooks": [
                        {"type": "command", "command": "python3 /x/kb-hook-stop.py"}
                    ]}]
                },
                "theme": "dark"
            }))
            .unwrap(),
        )
        .unwrap();

        assert!(legacy_claude_hooks_present_at(&settings).unwrap());
        remove_legacy_claude_hooks_at(&settings).unwrap();
        assert!(!legacy_claude_hooks_present_at(&settings).unwrap());
        let updated: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(updated["theme"], "dark");
        assert_eq!(
            updated["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"],
            "python3 /x/other.py"
        );
        assert!(updated["hooks"].get("Stop").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn matching_user_owned_files_are_not_managed_policy() {
        let dir = tempfile::tempdir().unwrap();
        let codex = dir.path().join("requirements.toml");
        let claude = dir.path().join("managed.json");
        let policy = example_policy();
        fs::write(&codex, &policy.codex_requirements).unwrap();
        fs::write(&claude, &policy.claude_settings).unwrap();
        if fs::metadata(&codex).unwrap().uid() == 0 {
            return;
        }

        let status = status_for(&policy, &codex, &claude, true).unwrap();
        assert!(!status.ready);
        assert_eq!(status.codex, GuardTargetState::Outdated);
        assert_eq!(status.claude, GuardTargetState::Outdated);
    }
}
