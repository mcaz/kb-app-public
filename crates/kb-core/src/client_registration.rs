//! 通常MCPのユーザー登録を診断・修復する。登録の一致は稼働中クライアントの受入証拠ではない。
//! Codexは公式config.toml、Claude Codeはuser scopeの.claude.json、Desktopは専用JSONを使う。
//! 管理設定やproject overrideは変更せず、未知の制約を成功と扱わない。

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use toml_edit::{DocumentMut, Item};

const SERVERS: [(&str, &str); 3] = [
    ("kb-app-read", "read"),
    ("kb-app-write", "write"),
    ("kb-app-maintenance", "maintenance"),
];
// Claudeのglobal設定にはproject状態も入る。異常に大きな入力でGUIを無制限に占有しない。
const MAX_CONFIG_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum RegistrationClient {
    Codex,
    ClaudeCode,
    ClaudeDesktop,
}

impl RegistrationClient {
    fn actor(self) -> &'static str {
        match self {
            Self::Codex => "codex-cli/gpt",
            Self::ClaudeCode => "claude-code/claude",
            Self::ClaudeDesktop => "claude-desktop/claude",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum RegistrationState {
    Registered,
    Missing,
    NeedsRepair,
    Blocked,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum RegistrationIssueKind {
    MissingConfig,
    MissingServer,
    LegacyRegistration,
    ExecutableMismatch,
    VaultMismatch,
    ClientMismatch,
    ArgumentsMismatch,
    DisabledServer,
    ToolPolicyRestriction,
    InvalidConfig,
    UnreadableConfig,
    ManagedPolicyConflict,
    ManagedPolicyUnverified,
    ScopedOverride,
    UnsafePath,
    UnsupportedPlatform,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RegistrationIssue {
    pub kind: RegistrationIssueKind,
    /// 固定した登録名だけを返し、他サーバー名・設定値・資格情報は診断へ出さない。
    pub server: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RegistrationStatus {
    pub client: RegistrationClient,
    pub state: RegistrationState,
    pub issues: Vec<RegistrationIssue>,
    pub can_repair: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RegistrationRepair {
    pub status: RegistrationStatus,
    pub changed: bool,
    pub backup_path: Option<String>,
}

#[derive(Default)]
struct Paths {
    config: PathBuf,
    codex_requirements: Option<PathBuf>,
    codex_managed_config: Option<PathBuf>,
    claude_managed_mcp: Option<PathBuf>,
    claude_settings: Vec<(PathBuf, bool)>,
    claude_managed_directory: Option<PathBuf>,
}

/// ユーザーscopeの設定を検査する。任意のproject・CLI overrideや稼働プロセスは対象に含まない。
pub fn status(client: RegistrationClient, exe: &Path, vault_name: &str) -> RegistrationStatus {
    match paths(client) {
        Ok(paths) => inspect(&paths, client, exe, vault_name),
        Err(kind) => report(client, vec![issue(kind, None)]),
    }
}

/// アプリ自身の実行ファイル・選択中Vaultへ用途別登録を修復する。MCPの再起動は行わない。
pub fn repair(
    client: RegistrationClient,
    exe: &Path,
    vault_name: &str,
) -> Result<RegistrationRepair> {
    let paths = paths(client).map_err(|kind| anyhow::anyhow!("registration: {kind:?}"))?;
    repair_at(&paths, client, exe, vault_name)
}

fn paths(client: RegistrationClient) -> std::result::Result<Paths, RegistrationIssueKind> {
    use RegistrationIssueKind as Kind;
    if !cfg!(target_os = "macos") {
        return Err(Kind::UnsupportedPlatform);
    }
    let home = dirs::home_dir().ok_or(Kind::UnsafePath)?;
    let mut paths = Paths::default();
    match client {
        RegistrationClient::Codex => {
            let directory = std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex"));
            paths.config = directory.join("config.toml");
            // macOSの/etc自体は/private/etcへの標準symlink。既知の実体から検査する。
            paths.codex_requirements = Some(PathBuf::from("/private/etc/codex/requirements.toml"));
            paths.codex_managed_config =
                Some(PathBuf::from("/private/etc/codex/managed_config.toml"));
        }
        RegistrationClient::ClaudeCode => {
            // カスタム配置のglobal JSON名はクライアント版によって異なり得るため推測で書かない。
            if std::env::var_os("CLAUDE_CONFIG_DIR").is_some() {
                return Err(Kind::UnsafePath);
            }
            paths.config = home.join(".claude.json");
            let managed = PathBuf::from("/Library/Application Support/ClaudeCode");
            paths.claude_managed_mcp = Some(managed.join("managed-mcp.json"));
            paths.claude_settings = vec![
                (managed.join("managed-settings.json"), true),
                (home.join(".claude/settings.json"), false),
            ];
            paths.claude_managed_directory = Some(managed.join("managed-settings.d"));
        }
        RegistrationClient::ClaudeDesktop => {
            paths.config =
                home.join("Library/Application Support/Claude/claude_desktop_config.json");
        }
    }
    if !paths.config.is_absolute() {
        return Err(Kind::UnsafePath);
    }
    Ok(paths)
}

fn issue(kind: RegistrationIssueKind, server: Option<&str>) -> RegistrationIssue {
    RegistrationIssue {
        kind,
        server: server.map(str::to_owned),
    }
}

fn report(client: RegistrationClient, issues: Vec<RegistrationIssue>) -> RegistrationStatus {
    use RegistrationIssueKind as Kind;
    let blocked = issues.iter().any(|issue| {
        matches!(
            issue.kind,
            Kind::InvalidConfig
                | Kind::UnreadableConfig
                | Kind::ManagedPolicyConflict
                | Kind::ManagedPolicyUnverified
                | Kind::ToolPolicyRestriction
                | Kind::ScopedOverride
                | Kind::UnsafePath
        )
    });
    let state = if issues.iter().any(|i| i.kind == Kind::UnsupportedPlatform) {
        RegistrationState::Unsupported
    } else if blocked {
        RegistrationState::Blocked
    } else if issues.is_empty() {
        RegistrationState::Registered
    } else if issues.iter().any(|i| i.kind == Kind::MissingConfig) {
        RegistrationState::Missing
    } else {
        RegistrationState::NeedsRepair
    };
    RegistrationStatus {
        client,
        can_repair: matches!(
            state,
            RegistrationState::Missing | RegistrationState::NeedsRepair
        ),
        state,
        issues,
    }
}

fn expected(client: RegistrationClient, exe: &Path, vault: &str, surface: &str) -> Value {
    let mut entry = json!({
        "command": exe.to_str().unwrap_or_default(),
        "args": ["--mcp", "--mcp-surface", surface, "--vault", vault, "--client", client.actor()]
    });
    if client == RegistrationClient::ClaudeCode {
        entry["type"] = json!("stdio");
    }
    entry
}

fn unique_option<'a>(args: Option<&'a Value>, flag: &str) -> Option<&'a str> {
    let args = args?.as_array()?;
    let mut positions = args
        .iter()
        .enumerate()
        .filter(|(_, value)| value.as_str() == Some(flag));
    let (position, _) = positions.next()?;
    if positions.next().is_some() {
        return None;
    }
    args.get(position + 1)?.as_str()
}

fn same_client_hint(client: RegistrationClient, hint: &str) -> bool {
    !hint.chars().any(char::is_control)
        && crate::client_surface::ClientSurface::from_hint(hint)
            == crate::client_surface::ClientSurface::from_hint(client.actor())
}

fn existing_client_hint(client: RegistrationClient, entry: Option<&Value>) -> Option<&str> {
    unique_option(entry?.get("args"), "--client").filter(|hint| same_client_hint(client, hint))
}

fn expected_preserving_client(
    client: RegistrationClient,
    exe: &Path,
    vault: &str,
    surface: &str,
    entry: Option<&Value>,
) -> Value {
    let mut wanted = expected(client, exe, vault, surface);
    if let Some(hint) = existing_client_hint(client, entry) {
        // 2026-09-08: 同じCodexのモデルsuffix違いを「別AI」と誤表示した。
        // surface以外のargvは厳密に照合し、修復時も既存のモデル名を失わない。
        wanted["args"][6] = json!(hint);
    }
    wanted
}

fn valid_target(exe: &Path, vault: &str) -> bool {
    exe.is_absolute()
        && exe
            .to_str()
            .is_some_and(|value| !value.contains(['\0', '\n', '\r']) && !value.contains("${"))
        && !vault.trim().is_empty()
        && !vault.contains(['\0', '\n', '\r'])
        && !vault.contains("${")
}

fn inspect(
    paths: &Paths,
    client: RegistrationClient,
    exe: &Path,
    vault: &str,
) -> RegistrationStatus {
    if !valid_target(exe, vault) {
        return report(client, vec![issue(RegistrationIssueKind::UnsafePath, None)]);
    }
    let mut issues = Vec::new();
    let mut servers = BTreeMap::new();
    match read_config(&paths.config) {
        Ok(text) => match Config::parse(client, text.as_deref()) {
            Ok(config) => {
                if text.is_none() {
                    issues.push(issue(RegistrationIssueKind::MissingConfig, None));
                }
                match config.servers() {
                    Ok(observed) => {
                        diagnose_servers(client, exe, vault, &observed, &mut issues);
                        servers = observed;
                    }
                    Err(kind) => issues.push(issue(kind, None)),
                }
                if config.has_scoped_override() {
                    issues.push(issue(RegistrationIssueKind::ScopedOverride, None));
                }
            }
            Err(kind) => issues.push(issue(kind, None)),
        },
        Err(kind) => issues.push(issue(kind, None)),
    }
    if let Err(kind) = check_policies(paths, client, exe, vault, &servers) {
        issues.push(issue(kind, None));
    }
    report(client, issues)
}

fn diagnose_servers(
    client: RegistrationClient,
    exe: &Path,
    vault: &str,
    servers: &BTreeMap<String, Value>,
    issues: &mut Vec<RegistrationIssue>,
) {
    use RegistrationIssueKind as Kind;
    if servers.contains_key("kb-app") {
        issues.push(issue(Kind::LegacyRegistration, Some("kb-app")));
    }
    for (name, surface) in SERVERS {
        let Some(actual) = servers.get(name) else {
            issues.push(issue(Kind::MissingServer, Some(name)));
            continue;
        };
        if !actual.is_object() {
            issues.push(issue(Kind::InvalidConfig, Some(name)));
            continue;
        }
        let wanted = expected_preserving_client(client, exe, vault, surface, Some(actual));
        if ["command", "type", "url", "experimental_environment"]
            .iter()
            .any(|key| actual.get(key).is_some_and(|value| !value.is_string()))
            || ["enabled", "disabled"]
                .iter()
                .any(|key| actual.get(key).is_some_and(|value| !value.is_boolean()))
            || actual.get("args").is_some_and(|value| {
                value
                    .as_array()
                    .is_none_or(|args| args.iter().any(|arg| !arg.is_string()))
            })
            || actual.get("env").is_some_and(|value| !string_map(value))
            || (client == RegistrationClient::Codex && !codex_optional_fields_valid(actual))
        {
            issues.push(issue(Kind::InvalidConfig, Some(name)));
        }
        if client == RegistrationClient::Codex
            && (actual.get("enabled_tools").is_some()
                || actual
                    .get("disabled_tools")
                    .is_some_and(|value| value.as_array().is_none_or(|tools| !tools.is_empty()))
                || actual.get("omit_tools_from").is_some_and(|value| {
                    value.as_array().is_none_or(|surfaces| !surfaces.is_empty())
                }))
        {
            // 能力一覧は版で変わるため、ユーザーのtool制約を削除して接続成功とは扱わない。
            issues.push(issue(Kind::ToolPolicyRestriction, Some(name)));
        }
        if actual.get("command") != wanted.get("command") {
            issues.push(issue(Kind::ExecutableMismatch, Some(name)));
        }
        if actual.get("args") != wanted.get("args") {
            if unique_option(actual.get("args"), "--vault") != Some(vault) {
                issues.push(issue(Kind::VaultMismatch, Some(name)));
            }
            if existing_client_hint(client, Some(actual)).is_none() {
                issues.push(issue(Kind::ClientMismatch, Some(name)));
            }
            issues.push(issue(Kind::ArgumentsMismatch, Some(name)));
        }
        if actual.get("enabled") == Some(&json!(false))
            || actual.get("disabled") == Some(&json!(true))
        {
            issues.push(issue(Kind::DisabledServer, Some(name)));
        }
        if actual.get("url").is_some()
            || actual.get("type").is_some_and(|value| value != "stdio")
            || actual
                .get("experimental_environment")
                .is_some_and(|value| value != "local")
        {
            issues.push(issue(Kind::ArgumentsMismatch, Some(name)));
        }
    }
}

fn string_map(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|values| values.values().all(Value::is_string))
}

fn string_array(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|values| values.iter().all(Value::is_string))
}

fn codex_approval_mode(value: &Value) -> bool {
    matches!(
        value.as_str(),
        Some("auto" | "prompt" | "writes" | "approve")
    )
}

// 2026-09-08の公式RawMcpServerConfigの既知optionalだけを検査する。
// https://learn.chatgpt.com/docs/config-schema.json
// 他clientの同名拡張へCodexの型を適用せず、未知のtop-levelキーは保存したままにする。
fn codex_optional_fields_valid(entry: &Value) -> bool {
    entry.as_object().is_some_and(|entry| {
        entry.iter().all(|(key, value)| match key.as_str() {
            "bearer_token_env_var"
            | "cwd"
            | "environment_id"
            | "http_headers_helper"
            | "name"
            | "oauth_resource" => value.is_string(),
            "required" | "supports_parallel_tool_calls" => value.is_boolean(),
            "startup_timeout_ms" => value.as_u64().is_some(),
            "startup_timeout_sec" | "tool_timeout_sec" => value.is_number(),
            "enabled_tools" | "disabled_tools" | "scopes" | "omit_tools_from" => {
                string_array(value)
            }
            "env_http_headers" | "http_headers" => string_map(value),
            "auth" => matches!(value.as_str(), Some("oauth" | "chatgpt")),
            "default_tools_approval_mode" => codex_approval_mode(value),
            "env_vars" => value.as_array().is_some_and(|values| {
                values.iter().all(|value| {
                    value.is_string()
                        || value.as_object().is_some_and(|entry| {
                            entry.get("name").is_some_and(Value::is_string)
                                && entry.iter().all(|(key, value)| {
                                    matches!(key.as_str(), "name" | "source") && value.is_string()
                                })
                        })
                })
            }),
            "oauth" => value.as_object().is_some_and(|entry| {
                entry.iter().all(|(key, value)| match key.as_str() {
                    "client_id" | "callback_url" => value.is_string(),
                    "callback_port" => value.as_u64().is_some_and(|port| port <= u16::MAX.into()),
                    _ => false,
                })
            }),
            "tools" => value.as_object().is_some_and(|tools| {
                tools.values().all(|value| {
                    value.as_object().is_some_and(|entry| {
                        entry.iter().all(|(key, value)| match key.as_str() {
                            "approval_mode" => codex_approval_mode(value),
                            "output_token_limit" => value.as_u64().is_some_and(|limit| limit > 0),
                            _ => false,
                        })
                    })
                })
            }),
            _ => true,
        })
    })
}

enum Config {
    Json(Value),
    Toml(DocumentMut),
}

impl Config {
    fn parse(
        client: RegistrationClient,
        bytes: Option<&[u8]>,
    ) -> std::result::Result<Self, RegistrationIssueKind> {
        let text = std::str::from_utf8(bytes.unwrap_or_default())
            .map_err(|_| RegistrationIssueKind::InvalidConfig)?;
        match client {
            RegistrationClient::Codex => text
                .parse::<DocumentMut>()
                .map(Self::Toml)
                .map_err(|_| RegistrationIssueKind::InvalidConfig),
            _ => {
                let value = if bytes.is_none() {
                    json!({})
                } else {
                    unique_json(text)?
                };
                if !value.is_object() {
                    return Err(RegistrationIssueKind::InvalidConfig);
                }
                Ok(Self::Json(value))
            }
        }
    }

    fn servers(&self) -> std::result::Result<BTreeMap<String, Value>, RegistrationIssueKind> {
        let value = match self {
            Self::Json(value) => value.get("mcpServers").cloned(),
            Self::Toml(value) => value.get("mcp_servers").map(toml_value),
        };
        match value {
            None => Ok(BTreeMap::new()),
            Some(Value::Object(value)) => Ok(value.into_iter().collect()),
            _ => Err(RegistrationIssueKind::InvalidConfig),
        }
    }

    fn has_scoped_override(&self) -> bool {
        let overrides = |value: &Value, key: &str| {
            value
                .get(key)
                .and_then(Value::as_object)
                .is_some_and(|servers| servers.keys().any(|name| managed_name(name)))
                || value
                    .get("disabledMcpServers")
                    .and_then(Value::as_array)
                    .is_some_and(|names| names.iter().filter_map(Value::as_str).any(managed_name))
        };
        match self {
            Self::Json(value) => value
                .get("projects")
                .and_then(Value::as_object)
                .is_some_and(|projects| {
                    projects
                        .values()
                        .any(|project| overrides(project, "mcpServers"))
                }),
            Self::Toml(value) => value
                .get("profiles")
                .map(toml_value)
                .and_then(|value| value.as_object().cloned())
                .is_some_and(|profiles| {
                    profiles
                        .values()
                        .any(|profile| overrides(profile, "mcp_servers"))
                }),
        }
    }

    fn updated(&mut self, client: RegistrationClient, exe: &Path, vault: &str) -> Result<Vec<u8>> {
        let original_servers = self
            .servers()
            .map_err(|_| anyhow::anyhow!("registration config invalid"))?;
        let mut text = match self {
            Self::Json(value) => {
                if value.get("mcpServers").is_none() {
                    value["mcpServers"] = json!({});
                }
                let servers = value["mcpServers"]
                    .as_object_mut()
                    .context("registration servers invalid")?;
                servers.remove("kb-app");
                for (name, surface) in SERVERS {
                    let entry = servers
                        .entry(name)
                        .or_insert_with(|| json!({}))
                        .as_object_mut()
                        .context("registration server invalid")?;
                    for key in ["url", "type", "experimental_environment"] {
                        entry.remove(key);
                    }
                    if entry.contains_key("enabled") {
                        entry.insert("enabled".into(), json!(true));
                    }
                    if entry.contains_key("disabled") {
                        entry.insert("disabled".into(), json!(false));
                    }
                    entry.extend(
                        expected_preserving_client(
                            client,
                            exe,
                            vault,
                            surface,
                            original_servers.get(name),
                        )
                        .as_object()
                        .unwrap()
                        .clone(),
                    );
                }
                serde_json::to_string_pretty(value)?
            }
            Self::Toml(value) => {
                if !value.contains_key("mcp_servers") {
                    value["mcp_servers"] = Item::Table(toml_edit::Table::new());
                }
                let table = value["mcp_servers"]
                    .as_table_like_mut()
                    .context("registration servers invalid")?;
                table.remove("kb-app");
                for (name, surface) in SERVERS {
                    if !table.contains_key(name) {
                        table.insert(name, Item::Table(toml_edit::Table::new()));
                    }
                    let entry = table
                        .get_mut(name)
                        .and_then(Item::as_table_like_mut)
                        .context("registration server invalid")?;
                    // TOMLの型・未知の入れ子・コメントをJSON往復で失わず、所有するキーだけ更新する。
                    for key in ["url", "type", "experimental_environment"] {
                        entry.remove(key);
                    }
                    if entry.contains_key("enabled") {
                        entry.insert("enabled", toml_edit::value(true));
                    }
                    if entry.contains_key("disabled") {
                        entry.insert("disabled", toml_edit::value(false));
                    }
                    entry.insert(
                        "command",
                        toml_edit::value(exe.to_str().context("registration executable invalid")?),
                    );
                    let args = [
                        "--mcp",
                        "--mcp-surface",
                        surface,
                        "--vault",
                        vault,
                        "--client",
                        existing_client_hint(client, original_servers.get(name))
                            .unwrap_or(client.actor()),
                    ]
                    .into_iter()
                    .collect::<toml_edit::Array>();
                    entry.insert("args", toml_edit::value(args));
                }
                value.to_string()
            }
        };
        if !text.ends_with('\n') {
            text.push('\n');
        }
        Ok(text.into_bytes())
    }
}

fn managed_name(name: &str) -> bool {
    name == "kb-app" || SERVERS.iter().any(|(expected, _)| name == *expected)
}

fn toml_value(item: &Item) -> Value {
    if let Some(table) = item.as_table_like() {
        return Value::Object(
            table
                .iter()
                .map(|(key, item)| (key.to_owned(), toml_value(item)))
                .collect(),
        );
    }
    if let Some(array) = item.as_array_of_tables() {
        return Value::Array(
            array
                .iter()
                .map(|table| toml_value(&Item::Table(table.clone())))
                .collect(),
        );
    }
    match item.as_value() {
        Some(toml_edit::Value::String(v)) => json!(v.value()),
        Some(toml_edit::Value::Integer(v)) => json!(v.value()),
        Some(toml_edit::Value::Float(v)) => json!(v.value()),
        Some(toml_edit::Value::Boolean(v)) => json!(v.value()),
        Some(toml_edit::Value::Array(v)) => Value::Array(
            v.iter()
                .map(|value| toml_value(&Item::Value(value.clone())))
                .collect(),
        ),
        // 診断時にdatetimeを文字列として通すと、env等の既知キーの型不正を見逃す。
        // 保存は元DocumentMutを編集するので未知キーのdatetimeはそのまま残る。
        Some(toml_edit::Value::Datetime(_)) => Value::Null,
        _ => Value::Null,
    }
}

fn safe_path(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        && path
            .ancestors()
            .all(|part| match fs::symlink_metadata(part) {
                Ok(metadata) => !metadata.file_type().is_symlink(),
                Err(error) => error.kind() == std::io::ErrorKind::NotFound,
            })
}

fn read_config(path: &Path) -> std::result::Result<Option<Vec<u8>>, RegistrationIssueKind> {
    use RegistrationIssueKind as Kind;
    if !safe_path(path) {
        return Err(Kind::UnsafePath);
    }
    // FIFOはopen自体が待機する。既知の非通常ファイルは読み始める前に拒否する。
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => return Err(Kind::UnsafePath),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Kind::UnreadableConfig),
    }
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Kind::UnreadableConfig),
    };
    let metadata = file.metadata().map_err(|_| Kind::UnreadableConfig)?;
    if !metadata.is_file() {
        return Err(Kind::UnsafePath);
    }
    if metadata.len() > MAX_CONFIG_BYTES {
        return Err(Kind::InvalidConfig);
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Kind::UnreadableConfig)?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(Kind::InvalidConfig);
    }
    Ok(Some(bytes))
}

fn repair_at(
    paths: &Paths,
    client: RegistrationClient,
    exe: &Path,
    vault: &str,
) -> Result<RegistrationRepair> {
    let before = inspect(paths, client, exe, vault);
    if before.state == RegistrationState::Registered {
        return Ok(RegistrationRepair {
            status: before,
            changed: false,
            backup_path: None,
        });
    }
    ensure!(
        before.can_repair,
        "registration is blocked: {:?}",
        before.issues
    );
    let parent = paths
        .config
        .parent()
        .context("registration config parent missing")?;
    fs::create_dir_all(parent)?;
    let lock_path = parent.join(".kb-app-registration.lock");
    ensure!(safe_path(&lock_path), "registration lock path unsafe");
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock.lock_exclusive()?;
    let checked = inspect(paths, client, exe, vault);
    ensure!(
        checked.can_repair || checked.state == RegistrationState::Registered,
        "registration changed or became blocked"
    );
    if checked.state == RegistrationState::Registered {
        return Ok(RegistrationRepair {
            status: checked,
            changed: false,
            backup_path: None,
        });
    }
    let original = read_config(&paths.config)
        .map_err(|_| anyhow::anyhow!("registration config unreadable"))?;
    let mut parsed = Config::parse(client, original.as_deref())
        .map_err(|_| anyhow::anyhow!("registration config invalid"))?;
    ensure!(
        !parsed.has_scoped_override(),
        "registration scoped override appeared"
    );
    let mut latest_issues = Vec::new();
    let servers = parsed
        .servers()
        .map_err(|_| anyhow::anyhow!("registration config invalid"))?;
    diagnose_servers(client, exe, vault, &servers, &mut latest_issues);
    ensure!(
        report(client, latest_issues).state != RegistrationState::Blocked,
        "registration became blocked"
    );
    let updated = parsed.updated(client, exe, vault)?;
    let backup_path = replace_config(&paths.config, original.as_deref(), &updated, || {
        check_policies(paths, client, exe, vault, &servers)
            .map_err(|_| anyhow::anyhow!("registration policy changed"))
    })?;
    let status = inspect(paths, client, exe, vault);
    ensure!(
        status.state == RegistrationState::Registered,
        "registration verification failed after write"
    );
    Ok(RegistrationRepair {
        status,
        changed: true,
        backup_path: backup_path.map(|path| path.to_string_lossy().into_owned()),
    })
}

// 他クライアントはこのadvisory lockに従うとは限らない。置換直前に再読するが、
// 最後の比較とrename間の外部編集をOSのcompare-and-swapとして保証するものではない。
fn replace_config(
    path: &Path,
    original: Option<&[u8]>,
    updated: &[u8],
    recheck_policy: impl FnOnce() -> Result<()>,
) -> Result<Option<PathBuf>> {
    let parent = path
        .parent()
        .context("registration config parent missing")?;
    let mut replacement = tempfile::NamedTempFile::new_in(parent)?;
    replacement.write_all(updated)?;
    replacement.as_file().sync_all()?;
    recheck_policy()?;
    ensure_unchanged(path, original)?;
    let backup = if let Some(original) = original {
        let prefix = format!(
            "{}.bak-kbapp-",
            path.file_name()
                .and_then(|s| s.to_str())
                .context("registration filename invalid")?
        );
        let mut backup = tempfile::Builder::new()
            .prefix(&prefix)
            .tempfile_in(parent)?;
        backup.write_all(original)?;
        backup.as_file().sync_all()?;
        Some(backup.keep()?.1)
    } else {
        None
    };
    ensure_unchanged(path, original)?;
    replacement.persist(path)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(backup)
}

fn ensure_unchanged(path: &Path, original: Option<&[u8]>) -> Result<()> {
    let current = read_config(path)
        .map_err(|_| anyhow::anyhow!("registration config unreadable before replacement"))?;
    ensure!(
        current.as_deref() == original,
        "registration config changed before replacement"
    );
    Ok(())
}

fn check_policies(
    paths: &Paths,
    client: RegistrationClient,
    exe: &Path,
    vault: &str,
    registrations: &BTreeMap<String, Value>,
) -> std::result::Result<(), RegistrationIssueKind> {
    use RegistrationIssueKind as Kind;
    if let Some(path) = &paths.codex_requirements
        && let Some(bytes) = read_config(path).map_err(|_| Kind::ManagedPolicyUnverified)?
    {
        let Config::Toml(value) = Config::parse(RegistrationClient::Codex, Some(&bytes))
            .map_err(|_| Kind::ManagedPolicyUnverified)?
        else {
            unreachable!()
        };
        if let Some(servers) = value.get("mcp_servers") {
            let servers = toml_value(servers);
            let servers = servers.as_object().ok_or(Kind::ManagedPolicyUnverified)?;
            for (name, surface) in SERVERS {
                let command = servers
                    .get(name)
                    .and_then(|entry| entry.pointer("/identity/command"))
                    .ok_or(Kind::ManagedPolicyConflict)?;
                let expected = expected_preserving_client(
                    client,
                    exe,
                    vault,
                    surface,
                    registrations.get(name),
                );
                if !codex_identity_matches(command, &expected)? {
                    return Err(Kind::ManagedPolicyConflict);
                }
            }
        }
    }
    if let Some(path) = &paths.codex_managed_config
        && let Some(bytes) = read_config(path).map_err(|_| Kind::ManagedPolicyUnverified)?
    {
        let config = Config::parse(RegistrationClient::Codex, Some(&bytes))
            .map_err(|_| Kind::ManagedPolicyUnverified)?;
        if config
            .servers()
            .map_err(|_| Kind::ManagedPolicyUnverified)?
            .keys()
            .any(|name| managed_name(name))
        {
            return Err(Kind::ManagedPolicyConflict);
        }
    }
    if let Some(path) = &paths.claude_managed_mcp
        && read_config(path)
            .map_err(|_| Kind::ManagedPolicyUnverified)?
            .is_some()
    {
        return Err(Kind::ManagedPolicyConflict);
    }
    let mut settings = paths.claude_settings.clone();
    if let Some(directory) = &paths.claude_managed_directory {
        if !safe_path(directory) {
            return Err(Kind::ManagedPolicyUnverified);
        }
        match fs::read_dir(directory) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(|_| Kind::ManagedPolicyUnverified)?;
                    if entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "json")
                    {
                        settings.push((entry.path(), true));
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(Kind::ManagedPolicyUnverified),
        }
    }
    let mut policies = Vec::new();
    for (path, managed) in settings {
        let Some(bytes) = read_config(&path).map_err(|_| Kind::ManagedPolicyUnverified)? else {
            continue;
        };
        let value =
            unique_json(std::str::from_utf8(&bytes).map_err(|_| Kind::ManagedPolicyUnverified)?)
                .map_err(|_| Kind::ManagedPolicyUnverified)?;
        policies.push((value, managed));
    }
    let policy = merge_claude_policies(&policies)?;
    for (name, surface) in SERVERS {
        claude_policy_allows(
            &policy,
            name,
            &expected_preserving_client(client, exe, vault, surface, registrations.get(name)),
        )?;
    }
    Ok(())
}

fn merge_claude_policies(
    policies: &[(Value, bool)],
) -> std::result::Result<Value, RegistrationIssueKind> {
    use RegistrationIssueKind as Kind;
    let mut managed_only = false;
    let mut strict = false;
    for (policy, managed) in policies {
        if !policy.is_object() {
            return Err(Kind::ManagedPolicyUnverified);
        }
        if let Some(value) = policy.get("allowManagedMcpServersOnly") {
            let value = value.as_bool().ok_or(Kind::ManagedPolicyUnverified)?;
            managed_only |= *managed && value;
        }
        if let Some(value) = policy.get("strictPluginOnlyCustomization") {
            if let Some(value) = value.as_bool() {
                strict |= value;
            } else if let Some(values) = value.as_array() {
                if values.iter().any(|value| !value.is_string()) {
                    return Err(Kind::ManagedPolicyUnverified);
                }
                strict |= values.iter().any(|value| value == "mcp");
            } else {
                return Err(Kind::ManagedPolicyUnverified);
            }
        }
    }
    let mut merged = json!({"strictPluginOnlyCustomization": strict});
    for key in ["allowedMcpServers", "deniedMcpServers"] {
        let mut present = false;
        let mut entries = Vec::new();
        for (policy, managed) in policies {
            if key == "allowedMcpServers" && managed_only && !managed {
                continue;
            }
            if let Some(values) = policy.get(key) {
                present = true;
                entries.extend(
                    values
                        .as_array()
                        .ok_or(Kind::ManagedPolicyUnverified)?
                        .iter()
                        .cloned(),
                );
            }
        }
        // 未指定と空配列は別の意味。管理限定でも、存在しないallowlistを空配列へ変えない。
        if present {
            merged[key] = Value::Array(entries);
        }
    }
    Ok(merged)
}

fn codex_identity_matches(
    rule: &Value,
    expected: &Value,
) -> std::result::Result<bool, RegistrationIssueKind> {
    use RegistrationIssueKind as Kind;
    if let Some(command) = rule.as_str() {
        return Ok(Some(command) == expected["command"].as_str());
    }
    let rule = rule.as_object().ok_or(Kind::ManagedPolicyUnverified)?;
    if rule.get("executable") != expected.get("command") {
        return Ok(false);
    }
    let rules = rule
        .get("args")
        .and_then(Value::as_array)
        .ok_or(Kind::ManagedPolicyUnverified)?;
    let args = expected["args"]
        .as_array()
        .ok_or(Kind::ManagedPolicyUnverified)?;
    if rules.len() != args.len() {
        return Ok(false);
    }
    for (rule, arg) in rules.iter().zip(args) {
        let value = rule
            .get("value")
            .and_then(Value::as_str)
            .ok_or(Kind::ManagedPolicyUnverified)?;
        let actual = arg.as_str().ok_or(Kind::ManagedPolicyUnverified)?;
        let matches = match rule.get("match").and_then(Value::as_str) {
            Some("exact") => actual == value,
            Some("prefix") => actual.starts_with(value),
            _ => return Err(Kind::ManagedPolicyUnverified),
        };
        if !matches {
            return Ok(false);
        }
    }
    Ok(true)
}

fn claude_policy_allows(
    policy: &Value,
    name: &str,
    expected: &Value,
) -> std::result::Result<(), RegistrationIssueKind> {
    use RegistrationIssueKind as Kind;
    if !policy.is_object() {
        return Err(Kind::ManagedPolicyUnverified);
    }
    if policy
        .get("strictPluginOnlyCustomization")
        .is_some_and(|v| {
            v == &json!(true) || v.as_array().is_some_and(|a| a.iter().any(|v| v == "mcp"))
        })
    {
        return Err(Kind::ManagedPolicyConflict);
    }
    let mut command = vec![expected["command"].clone()];
    command.extend(expected["args"].as_array().unwrap().iter().cloned());
    let matches = |rule: &Value| -> std::result::Result<bool, RegistrationIssueKind> {
        let rule = rule
            .as_object()
            .filter(|rule| rule.len() == 1)
            .ok_or(Kind::ManagedPolicyUnverified)?;
        if let Some(value) = rule.get("serverName") {
            return value
                .as_str()
                .map(|value| value == name)
                .ok_or(Kind::ManagedPolicyUnverified);
        }
        if let Some(value) = rule.get("serverCommand") {
            let values = value.as_array().ok_or(Kind::ManagedPolicyUnverified)?;
            if values
                .iter()
                .any(|v| v.as_str().is_none_or(|v| v.contains("${")))
            {
                return Err(Kind::ManagedPolicyUnverified);
            }
            return Ok(values == &command);
        }
        if rule.get("serverUrl").and_then(Value::as_str).is_some() {
            return Ok(false);
        }
        Err(Kind::ManagedPolicyUnverified)
    };
    if let Some(denied) = policy.get("deniedMcpServers") {
        for rule in denied.as_array().ok_or(Kind::ManagedPolicyUnverified)? {
            if matches(rule)? {
                return Err(Kind::ManagedPolicyConflict);
            }
        }
    }
    if let Some(allowed) = policy.get("allowedMcpServers") {
        let allowed = allowed.as_array().ok_or(Kind::ManagedPolicyUnverified)?;
        let has_commands = allowed
            .iter()
            .any(|rule| rule.get("serverCommand").is_some());
        let mut permitted = false;
        for rule in allowed {
            let matched = matches(rule)?;
            if !has_commands || rule.get("serverCommand").is_some() {
                permitted |= matched;
            }
        }
        if !permitted {
            return Err(Kind::ManagedPolicyConflict);
        }
    }
    Ok(())
}

// serde_jsonの既定の後勝ちparseでは、重複キーを含む設定の片側を修復時に消してしまう。
fn unique_json(text: &str) -> std::result::Result<Value, RegistrationIssueKind> {
    struct Unique(Value);
    impl<'de> Deserialize<'de> for Unique {
        fn deserialize<D: serde::Deserializer<'de>>(
            deserializer: D,
        ) -> std::result::Result<Self, D::Error> {
            struct Visitor;
            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = Value;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("JSON without duplicate keys")
                }
                fn visit_map<M: serde::de::MapAccess<'de>>(
                    self,
                    mut map: M,
                ) -> std::result::Result<Value, M::Error> {
                    let mut out = Map::new();
                    while let Some((key, Unique(value))) = map.next_entry::<String, Unique>()? {
                        if out.insert(key, value).is_some() {
                            return Err(serde::de::Error::custom("duplicate JSON key"));
                        }
                    }
                    Ok(Value::Object(out))
                }
                fn visit_seq<S: serde::de::SeqAccess<'de>>(
                    self,
                    mut seq: S,
                ) -> std::result::Result<Value, S::Error> {
                    let mut out = Vec::new();
                    while let Some(Unique(value)) = seq.next_element()? {
                        out.push(value);
                    }
                    Ok(Value::Array(out))
                }
                fn visit_bool<E: serde::de::Error>(self, v: bool) -> std::result::Result<Value, E> {
                    Ok(json!(v))
                }
                fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Value, E> {
                    Ok(json!(v))
                }
                fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Value, E> {
                    Ok(json!(v))
                }
                fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Value, E> {
                    Ok(json!(v))
                }
                fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Value, E> {
                    Ok(json!(v))
                }
                fn visit_string<E: serde::de::Error>(
                    self,
                    v: String,
                ) -> std::result::Result<Value, E> {
                    Ok(json!(v))
                }
                fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Value, E> {
                    Ok(Value::Null)
                }
            }
            deserializer.deserialize_any(Visitor).map(Unique)
        }
    }
    serde_json::from_str::<Unique>(text)
        .map(|value| value.0)
        .map_err(|_| RegistrationIssueKind::InvalidConfig)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _temporary: tempfile::TempDir,
        root: PathBuf,
        paths: Paths,
        client: RegistrationClient,
        exe: PathBuf,
    }

    impl Fixture {
        fn new(client: RegistrationClient) -> Self {
            let temporary = tempfile::tempdir().unwrap();
            // macOSの/var→/private/varを取り除き、symlink拒否自体は別の試験で確認する。
            let root = temporary.path().canonicalize().unwrap();
            let paths = Paths {
                config: root.join(if client == RegistrationClient::Codex {
                    "config.toml"
                } else {
                    "config.json"
                }),
                ..Default::default()
            };
            Self {
                _temporary: temporary,
                exe: root.join("Applications/Example App/kb-app"),
                root,
                paths,
                client,
            }
        }

        fn write(&self, bytes: impl AsRef<[u8]>) {
            fs::write(&self.paths.config, bytes).unwrap();
        }
        fn status(&self) -> RegistrationStatus {
            inspect(&self.paths, self.client, &self.exe, "Synthetic Vault")
        }
        fn repair(&self) -> Result<RegistrationRepair> {
            repair_at(&self.paths, self.client, &self.exe, "Synthetic Vault")
        }
        fn json(&self) -> Value {
            serde_json::from_slice(&fs::read(&self.paths.config).unwrap()).unwrap()
        }

        fn set_codex_options(&self, options: &str) {
            let mut config: DocumentMut = fs::read_to_string(&self.paths.config)
                .unwrap()
                .parse()
                .unwrap();
            let options: DocumentMut = options.parse().unwrap();
            for (key, value) in options.iter() {
                config["mcp_servers"]["kb-app-read"][key] = value.clone();
            }
            self.write(config.to_string());
        }

        fn servers(&self) -> BTreeMap<String, Value> {
            Config::parse(self.client, Some(&fs::read(&self.paths.config).unwrap()))
                .unwrap()
                .servers()
                .unwrap()
        }

        fn set_args(&self, name: &str, args: &[&str]) {
            if self.client == RegistrationClient::Codex {
                let mut config: DocumentMut = fs::read_to_string(&self.paths.config)
                    .unwrap()
                    .parse()
                    .unwrap();
                config["mcp_servers"][name]["args"] =
                    toml_edit::value(args.iter().copied().collect::<toml_edit::Array>());
                self.write(config.to_string());
            } else {
                let mut config = self.json();
                config["mcpServers"][name]["args"] = json!(args);
                self.write(serde_json::to_vec(&config).unwrap());
            }
        }
    }

    fn contains(status: &RegistrationStatus, kind: RegistrationIssueKind) -> bool {
        status.issues.iter().any(|issue| issue.kind == kind)
    }

    #[test]
    fn missing_registration_is_read_only_then_repairs_exact_three_surfaces_for_each_client() {
        for client in [
            RegistrationClient::Codex,
            RegistrationClient::ClaudeCode,
            RegistrationClient::ClaudeDesktop,
        ] {
            let fixture = Fixture::new(client);
            assert_eq!(fixture.status().state, RegistrationState::Missing);
            assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
            let repaired = fixture.repair().unwrap();
            assert!(repaired.changed);
            assert!(repaired.backup_path.is_none());
            assert_eq!(repaired.status.state, RegistrationState::Registered);
            let text = fs::read(&fixture.paths.config).unwrap();
            let servers = Config::parse(client, Some(&text))
                .unwrap()
                .servers()
                .unwrap();
            assert_eq!(servers.len(), 3);
            for (name, surface) in SERVERS {
                assert_eq!(
                    servers[name],
                    expected(client, &fixture.exe, "Synthetic Vault", surface)
                );
            }
            let second = fixture.repair().unwrap();
            assert!(!second.changed);
            assert!(second.backup_path.is_none());
            assert_eq!(fs::read(&fixture.paths.config).unwrap(), text);
        }
    }

    /// 2026-09-08: 実MCPと同surfaceのモデルsuffix/actor aliasを「別AI」と誤表示しない。
    #[test]
    fn same_surface_aliases_and_model_suffixes_are_registered_without_rewriting() {
        for (client, hints) in [
            (
                RegistrationClient::Codex,
                [
                    "codex-cli/synthetic-model-v2",
                    "codex/synthetic-model-v3",
                    "CODEX-CLI/synthetic-next",
                ],
            ),
            (
                RegistrationClient::ClaudeCode,
                [
                    "claude-code/synthetic-model-v2",
                    "claude-code/synthetic-model-v3",
                    "CLAUDE-CODE/synthetic-next",
                ],
            ),
            (
                RegistrationClient::ClaudeDesktop,
                [
                    "claude-desktop/synthetic-model-v2",
                    "claude-desktop/synthetic-model-v3",
                    "CLAUDE-DESKTOP/synthetic-next",
                ],
            ),
        ] {
            let fixture = Fixture::new(client);
            fixture.repair().unwrap();
            for ((name, surface), hint) in SERVERS.into_iter().zip(hints) {
                fixture.set_args(
                    name,
                    &[
                        "--mcp",
                        "--mcp-surface",
                        surface,
                        "--vault",
                        "Synthetic Vault",
                        "--client",
                        hint,
                    ],
                );
            }
            let original = fs::read(&fixture.paths.config).unwrap();
            let status = fixture.status();
            assert_eq!(
                status.state,
                RegistrationState::Registered,
                "{client:?}: {status:?}"
            );
            assert!(status.issues.is_empty());
            assert!(!fixture.repair().unwrap().changed);
            assert_eq!(fs::read(&fixture.paths.config).unwrap(), original);
        }
    }

    /// 2026-09-08: Vaultや旧引数を修復しても、同じAIのモデルhintを既定名へ戻さない。
    #[test]
    fn repair_preserves_valid_model_hints_but_keeps_other_arguments_strict() {
        for (client, hint) in [
            (RegistrationClient::Codex, "codex/synthetic-model-v2"),
            (
                RegistrationClient::ClaudeCode,
                "claude-code/synthetic-model-v2",
            ),
            (
                RegistrationClient::ClaudeDesktop,
                "claude-desktop/synthetic-model-v2",
            ),
        ] {
            let fixture = Fixture::new(client);
            fixture.repair().unwrap();
            for (name, surface) in SERVERS {
                fixture.set_args(
                    name,
                    &[
                        "--mcp",
                        "--mcp-surface",
                        surface,
                        "--vault",
                        "Other Vault",
                        "--client",
                        hint,
                    ],
                );
            }
            let status = fixture.status();
            assert_eq!(status.state, RegistrationState::NeedsRepair);
            assert!(contains(&status, RegistrationIssueKind::VaultMismatch));
            assert!(contains(&status, RegistrationIssueKind::ArgumentsMismatch));
            assert!(!contains(&status, RegistrationIssueKind::ClientMismatch));
            fixture.repair().unwrap();
            for (name, surface) in SERVERS {
                let mut wanted = expected(client, &fixture.exe, "Synthetic Vault", surface);
                wanted["args"][6] = json!(hint);
                assert_eq!(fixture.servers()[name], wanted);
            }

            for malformed in [
                vec![
                    "--mcp",
                    "--vault",
                    "Synthetic Vault",
                    "--mcp-surface",
                    "read",
                    "--client",
                    hint,
                ],
                vec![
                    "--mcp",
                    "--mcp-surface",
                    "write",
                    "--vault",
                    "Synthetic Vault",
                    "--client",
                    hint,
                ],
                vec![
                    "--mcp",
                    "--mcp-surface",
                    "read",
                    "--vault",
                    "Synthetic Vault",
                    "--client",
                    hint,
                    "--extra",
                ],
                vec![
                    "--mcp",
                    "--mcp-surface",
                    "read",
                    "--vault",
                    "Synthetic Vault",
                    "--client",
                    hint,
                    "--vault",
                    "Synthetic Vault",
                ],
            ] {
                fixture.set_args("kb-app-read", &malformed);
                let status = fixture.status();
                assert_eq!(status.state, RegistrationState::NeedsRepair);
                assert!(contains(&status, RegistrationIssueKind::ArgumentsMismatch));
                assert!(!contains(&status, RegistrationIssueKind::ClientMismatch));
                fixture.repair().unwrap();
                assert_eq!(fixture.servers()["kb-app-read"]["args"][6], hint);
            }
        }
    }

    /// 2026-09-08: model名に相手AI名が含まれていても、別surface/unknown/重複hintは通さない。
    #[test]
    fn distinct_or_unknown_surfaces_and_duplicate_client_flags_still_require_repair() {
        for (client, other, valid) in [
            (
                RegistrationClient::Codex,
                "chatgpt/synthetic-codex",
                "codex/synthetic-model",
            ),
            (
                RegistrationClient::ClaudeCode,
                "claude-desktop/synthetic-claude",
                "claude-code/synthetic-model",
            ),
            (
                RegistrationClient::ClaudeDesktop,
                "claude-code/synthetic-claude",
                "claude-desktop/synthetic-model",
            ),
        ] {
            let fixture = Fixture::new(client);
            fixture.repair().unwrap();
            for hint in [
                other,
                "unknown/synthetic-codex-claude",
                "",
                "codex/synthetic\nmodel",
            ] {
                fixture.set_args(
                    "kb-app-read",
                    &[
                        "--mcp",
                        "--mcp-surface",
                        "read",
                        "--vault",
                        "Synthetic Vault",
                        "--client",
                        hint,
                    ],
                );
                let status = fixture.status();
                assert_eq!(status.state, RegistrationState::NeedsRepair);
                assert!(contains(&status, RegistrationIssueKind::ClientMismatch));
                assert!(contains(&status, RegistrationIssueKind::ArgumentsMismatch));
                fixture.repair().unwrap();
                assert_eq!(fixture.servers()["kb-app-read"]["args"][6], client.actor());
            }
            for suffix in [vec!["--client", valid], vec!["--client"]] {
                let mut args = vec![
                    "--mcp",
                    "--mcp-surface",
                    "read",
                    "--vault",
                    "Synthetic Vault",
                    "--client",
                    valid,
                ];
                args.extend(suffix);
                fixture.set_args("kb-app-read", &args);
                let status = fixture.status();
                assert!(contains(&status, RegistrationIssueKind::ClientMismatch));
                assert!(contains(&status, RegistrationIssueKind::ArgumentsMismatch));
                assert_ne!(status.state, RegistrationState::Registered);
                fixture.repair().unwrap();
                assert_eq!(fixture.servers()["kb-app-read"]["args"][6], client.actor());
            }
        }
    }

    /// 2026-09-08: 同surface判定で管理ポリシーのexact argv制約まで緩めない。
    #[test]
    fn policies_match_the_preserved_hint_and_still_reject_a_different_exact_command() {
        for (client, hint) in [
            (RegistrationClient::Codex, "codex/synthetic-model-v2"),
            (
                RegistrationClient::ClaudeCode,
                "claude-code/synthetic-model-v2",
            ),
        ] {
            let mut fixture = Fixture::new(client);
            fixture.repair().unwrap();
            for (name, surface) in SERVERS {
                fixture.set_args(
                    name,
                    &[
                        "--mcp",
                        "--mcp-surface",
                        surface,
                        "--vault",
                        "Synthetic Vault",
                        "--client",
                        hint,
                    ],
                );
            }
            let policy = fixture.root.join(if client == RegistrationClient::Codex {
                "requirements.toml"
            } else {
                "managed-settings.json"
            });
            let write_policy = |policy_hint: &str| {
                if client == RegistrationClient::Codex {
                    let mut doc = DocumentMut::new();
                    for (name, surface) in SERVERS {
                        doc["mcp_servers"][name]["identity"]["command"]["executable"] =
                            toml_edit::value(fixture.exe.to_str().unwrap());
                        let mut args = toml_edit::Array::new();
                        for arg in [
                            "--mcp",
                            "--mcp-surface",
                            surface,
                            "--vault",
                            "Synthetic Vault",
                            "--client",
                            policy_hint,
                        ] {
                            let mut rule = toml_edit::InlineTable::new();
                            rule.insert("match", toml_edit::Value::from("exact"));
                            rule.insert("value", toml_edit::Value::from(arg));
                            args.push(toml_edit::Value::InlineTable(rule));
                        }
                        doc["mcp_servers"][name]["identity"]["command"]["args"] =
                            toml_edit::value(args);
                    }
                    fs::write(&policy, doc.to_string()).unwrap();
                } else {
                    let allowed = SERVERS.iter().map(|(_, surface)| json!({"serverCommand":[fixture.exe.to_str().unwrap(), "--mcp", "--mcp-surface", surface, "--vault", "Synthetic Vault", "--client", policy_hint]})).collect::<Vec<_>>();
                    fs::write(
                        &policy,
                        serde_json::to_vec(&json!({"allowedMcpServers": allowed})).unwrap(),
                    )
                    .unwrap();
                }
            };
            write_policy(hint);
            if client == RegistrationClient::Codex {
                fixture.paths.codex_requirements = Some(policy.clone());
            } else {
                fixture.paths.claude_settings = vec![(policy.clone(), true)];
            }
            assert_eq!(fixture.status().state, RegistrationState::Registered);
            fixture.set_args(
                "kb-app-read",
                &[
                    "--mcp",
                    "--mcp-surface",
                    "read",
                    "--vault",
                    "Other Vault",
                    "--client",
                    hint,
                ],
            );
            assert!(fixture.status().can_repair);
            fixture.repair().unwrap();
            assert_eq!(fixture.servers()["kb-app-read"]["args"][6], hint);
            write_policy(client.actor());
            let status = fixture.status();
            assert_eq!(status.state, RegistrationState::Blocked);
            assert!(contains(
                &status,
                RegistrationIssueKind::ManagedPolicyConflict
            ));
            let original = fs::read(&fixture.paths.config).unwrap();
            assert!(fixture.repair().is_err());
            assert_eq!(fs::read(&fixture.paths.config).unwrap(), original);
        }
    }

    #[test]
    fn toml_repair_preserves_unrelated_text_comments_and_unknown_nested_types() {
        let fixture = Fixture::new(RegistrationClient::Codex);
        let original = r#"# synthetic configuration
model = "example-model" # keep this comment

[mcp_servers.other]
command = "other-service"
env = { SYNTHETIC_TOKEN = "fixture-only" }

[mcp_servers.kb-app]
command = "/old/kb-app"

[mcp_servers.kb-app-read]
command = "/old/kb-app"
args = ["--mcp"]
enabled = false
# preserve the setting and its comment
tool_timeout_sec = 120
custom_date = 2025-01-02T03:04:05Z
custom_nested = [{ values = [[1, 2], [3]], detail = { key = "value" } }]

[mcp_servers.kb-app-read.tools.kb_search]
approval_mode = "prompt" # preserved
"#;
        fixture.write(original);
        let result = fixture.repair().unwrap();
        let backup = result.backup_path.unwrap();
        assert_eq!(fs::read(&backup).unwrap(), original.as_bytes());
        let updated = fs::read_to_string(&fixture.paths.config).unwrap();
        for retained in [
            "# synthetic configuration\nmodel = \"example-model\" # keep this comment",
            "[mcp_servers.other]\ncommand = \"other-service\"\nenv = { SYNTHETIC_TOKEN = \"fixture-only\" }",
            "# preserve the setting and its comment\ntool_timeout_sec = 120",
            "custom_date = 2025-01-02T03:04:05Z",
            "custom_nested = [{ values = [[1, 2], [3]], detail = { key = \"value\" } }]",
            "approval_mode = \"prompt\" # preserved",
        ] {
            assert!(updated.contains(retained), "lost TOML text: {retained}");
        }
        let doc: DocumentMut = updated.parse().unwrap();
        assert!(
            !doc["mcp_servers"]
                .as_table()
                .unwrap()
                .contains_key("kb-app")
        );
        assert_eq!(
            doc["mcp_servers"]["kb-app-read"]["enabled"].as_bool(),
            Some(true)
        );
        assert!(
            doc["mcp_servers"]["kb-app-read"]["custom_date"]
                .as_datetime()
                .is_some()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(&fixture.paths.config)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn json_repair_preserves_unrelated_state_and_server_options_and_keeps_exact_backup() {
        for client in [
            RegistrationClient::ClaudeCode,
            RegistrationClient::ClaudeDesktop,
        ] {
            let fixture = Fixture::new(client);
            let original = json!({"theme":"dark", "projects":{"/synthetic/project":{"trusted":true}}, "mcpServers":{
                "other":{"command":"other", "env":{"SYNTHETIC_TOKEN":"fixture-only"}},
                "kb-app":{"command":"old"},
                "kb-app-read":{"command":"old", "args":[], "env":{"EXAMPLE":"value"},"unknown":{"nested":[[1],[2]]}}
            }});
            let text = serde_json::to_vec(&original).unwrap();
            fixture.write(&text);
            let result = fixture.repair().unwrap();
            assert_eq!(fs::read(result.backup_path.unwrap()).unwrap(), text);
            let actual = fixture.json();
            for pointer in [
                "/theme",
                "/projects",
                "/mcpServers/other",
                "/mcpServers/kb-app-read/env",
                "/mcpServers/kb-app-read/unknown",
            ] {
                assert_eq!(actual.pointer(pointer), original.pointer(pointer));
            }
            assert!(actual["mcpServers"].get("kb-app").is_none());
        }
    }

    #[test]
    fn old_executable_other_vault_and_other_client_are_distinct_diagnostics() {
        let fixture = Fixture::new(RegistrationClient::ClaudeCode);
        fixture.repair().unwrap();
        let correct = fixture.json();
        for (field, value, expected_kind) in [
            (
                "command",
                json!("/old/kb-app"),
                RegistrationIssueKind::ExecutableMismatch,
            ),
            (
                "vault",
                json!("Another Vault"),
                RegistrationIssueKind::VaultMismatch,
            ),
            (
                "client",
                json!("claude-desktop/claude"),
                RegistrationIssueKind::ClientMismatch,
            ),
        ] {
            let mut changed = correct.clone();
            let entry = &mut changed["mcpServers"]["kb-app-read"];
            match field {
                "command" => entry["command"] = value,
                "vault" => entry["args"][4] = value,
                _ => entry["args"][6] = value,
            }
            fixture.write(serde_json::to_vec(&changed).unwrap());
            let status = fixture.status();
            assert_eq!(status.state, RegistrationState::NeedsRepair);
            for kind in [
                RegistrationIssueKind::ExecutableMismatch,
                RegistrationIssueKind::VaultMismatch,
                RegistrationIssueKind::ClientMismatch,
            ] {
                assert_eq!(
                    contains(&status, kind),
                    kind == expected_kind,
                    "{field}: {kind:?}"
                );
            }
            assert_eq!(
                fixture.repair().unwrap().status.state,
                RegistrationState::Registered
            );
        }
    }

    #[test]
    fn malformed_or_duplicate_configuration_is_never_overwritten() {
        for (client, samples) in [
            (
                RegistrationClient::ClaudeCode,
                vec![
                    "{broken",
                    "[]",
                    "{\"mcpServers\":null}",
                    "{\"mcpServers\":{},\"mcpServers\":{}}",
                    "{\"mcpServers\":{\"kb-app-read\":{\"args\":[1]}}}",
                ],
            ),
            (
                RegistrationClient::Codex,
                vec![
                    "[unfinished",
                    "mcp_servers = 4",
                    "[mcp_servers.kb-app-read]\ncommand = 2",
                    "[mcp_servers]\nkb-app-read = [1]",
                ],
            ),
        ] {
            for text in samples {
                let fixture = Fixture::new(client);
                fixture.write(text);
                assert_eq!(fixture.status().state, RegistrationState::Blocked, "{text}");
                assert!(contains(
                    &fixture.status(),
                    RegistrationIssueKind::InvalidConfig
                ));
                assert!(fixture.repair().is_err());
                assert_eq!(fs::read(&fixture.paths.config).unwrap(), text.as_bytes());
                assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 1);
            }
        }
    }

    /// 2026-09-08: envの型不正を登録済みとせず、既存3登録が一致していても上書きしない。
    #[test]
    fn invalid_environment_maps_block_each_client_without_overwriting_configuration() {
        for client in [
            RegistrationClient::Codex,
            RegistrationClient::ClaudeCode,
            RegistrationClient::ClaudeDesktop,
        ] {
            let fixture = Fixture::new(client);
            fixture.repair().unwrap();
            if client == RegistrationClient::Codex {
                fixture.set_codex_options("env = { EXAMPLE = 7 }");
            } else {
                let mut config = fixture.json();
                config["mcpServers"]["kb-app-read"]["env"] = json!([1]);
                fixture.write(serde_json::to_vec(&config).unwrap());
            }
            let original = fs::read(&fixture.paths.config).unwrap();
            let entries = fs::read_dir(&fixture.root).unwrap().count();
            assert!(contains(
                &fixture.status(),
                RegistrationIssueKind::InvalidConfig
            ));
            assert_eq!(fixture.status().state, RegistrationState::Blocked);
            assert!(fixture.repair().is_err());
            assert_eq!(fs::read(&fixture.paths.config).unwrap(), original);
            assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), entries);
        }
    }

    /// 2026-09-08: クライアントが読めない既知optionalを温存したまま修復成功にしない。
    #[test]
    fn invalid_codex_optional_fields_block_registration_without_touching_bytes() {
        for invalid in [
            "env = 7",
            "env = { EXAMPLE = 2025-01-02 }",
            "env_vars = 7",
            "env_vars = [7]",
            "env_vars = [{ source = 'value' }]",
            "env_vars = [{ name = 'EXAMPLE', source = 7 }]",
            "env_vars = [{ name = 'EXAMPLE', unknown = 'value' }]",
            "required = 'yes'",
            "supports_parallel_tool_calls = 'yes'",
            "startup_timeout_ms = -1",
            "startup_timeout_ms = 1.5",
            "startup_timeout_sec = 'soon'",
            "tool_timeout_sec = []",
            "cwd = 7",
            "name = 7",
            "environment_id = 7",
            "bearer_token_env_var = 7",
            "http_headers_helper = 7",
            "oauth_resource = 7",
            "env_http_headers = []",
            "http_headers = { EXAMPLE = 7 }",
            "scopes = [7]",
            "auth = 'unknown'",
            "auth = []",
            "default_tools_approval_mode = 'unknown'",
            "oauth = []",
            "oauth = { client_id = 7 }",
            "oauth = { callback_url = 7 }",
            "oauth = { callback_port = 65536 }",
            "oauth = { callback_port = -1 }",
            "oauth = { unknown = 'value' }",
            "tools = []",
            "tools = { search = 7 }",
            "tools = { search = { approval_mode = 'unknown' } }",
            "tools = { search = { output_token_limit = 0 } }",
            "tools = { search = { output_token_limit = 1.5 } }",
        ] {
            let fixture = Fixture::new(RegistrationClient::Codex);
            fixture.repair().unwrap();
            fixture.set_codex_options(invalid);
            let original = fs::read(&fixture.paths.config).unwrap();
            let entries = fs::read_dir(&fixture.root).unwrap().count();
            assert!(
                contains(&fixture.status(), RegistrationIssueKind::InvalidConfig),
                "{invalid}"
            );
            assert_eq!(
                fixture.status().state,
                RegistrationState::Blocked,
                "{invalid}"
            );
            assert!(fixture.repair().is_err(), "{invalid}");
            assert_eq!(fs::read(&fixture.paths.config).unwrap(), original);
            assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), entries);
        }
    }

    /// 2026-09-08: 公開面の除外はallow/deny toolsと同じく制約として扱い、削除して成功にしない。
    #[test]
    fn codex_tool_surface_omission_blocks_repair_including_unknown_values() {
        for omitted in [
            "['direct', 'deferred', 'code_mode']",
            "['direct']",
            "['future_surface']",
            "'direct'",
            "[7]",
        ] {
            let fixture = Fixture::new(RegistrationClient::Codex);
            fixture.repair().unwrap();
            fixture.set_codex_options(&format!("omit_tools_from = {omitted}"));
            let original = fs::read(&fixture.paths.config).unwrap();
            assert!(contains(
                &fixture.status(),
                RegistrationIssueKind::ToolPolicyRestriction
            ));
            assert_eq!(fixture.status().state, RegistrationState::Blocked);
            assert!(fixture.repair().is_err());
            assert_eq!(fs::read(&fixture.paths.config).unwrap(), original);
        }
        let fixture = Fixture::new(RegistrationClient::Codex);
        fixture.repair().unwrap();
        fixture.set_codex_options("omit_tools_from = []");
        assert_eq!(fixture.status().state, RegistrationState::Registered);
    }

    /// 2026-09-08: 正常な既知型と他clientの拡張をCodex固有schemaの導入で壊さない。
    #[test]
    fn valid_optional_types_remain_readable_and_codex_rules_do_not_apply_to_other_clients() {
        let fixture = Fixture::new(RegistrationClient::Codex);
        fixture.repair().unwrap();
        fixture.set_codex_options(
            r#"
env = { EXAMPLE = "value" }
env_vars = ["EXAMPLE", { name = "OTHER", source = "secure-source" }]
required = true
supports_parallel_tool_calls = false
startup_timeout_ms = 1000
tool_timeout_sec = 120.5
cwd = "/synthetic"
default_tools_approval_mode = "prompt"
tools = { search = { approval_mode = "auto", output_token_limit = 1000 } }
custom_date = 2025-01-02
"#,
        );
        let original = fs::read(&fixture.paths.config).unwrap();
        assert_eq!(fixture.status().state, RegistrationState::Registered);
        assert!(!fixture.repair().unwrap().changed);
        assert_eq!(fs::read(&fixture.paths.config).unwrap(), original);
        assert!(codex_optional_fields_valid(&json!({
            "auth": "oauth", "oauth": {"client_id": "synthetic", "callback_port": 0, "callback_url": "https://example.invalid/callback"},
            "http_headers": {"Example": "value"}, "env_http_headers": {"Example": "EXAMPLE"},
            "http_headers_helper": "/synthetic/helper", "bearer_token_env_var": "EXAMPLE",
            "environment_id": "synthetic-environment", "name": "synthetic-name",
            "oauth_resource": "https://example.invalid", "scopes": ["read"],
            "startup_timeout_sec": 1.5
        })));
        for client in [
            RegistrationClient::ClaudeCode,
            RegistrationClient::ClaudeDesktop,
        ] {
            let fixture = Fixture::new(client);
            fixture.repair().unwrap();
            let mut config = fixture.json();
            config["mcpServers"]["kb-app-read"]["required"] = json!("extension-value");
            config["mcpServers"]["kb-app-read"]["omit_tools_from"] = json!({"extension": true});
            fixture.write(serde_json::to_vec(&config).unwrap());
            assert_eq!(fixture.status().state, RegistrationState::Registered);
            assert!(!fixture.repair().unwrap().changed);
            assert_eq!(fixture.json(), config);
        }
    }

    /// 2026-09-08: FIFOはopenするだけで待機するので、通常ファイル以外は先に拒否する。
    #[cfg(unix)]
    #[test]
    fn non_regular_configuration_is_rejected_before_blocking_open() {
        let fixture = Fixture::new(RegistrationClient::ClaudeCode);
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fixture.paths.config)
                .status()
                .unwrap()
                .success()
        );
        let path = fixture.paths.config.clone();
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || send.send(read_config(&path)).unwrap());
        assert_eq!(
            receive
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("FIFOの読取り待ちにならず拒否する"),
            Err(RegistrationIssueKind::UnsafePath)
        );
        assert_eq!(
            read_config(&fixture.root),
            Err(RegistrationIssueKind::UnsafePath)
        );
        assert_eq!(
            read_config(Path::new("/dev/null")),
            Err(RegistrationIssueKind::UnsafePath)
        );
        assert!(fixture.repair().is_err());
        assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 1);
    }

    #[test]
    fn scoped_overrides_and_codex_tool_restrictions_are_detected_without_repair() {
        for (client, extra, kind) in [
            (
                RegistrationClient::ClaudeCode,
                "{\"projects\":{\"/synthetic\":{\"mcpServers\":{\"kb-app-read\":{\"command\":\"different\"}}}}}",
                RegistrationIssueKind::ScopedOverride,
            ),
            (
                RegistrationClient::Codex,
                "[profiles.work.mcp_servers.kb-app-read]\ncommand = \"different\"",
                RegistrationIssueKind::ScopedOverride,
            ),
            (
                RegistrationClient::Codex,
                "[mcp_servers.kb-app-read]\nenabled_tools = [\"kb_search\"]",
                RegistrationIssueKind::ToolPolicyRestriction,
            ),
            (
                RegistrationClient::Codex,
                "[mcp_servers.kb-app-read]\ndisabled_tools = [\"kb_search\"]",
                RegistrationIssueKind::ToolPolicyRestriction,
            ),
        ] {
            let fixture = Fixture::new(client);
            fixture.write(extra);
            let status = fixture.status();
            assert_eq!(status.state, RegistrationState::Blocked);
            assert!(contains(&status, kind));
            assert!(fixture.repair().is_err());
            assert_eq!(fs::read(&fixture.paths.config).unwrap(), extra.as_bytes());
        }
    }

    #[test]
    fn codex_allowlist_requires_each_name_and_matching_identity_and_rejects_unknown_matchers() {
        let mut fixture = Fixture::new(RegistrationClient::Codex);
        let requirements = fixture.root.join("requirements.toml");
        fixture.paths.codex_requirements = Some(requirements.clone());
        fs::write(&requirements, "[mcp_servers]\n").unwrap();
        assert!(contains(
            &fixture.status(),
            RegistrationIssueKind::ManagedPolicyConflict
        ));
        assert!(fixture.repair().is_err());
        assert!(!fixture.paths.config.exists());
        let mut doc = DocumentMut::new();
        for (name, surface) in SERVERS {
            doc["mcp_servers"][name]["identity"]["command"] =
                toml_edit::value(fixture.exe.to_str().unwrap());
            let wanted = expected(fixture.client, &fixture.exe, "Synthetic Vault", surface);
            assert_eq!(
                codex_identity_matches(&wanted["command"], &wanted),
                Ok(true)
            );
            let rule = json!({"executable": fixture.exe, "args": wanted["args"].as_array().unwrap().iter().map(|arg| json!({"match":"exact", "value":arg})).collect::<Vec<_>>()});
            assert_eq!(codex_identity_matches(&rule, &wanted), Ok(true));
            let mut unknown = rule.clone();
            unknown["args"][0] = json!({"match":"regex", "expression":".*"});
            assert_eq!(
                codex_identity_matches(&unknown, &wanted),
                Err(RegistrationIssueKind::ManagedPolicyUnverified)
            );
            let mut wrong = rule;
            wrong["args"][4]["value"] = json!("Other Vault");
            assert_eq!(codex_identity_matches(&wrong, &wanted), Ok(false));
        }
        fs::write(&requirements, doc.to_string()).unwrap();
        assert!(fixture.status().can_repair);
        assert_eq!(
            fixture.repair().unwrap().status.state,
            RegistrationState::Registered
        );
    }

    #[test]
    fn claude_managed_file_is_exclusive_and_merged_lists_keep_the_documented_semantics() {
        let mut fixture = Fixture::new(RegistrationClient::ClaudeCode);
        let managed_mcp = fixture.root.join("managed-mcp.json");
        fixture.paths.claude_managed_mcp = Some(managed_mcp.clone());
        fs::write(&managed_mcp, "{\"mcpServers\":{}}").unwrap();
        assert!(contains(
            &fixture.status(),
            RegistrationIssueKind::ManagedPolicyConflict
        ));
        assert!(fixture.repair().is_err());
        assert_eq!(
            fs::read_to_string(&managed_mcp).unwrap(),
            "{\"mcpServers\":{}}"
        );
        fixture.paths.claude_managed_mcp = None;
        let managed = fixture.root.join("managed-settings.json");
        let user = fixture.root.join("user-settings.json");
        fixture.paths.claude_settings = vec![(managed.clone(), true), (user.clone(), false)];
        fs::write(
            &managed,
            r#"{"allowedMcpServers":[{"serverName":"kb-app-read"}]}"#,
        )
        .unwrap();
        fs::write(&user, r#"{"allowedMcpServers":[{"serverName":"kb-app-write"},{"serverName":"kb-app-maintenance"}]}"#).unwrap();
        assert!(
            fixture.status().can_repair,
            "soft allowlists merge by union"
        );
        fs::write(&managed, r#"{"allowManagedMcpServersOnly":true,"allowedMcpServers":[{"serverName":"kb-app-read"}]}"#).unwrap();
        assert!(contains(
            &fixture.status(),
            RegistrationIssueKind::ManagedPolicyConflict
        ));
        fs::write(&managed, r#"{"allowedMcpServers":[]}"#).unwrap();
        fs::write(&user, "{}").unwrap();
        assert!(contains(
            &fixture.status(),
            RegistrationIssueKind::ManagedPolicyConflict
        ));
        fs::write(&managed, "{}").unwrap();
        assert!(
            fixture.status().can_repair,
            "absent allowlist is unrestricted"
        );
        fs::write(
            &user,
            r#"{"deniedMcpServers":[{"serverName":"kb-app-read"}]}"#,
        )
        .unwrap();
        assert!(contains(
            &fixture.status(),
            RegistrationIssueKind::ManagedPolicyConflict
        ));

        let wanted = expected(fixture.client, &fixture.exe, "Synthetic Vault", "read");
        let mut command = vec![wanted["command"].clone()];
        command.extend(wanted["args"].as_array().unwrap().iter().cloned());
        assert_eq!(
            claude_policy_allows(
                &json!({"allowedMcpServers":[{"serverName":"kb-app-read"},{"serverCommand":["different"]}]}),
                "kb-app-read",
                &wanted
            ),
            Err(RegistrationIssueKind::ManagedPolicyConflict)
        );
        assert_eq!(
            claude_policy_allows(
                &json!({"allowedMcpServers":[{"serverCommand":command}]}),
                "kb-app-read",
                &wanted
            ),
            Ok(())
        );
        assert_eq!(
            claude_policy_allows(
                &json!({"allowedMcpServers":[{"serverCommand":["${EXAMPLE}/kb-app"]}]}),
                "kb-app-read",
                &wanted
            ),
            Err(RegistrationIssueKind::ManagedPolicyUnverified)
        );
    }

    #[test]
    fn replacement_stops_on_concurrent_edit_or_unreadable_original_including_initially_missing() {
        let fixture = Fixture::new(RegistrationClient::ClaudeDesktop);
        fixture.write("{}");
        let concurrent = b"{partially written";
        let result = replace_config(
            &fixture.paths.config,
            Some(b"{}"),
            b"{\"replacement\":true}",
            || {
                fs::write(&fixture.paths.config, concurrent)?;
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(fs::read(&fixture.paths.config).unwrap(), concurrent);
        assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 1);
        let directory = fixture.root.join("appeared-as-directory.json");
        let result = replace_config(&directory, None, b"{}", || {
            fs::create_dir(&directory)?;
            Ok(())
        });
        assert!(
            result.is_err(),
            "an unreadable path must not compare equal to missing"
        );
        assert!(directory.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_registration_and_invalid_target_are_blocked_without_touching_target() {
        let fixture = Fixture::new(RegistrationClient::ClaudeDesktop);
        let target = fixture.root.join("original.json");
        fs::write(&target, "{}").unwrap();
        std::os::unix::fs::symlink(&target, &fixture.paths.config).unwrap();
        assert!(contains(
            &fixture.status(),
            RegistrationIssueKind::UnsafePath
        ));
        assert!(fixture.repair().is_err());
        assert_eq!(fs::read(&target).unwrap(), b"{}");
        assert!(!valid_target(Path::new("relative/kb-app"), "Vault"));
        assert!(!valid_target(&fixture.exe, "${VAULT}"));
        assert!(!safe_path(&fixture.root.join("../other")));
    }
}
