//! 蒸留AIは固定された入力からJSONを返すだけにし、KBへの反映はコアが検証する。
//! CLIの認証・管理ポリシーを保ちつつ、通常の作業ディレクトリとツール設定を渡さない。

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;
use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{CoreError, Result};

mod catalog;

// 1回の変更案は小さいwaveに限定する。CLIの診断も含めて上限を共有し、暴走を止める。
const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_PROMPT_BYTES: usize = 2 * 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum DistillationAiProvider {
    ClaudeCode,
    Codex,
}

impl DistillationAiProvider {
    fn executable_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(default, deny_unknown_fields)]
pub struct DistillationAiSettings {
    pub enabled: bool,
    pub provider: Option<DistillationAiProvider>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub timeout_seconds: u32,
    pub periodic_hours: u32,
}

impl Default for DistillationAiSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: None,
            model: None,
            reasoning_effort: None,
            timeout_seconds: 300,
            // 全体の再確認は週次。作成直後と失敗の再試行は別のキューで常時行う。
            periodic_hours: 168,
        }
    }
}

impl DistillationAiSettings {
    pub fn validate(&self) -> Result<()> {
        if self.enabled && self.provider.is_none() {
            return Err(CoreError::invalid_input(anyhow::anyhow!(
                "蒸留AIを有効にするには実行AIの選択が必要"
            )));
        }
        if !(30..=1800).contains(&self.timeout_seconds) || !(1..=720).contains(&self.periodic_hours)
        {
            return Err(CoreError::invalid_input(anyhow::anyhow!(
                "蒸留AIの実行時間または再確認間隔が範囲外"
            )));
        }
        if let Some(model) = &self.model
            && (model.is_empty()
                || model.len() > 200
                || model.starts_with('-')
                || !model
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-._:/[]".contains(c)))
        {
            return Err(CoreError::invalid_input(anyhow::anyhow!(
                "蒸留モデルの識別子が不正"
            )));
        }
        if let Some(effort) = &self.reasoning_effort
            && (self.provider.is_none()
                || self.model.is_none()
                || effort.is_empty()
                || effort.len() > 32
                || !effort.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
        {
            return Err(CoreError::invalid_input(anyhow::anyhow!(
                "推論強度はモデルを指定して選択する"
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct DistillationModel {
    pub model: String,
    pub display_name: String,
    pub supported_reasoning_efforts: Vec<String>,
    pub default_reasoning_effort: Option<String>,
    pub is_default: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct DistillationModelCatalog {
    pub provider: DistillationAiProvider,
    pub models: Vec<DistillationModel>,
    pub unavailable_reason: Option<AiRunError>,
}

/// 一覧取得は推論やKB参照を開始しない。取得不能を固定候補で埋めない。
pub fn models(provider: DistillationAiProvider) -> DistillationModelCatalog {
    let result = executable(provider)
        .ok_or(AiRunError::NotInstalled)
        .and_then(|program| catalog::read(provider, &program, false, || false));
    match result {
        Ok(models) => DistillationModelCatalog {
            provider,
            models,
            unavailable_reason: None,
        },
        Err(error) => DistillationModelCatalog {
            provider,
            models: Vec::new(),
            unavailable_reason: Some(error),
        },
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct DistillationAiProviderStatus {
    pub provider: DistillationAiProvider,
    pub installed: bool,
    pub unavailable_reason: Option<AiRunError>,
}

pub fn providers() -> Vec<DistillationAiProviderStatus> {
    let guard = crate::ai_guard::status().ok();
    [
        DistillationAiProvider::ClaudeCode,
        DistillationAiProvider::Codex,
    ]
    .into_iter()
    .map(|provider| {
        let installed = executable(provider).is_some();
        DistillationAiProviderStatus {
            provider,
            installed,
            unavailable_reason: if !installed {
                Some(AiRunError::NotInstalled)
            } else {
                provider_policy_with_guard(provider, guard.as_ref()).err()
            },
        }
    })
    .collect()
}

pub fn kb_enabled(settings: &DistillationAiSettings, kb: &crate::settings::Settings) -> bool {
    kb.ai_kb_enabled
        && match settings.provider {
            Some(DistillationAiProvider::ClaudeCode) => kb.claude_kb_enabled,
            Some(DistillationAiProvider::Codex) => kb.gpt_kb_enabled,
            None => false,
        }
}

pub fn load() -> Result<DistillationAiSettings> {
    load_at(&settings_path()?)
}

pub fn save(settings: DistillationAiSettings) -> Result<DistillationAiSettings> {
    save_at(&settings_path()?, settings)
}

fn settings_path() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("設定ディレクトリが特定できない")
        .map_err(CoreError::configuration)?
        .join("kb-app/distillation-ai.json"))
}

fn load_at(path: &Path) -> Result<DistillationAiSettings> {
    let settings: DistillationAiSettings = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(CoreError::configuration)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            DistillationAiSettings::default()
        }
        Err(error) => return Err(CoreError::configuration(error)),
    };
    settings.validate()?;
    Ok(settings)
}

fn save_at(path: &Path, mut settings: DistillationAiSettings) -> Result<DistillationAiSettings> {
    settings.model = settings
        .model
        .map(|model| model.trim().to_owned())
        .filter(|model| !model.is_empty());
    settings.validate()?;
    let parent = path
        .parent()
        .context("蒸留AI設定に親ディレクトリがない")
        .map_err(CoreError::configuration)?;
    fs::create_dir_all(parent).map_err(CoreError::configuration)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.with_extension("lock"))
        .map_err(CoreError::configuration)?;
    lock.lock_exclusive().map_err(CoreError::configuration)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(CoreError::configuration)?;
    serde_json::to_writer_pretty(&mut temp, &settings).map_err(CoreError::configuration)?;
    temp.write_all(b"\n").map_err(CoreError::configuration)?;
    temp.as_file()
        .sync_all()
        .map_err(CoreError::configuration)?;
    temp.persist(path)
        .map_err(|error| CoreError::configuration(error.error))?;
    #[cfg(unix)]
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(CoreError::configuration)?;
    Ok(settings)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum AiRunError {
    Unconfigured,
    KbDisabled,
    InvalidSettings,
    NotInstalled,
    UnsupportedCli,
    SystemPolicyConflict,
    GuardOutdated,
    GuardUnavailable,
    StartFailed,
    TimedOut,
    Cancelled,
    OutputLimit,
    ProcessFailed,
    InvalidResponse,
    Io,
}

impl AiRunError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unconfigured => "unconfigured",
            Self::KbDisabled => "kb_disabled",
            Self::InvalidSettings => "invalid_settings",
            Self::NotInstalled => "not_installed",
            Self::UnsupportedCli => "unsupported_cli",
            Self::SystemPolicyConflict => "system_policy_conflict",
            Self::GuardOutdated => "guard_outdated",
            Self::GuardUnavailable => "guard_unavailable",
            Self::StartFailed => "start_failed",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
            Self::OutputLimit => "output_limit",
            Self::ProcessFailed => "process_failed",
            Self::InvalidResponse => "invalid_response",
            Self::Io => "io",
        }
    }
}

impl std::fmt::Display for AiRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for AiRunError {}

/// KB本文はstdinでのみ渡す。呼び出し元は返されたJSONを未信頼入力として検証する。
pub fn run(
    settings: &DistillationAiSettings,
    prompt: &str,
    schema: &Value,
    cancelled: impl Fn() -> bool,
) -> std::result::Result<Value, AiRunError> {
    run_observed(settings, prompt, schema, cancelled, None, 0)
}

pub(crate) fn run_with_metrics(
    settings: &DistillationAiSettings,
    prompt: &str,
    schema: &Value,
    cancelled: impl Fn() -> bool,
    recorder: &crate::distillation_metrics::Recorder,
    round: u32,
) -> std::result::Result<Value, AiRunError> {
    run_observed(settings, prompt, schema, cancelled, Some(recorder), round)
}

fn run_observed(
    settings: &DistillationAiSettings,
    prompt: &str,
    schema: &Value,
    cancelled: impl Fn() -> bool,
    recorder: Option<&crate::distillation_metrics::Recorder>,
    round: u32,
) -> std::result::Result<Value, AiRunError> {
    use crate::distillation_metrics::Stage;
    let (provider, program) = measured(recorder, Stage::CliCheck, round, || {
        settings
            .validate()
            .map_err(|_| AiRunError::InvalidSettings)?;
        if !settings.enabled {
            return Err(AiRunError::Unconfigured);
        }
        let provider = settings.provider.ok_or(AiRunError::Unconfigured)?;
        let kb = crate::settings::load().map_err(|_| AiRunError::KbDisabled)?;
        if !kb_enabled(settings, &kb) {
            return Err(AiRunError::KbDisabled);
        }
        provider_policy_with_guard(provider, crate::ai_guard::status().ok().as_ref())?;
        let program = executable(provider).ok_or(AiRunError::NotInstalled)?;
        verify_cli(provider, &program, &cancelled)?;
        Ok((provider, program))
    })?;
    if settings.reasoning_effort.is_some() {
        measured(recorder, Stage::ModelCatalog, round, || {
            let available = catalog::read(provider, &program, true, &cancelled)?;
            validate_selection(settings, &available)
        })?;
    }
    // CLI起動・応答待ち・JSON解析を含む呼び出し全体。サーバー内推論時間ではない。
    measured(recorder, Stage::AiResponse, round, || {
        run_at(settings, &program, prompt, schema, cancelled)
    })
}

fn measured<T>(
    recorder: Option<&crate::distillation_metrics::Recorder>,
    stage: crate::distillation_metrics::Stage,
    round: u32,
    operation: impl FnOnce() -> std::result::Result<T, AiRunError>,
) -> std::result::Result<T, AiRunError> {
    match recorder {
        Some(recorder) => recorder.measure(stage, round, operation),
        None => operation(),
    }
}

fn validate_selection(
    settings: &DistillationAiSettings,
    available: &[DistillationModel],
) -> std::result::Result<(), AiRunError> {
    if let Some(effort) = &settings.reasoning_effort {
        let selected = available
            .iter()
            .find(|entry| Some(entry.model.as_str()) == settings.model.as_deref())
            .ok_or(AiRunError::InvalidSettings)?;
        if !selected.supported_reasoning_efforts.contains(effort) {
            return Err(AiRunError::InvalidSettings);
        }
    }
    Ok(())
}

fn verify_cli(
    provider: DistillationAiProvider,
    program: &Path,
    cancelled: impl Fn() -> bool,
) -> std::result::Result<(), AiRunError> {
    let working = tempfile::tempdir().map_err(|_| AiRunError::Io)?;
    let mut command = Command::new(program);
    command.current_dir(working.path());
    command.arg(match provider {
        DistillationAiProvider::ClaudeCode => "--help",
        DistillationAiProvider::Codex => "--version",
    });
    let output = execute(
        &mut command,
        Vec::new(),
        Duration::from_secs(10),
        128 * 1024,
        cancelled,
    )?;
    let text = std::str::from_utf8(&output).map_err(|_| AiRunError::UnsupportedCli)?;
    let supported = match provider {
        DistillationAiProvider::ClaudeCode => [
            "--tools",
            "--safe-mode",
            "--restricted",
            "--strict-mcp-config",
            "--setting-sources",
            "--json-schema",
            "--effort",
        ]
        .iter()
        .all(|flag| text.contains(flag)),
        // 古いCLIが未知のpermission設定を無視して実行するのを防ぐ。
        // この境界と--ignore-user-configは0.144.3のローカルCLIで照合済み。
        DistillationAiProvider::Codex => codex_version_supported(text),
    };
    if supported {
        Ok(())
    } else {
        Err(AiRunError::UnsupportedCli)
    }
}

fn codex_version_supported(text: &str) -> bool {
    let Some(version) = text.trim().strip_prefix("codex-cli ") else {
        return false;
    };
    let (core, prerelease) = match version.split_once('-') {
        Some((core, suffix)) => {
            let mut components = suffix.split('.');
            if !matches!(components.next(), Some("alpha" | "beta" | "rc"))
                || !components.all(version_number)
            {
                return false;
            }
            (core, true)
        }
        None => (version, false),
    };
    if !core.split('.').all(version_number) {
        return false;
    }
    let parts = core
        .split('.')
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>();
    // 公式アプリの0.147.0-alpha.6.6を受け入れつつ、境界版のpre-releaseは
    // 検証済みstableより前として拒否する。未知のmajor系列は引き続き受け入れない。
    matches!(parts.as_deref(), Ok([0, minor, patch]) if (*minor, *patch) > (144, 3) || ((*minor, *patch) == (144, 3) && !prerelease))
}

fn version_number(component: &str) -> bool {
    !component.is_empty()
        && component
            .chars()
            .all(|character| character.is_ascii_digit())
        && (component == "0" || !component.starts_with('0'))
}

fn run_at(
    settings: &DistillationAiSettings,
    program: &Path,
    prompt: &str,
    schema: &Value,
    cancelled: impl Fn() -> bool,
) -> std::result::Result<Value, AiRunError> {
    if prompt.len() > MAX_PROMPT_BYTES || !schema.is_object() {
        return Err(AiRunError::InvalidSettings);
    }
    let provider = settings.provider.ok_or(AiRunError::Unconfigured)?;
    let working = tempfile::Builder::new()
        .prefix("kb-distillation-")
        .tempdir()
        .map_err(|_| AiRunError::Io)?;
    let mut command = command_for(provider, program, settings, schema, working.path())?;
    let output = execute(
        &mut command,
        prompt.as_bytes().to_vec(),
        Duration::from_secs(u64::from(settings.timeout_seconds)),
        MAX_OUTPUT_BYTES,
        cancelled,
    )?;
    parse_response(provider, &output)
}

fn command_for(
    provider: DistillationAiProvider,
    program: &Path,
    settings: &DistillationAiSettings,
    schema: &Value,
    working: &Path,
) -> std::result::Result<Command, AiRunError> {
    let mut command = Command::new(program);
    command.current_dir(working);
    match provider {
        DistillationAiProvider::ClaudeCode => {
            command.args([
                "--print",
                "--output-format",
                "json",
                "--tools",
                "",
                "--safe-mode",
                "--restricted",
                "--strict-mcp-config",
                "--mcp-config",
                "{\"mcpServers\":{}}",
                "--disable-slash-commands",
                "--setting-sources",
                "",
                "--no-session-persistence",
                "--no-chrome",
                "--json-schema",
            ]);
            command.arg(schema.to_string());
            if let Some(model) = &settings.model {
                command.arg("--model").arg(model);
            }
            if let Some(effort) = &settings.reasoning_effort {
                command.arg("--effort").arg(effort);
            }
        }
        DistillationAiProvider::Codex => {
            let schema_path = working.join("response-schema.json");
            fs::write(&schema_path, schema.to_string()).map_err(|_| AiRunError::Io)?;
            command.args([
                "exec",
                "--ignore-user-config",
                "--strict-config",
                "--ephemeral",
                "--skip-git-repo-check",
                "--json",
                "--color",
                "never",
                "--output-schema",
            ]);
            command.arg(schema_path);
            // --sandboxはdefault_permissionsに優先するため併用しない。
            // https://learn.chatgpt.com/docs/permissions のworkspace限定profileに従う。
            // profile本体は管理requirementsが所有する。ローカルに同名定義を作らない。
            command.arg("--config").arg(format!(
                "default_permissions=\"{}\"",
                crate::ai_guard::CODEX_DISTILLATION_PROFILE
            ));
            for override_value in [
                "approval_policy=\"never\"",
                "web_search=\"disabled\"",
                "project_doc_max_bytes=0",
                "mcp_servers={}",
                "features.shell_tool=false",
                "features.unified_exec=false",
                "features.code_mode=false",
                "features.code_mode_host=false",
                "features.apps=false",
                "features.plugins=false",
                "features.multi_agent=false",
                "features.computer_use=false",
                "features.browser_use=false",
                "features.image_generation=false",
                "features.memories=false",
                "features.workspace_dependencies=false",
            ] {
                command.arg("--config").arg(override_value);
            }
            if let Some(model) = &settings.model {
                command.arg("--model").arg(model);
            }
            if let Some(effort) = &settings.reasoning_effort {
                command
                    .arg("--config")
                    .arg(format!("model_reasoning_effort=\"{effort}\""));
                if effort == "ultra" {
                    // Ultraは並列subagentを使う。本文と同じ制限を継承したうえで
                    // 同時数を制限し、通常の蒸留で不要な並列実行は有効にしない。
                    command.args([
                        "--config",
                        "features.multi_agent=true",
                        "--config",
                        "agents.max_threads=4",
                    ]);
                }
            }
            command.arg("-");
        }
    }
    Ok(command)
}

fn provider_policy_with_guard(
    provider: DistillationAiProvider,
    guard: Option<&crate::ai_guard::AiGuardStatus>,
) -> std::result::Result<(), AiRunError> {
    use crate::ai_guard::GuardTargetState;
    let state = guard.map(|guard| match provider {
        DistillationAiProvider::ClaudeCode => guard.claude,
        DistillationAiProvider::Codex => guard.codex,
    });
    match state {
        Some(GuardTargetState::Enforced) => {}
        Some(GuardTargetState::Outdated) => return Err(AiRunError::GuardOutdated),
        _ => return Err(AiRunError::GuardUnavailable),
    }
    if provider == DistillationAiProvider::Codex {
        // system設定のsandbox_modeやMCPは--ignore-user-configでは消えない。
        // 設定を読んだり変更したりせず、確認できない併用は実行前に止める。
        // requirements.tomlは通常どおりCLIが適用する。管理ポリシーを外さない。
        match fs::symlink_metadata("/etc/codex/config.toml") {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(AiRunError::SystemPolicyConflict),
        }
    }
    Ok(())
}

fn executable(provider: DistillationAiProvider) -> Option<PathBuf> {
    let directories: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default();
    let home_dir = dirs::home_dir();
    let bundled_codex = if cfg!(target_os = "macos") {
        let mut candidates = vec![PathBuf::from(
            "/Applications/Codex.app/Contents/Resources/codex",
        )];
        if let Some(home_dir) = &home_dir {
            candidates.push(home_dir.join("Applications/Codex.app/Contents/Resources/codex"));
        }
        candidates
    } else {
        Vec::new()
    };
    executable_candidates(provider, directories, home_dir, bundled_codex)
        .into_iter()
        .find(|candidate| executable_file(candidate))
}

fn executable_candidates(
    provider: DistillationAiProvider,
    mut directories: Vec<PathBuf>,
    home_dir: Option<PathBuf>,
    bundled_codex: Vec<PathBuf>,
) -> Vec<PathBuf> {
    let name = provider.executable_name();
    // 2026-09-07: PATH上の旧Homebrew CLIだけを見て、公式アプリ同梱CLIを
    // 探索していなかった。候補取得と推論の両方を公式アプリの更新に追従させる。
    let mut candidates = match provider {
        DistillationAiProvider::Codex => bundled_codex,
        DistillationAiProvider::ClaudeCode => Vec::new(),
    };
    // Finderから起動されたアプリのPATHには、標準的なCLIの導入先が含まれない。
    if let Some(home_dir) = home_dir {
        directories.push(home_dir.join(".local/bin"));
    }
    directories.extend([
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ]);
    candidates.extend(
        directories
            .into_iter()
            .filter(|directory| directory.is_absolute())
            .map(|directory| directory.join(name)),
    );
    candidates
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

enum IoEvent {
    Stdout(std::io::Result<Vec<u8>>),
    Stderr(std::io::Result<Vec<u8>>),
    Stdin(std::io::Result<()>),
}

fn drain(mut stream: impl Read, total: &AtomicUsize, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut bytes = [0; 8192];
    loop {
        let count = stream.read(&mut bytes)?;
        if count == 0 {
            return Ok(output);
        }
        let previous = total.fetch_add(count, Ordering::Relaxed);
        let retained = count.min(limit.saturating_sub(previous));
        output.extend_from_slice(&bytes[..retained]);
    }
}

fn execute(
    command: &mut Command,
    input: Vec<u8>,
    timeout: Duration,
    output_limit: usize,
    cancelled: impl Fn() -> bool,
) -> std::result::Result<Vec<u8>, AiRunError> {
    if cancelled() {
        return Err(AiRunError::Cancelled);
    }
    let child = spawn_process(command)?;
    monitor_process(child, input, timeout, output_limit, cancelled)
}

fn spawn_process(command: &mut Command) -> std::result::Result<Child, AiRunError> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn().map_err(|_| AiRunError::StartFailed)
}

fn monitor_process(
    mut child: Child,
    input: Vec<u8>,
    timeout: Duration,
    output_limit: usize,
    cancelled: impl Fn() -> bool,
) -> std::result::Result<Vec<u8>, AiRunError> {
    let stdout = child.stdout.take().ok_or(AiRunError::Io)?;
    let stderr = child.stderr.take().ok_or(AiRunError::Io)?;
    let mut stdin = child.stdin.take().ok_or(AiRunError::Io)?;
    let total = Arc::new(AtomicUsize::new(0));
    let (sender, receiver) = mpsc::channel();
    let stdout_sender = sender.clone();
    let stdout_total = total.clone();
    thread::spawn(move || {
        let _ = stdout_sender.send(IoEvent::Stdout(drain(stdout, &stdout_total, output_limit)));
    });
    let stderr_sender = sender.clone();
    let stderr_total = total.clone();
    thread::spawn(move || {
        let _ = stderr_sender.send(IoEvent::Stderr(drain(stderr, &stderr_total, output_limit)));
    });
    thread::spawn(move || {
        let result = stdin.write_all(&input);
        drop(stdin);
        let _ = sender.send(IoEvent::Stdin(result));
    });

    let started = Instant::now();
    let mut output = None;
    let mut received = 0;
    let mut exit = None;
    let mut input_failed = false;
    let outcome = loop {
        if cancelled() {
            break Err(AiRunError::Cancelled);
        }
        if total.load(Ordering::Relaxed) > output_limit {
            break Err(AiRunError::OutputLimit);
        }
        if started.elapsed() >= timeout {
            break Err(AiRunError::TimedOut);
        }
        match child.try_wait() {
            Ok(Some(status)) => exit = Some(status),
            Ok(None) => {}
            Err(_) => break Err(AiRunError::Io),
        }
        match receiver.recv_timeout(POLL_INTERVAL) {
            Ok(event) => {
                received += 1;
                match event {
                    IoEvent::Stdout(Ok(bytes)) => output = Some(bytes),
                    IoEvent::Stderr(Ok(_)) | IoEvent::Stdin(Ok(())) => {}
                    IoEvent::Stdout(Err(_)) | IoEvent::Stderr(Err(_)) => break Err(AiRunError::Io),
                    // 認証失敗などで先に終了したプロセスはstdinを閉じる。終了分類を優先する。
                    IoEvent::Stdin(Err(_)) => input_failed = true,
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) if exit.is_none() => {
                thread::sleep(POLL_INTERVAL);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }
        if let Some(status) = exit
            && received == 3
        {
            if total.load(Ordering::Relaxed) > output_limit {
                break Err(AiRunError::OutputLimit);
            }
            if !status.success() {
                break Err(AiRunError::ProcessFailed);
            }
            if input_failed {
                break Err(AiRunError::Io);
            }
            break output.ok_or(AiRunError::InvalidResponse);
        }
    };
    if outcome.is_err() {
        stop_process(&mut child);
        // CLIの子がpipeを継承していても、呼び出し元を無期限に待たせない。
        let until = Instant::now() + Duration::from_secs(1);
        while received < 3 && Instant::now() < until {
            match receiver.recv_timeout(POLL_INTERVAL) {
                Ok(_) => received += 1,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
    outcome
}

fn stop_process(child: &mut Child) {
    #[cfg(unix)]
    {
        // 自分が作ったgroupだけを止める。CLIが起動した子にpipeを保持させない。
        let _ = Command::new("/bin/kill")
            .args(["-s", "KILL", "--", &format!("-{}", child.id())])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn parse_response(
    provider: DistillationAiProvider,
    bytes: &[u8],
) -> std::result::Result<Value, AiRunError> {
    match provider {
        DistillationAiProvider::ClaudeCode => {
            let result: Value =
                serde_json::from_slice(bytes).map_err(|_| AiRunError::InvalidResponse)?;
            if result.get("is_error").and_then(Value::as_bool) == Some(true)
                || result.get("subtype").and_then(Value::as_str) != Some("success")
            {
                return Err(AiRunError::ProcessFailed);
            }
            result
                .get("structured_output")
                .filter(|value| value.is_object())
                .cloned()
                .ok_or(AiRunError::InvalidResponse)
        }
        DistillationAiProvider::Codex => {
            let text = std::str::from_utf8(bytes).map_err(|_| AiRunError::InvalidResponse)?;
            let mut result = None;
            let mut completed = false;
            for line in text.lines().filter(|line| !line.trim().is_empty()) {
                let event: Value =
                    serde_json::from_str(line).map_err(|_| AiRunError::InvalidResponse)?;
                match event.get("type").and_then(Value::as_str) {
                    Some("error" | "turn.failed") => return Err(AiRunError::ProcessFailed),
                    Some("turn.completed") => completed = true,
                    Some("item.completed") => {
                        let item = &event["item"];
                        if item["type"] == "agent_message" {
                            result = item["text"]
                                .as_str()
                                .and_then(|text| serde_json::from_str(text).ok());
                        }
                    }
                    _ => {}
                }
            }
            result
                .filter(|value: &Value| completed && value.is_object())
                .ok_or(AiRunError::InvalidResponse)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_to_unconfigured_and_round_trip_atomically() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings/distillation-ai.json");
        assert_eq!(load_at(&path).unwrap(), DistillationAiSettings::default());
        let saved = save_at(
            &path,
            DistillationAiSettings {
                enabled: true,
                provider: Some(DistillationAiProvider::ClaudeCode),
                model: Some(" test-model ".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(saved.model.as_deref(), Some("test-model"));
        assert_eq!(load_at(&path).unwrap(), saved);
        assert!(temp.path().join("settings/distillation-ai.lock").exists());
    }

    #[test]
    fn settings_reject_options_and_unbounded_work() {
        for model in ["--danger", "a b", "a\nb", "$(pwd)"] {
            let settings = DistillationAiSettings {
                model: Some(model.into()),
                ..Default::default()
            };
            assert!(settings.validate().is_err());
        }
        assert!(
            DistillationAiSettings {
                enabled: true,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            DistillationAiSettings {
                timeout_seconds: 0,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            DistillationAiSettings {
                periodic_hours: 0,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }

    /// 2026-09-06: モデル欄だけではAstra Ultraを指定できなかったため、
    /// モデル識別子と推論強度を別の永続値として保存する。
    #[test]
    fn reasoning_effort_is_separate_and_old_settings_remain_readable() {
        let old: DistillationAiSettings = serde_json::from_str(
            r#"{"enabled":true,"provider":"codex","model":"gpt-6-astra","timeout_seconds":300,"periodic_hours":168}"#,
        ).unwrap();
        assert_eq!(old.reasoning_effort, None);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        let selected = DistillationAiSettings {
            reasoning_effort: Some("ultra".into()),
            ..old
        };
        save_at(&path, selected.clone()).unwrap();
        assert_eq!(load_at(&path).unwrap(), selected);
        assert!(
            DistillationAiSettings {
                model: None,
                ..selected.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            DistillationAiSettings {
                reasoning_effort: Some("ultra\"\n".into()),
                ..selected
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn unsupported_effort_or_unknown_model_never_falls_back() {
        let selected = DistillationAiSettings {
            provider: Some(DistillationAiProvider::Codex),
            model: Some("gpt-6-astra".into()),
            reasoning_effort: Some("ultra".into()),
            ..Default::default()
        };
        let mut entry = DistillationModel {
            model: "gpt-6-astra".into(),
            display_name: "Astra".into(),
            supported_reasoning_efforts: vec!["high".into()],
            default_reasoning_effort: Some("high".into()),
            is_default: true,
        };
        assert_eq!(
            validate_selection(&selected, std::slice::from_ref(&entry)),
            Err(AiRunError::InvalidSettings)
        );
        entry.supported_reasoning_efforts.push("ultra".into());
        assert_eq!(
            validate_selection(&selected, std::slice::from_ref(&entry)),
            Ok(())
        );
        entry.model = "another-model".into();
        assert_eq!(
            validate_selection(&selected, &[entry]),
            Err(AiRunError::InvalidSettings)
        );
    }

    #[test]
    fn codex_astra_ultra_sets_both_cli_fields_and_bounded_parallelism() {
        let temp = tempfile::tempdir().unwrap();
        let selected = DistillationAiSettings {
            provider: Some(DistillationAiProvider::Codex),
            model: Some("gpt-6-astra".into()),
            reasoning_effort: Some("ultra".into()),
            ..Default::default()
        };
        let command = command_for(
            DistillationAiProvider::Codex,
            Path::new("/fake/codex"),
            &selected,
            &serde_json::json!({}),
            temp.path(),
        )
        .unwrap();
        let args: Vec<_> = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--model", "gpt-6-astra"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--config", "model_reasoning_effort=\"ultra\""])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--config", "agents.max_threads=4"])
        );
        assert_eq!(
            args.iter()
                .rfind(|arg| arg.starts_with("features.multi_agent="))
                .map(String::as_str),
            Some("features.multi_agent=true")
        );
        assert!(args.contains(&"features.shell_tool=false".into()));
        assert!(args.contains(&"mcp_servers={}".into()));
        assert!(args.contains(&"default_permissions=\"kb_app_distillation\"".into()));
    }

    #[test]
    fn cli_auth_errors_and_incomplete_outputs_never_become_decisions() {
        assert_eq!(
            parse_response(
                DistillationAiProvider::ClaudeCode,
                br#"{"is_error":true,"subtype":"success","structured_output":{}}"#
            ),
            Err(AiRunError::ProcessFailed)
        );
        assert_eq!(
            parse_response(
                DistillationAiProvider::ClaudeCode,
                br#"{"subtype":"success","result":"not structured"}"#
            ),
            Err(AiRunError::InvalidResponse)
        );
        assert_eq!(
            parse_response(
                DistillationAiProvider::Codex,
                br#"{"type":"item.completed","item":{"type":"agent_message","text":"{}"}}"#
            ),
            Err(AiRunError::InvalidResponse)
        );
        let codex = b"{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"{}\"}}\n{\"type\":\"turn.completed\"}\n";
        assert_eq!(
            parse_response(DistillationAiProvider::Codex, codex).unwrap(),
            serde_json::json!({})
        );
    }

    #[test]
    fn provider_specific_kb_disable_is_respected() {
        let settings = DistillationAiSettings {
            provider: Some(DistillationAiProvider::ClaudeCode),
            ..Default::default()
        };
        let kb = crate::settings::Settings {
            claude_kb_enabled: false,
            ..Default::default()
        };
        assert!(!kb_enabled(&settings, &kb));
        assert!(kb_enabled(
            &DistillationAiSettings {
                provider: Some(DistillationAiProvider::Codex),
                ..settings
            },
            &kb
        ));
    }

    #[test]
    fn outdated_or_missing_guard_prevents_running_before_model_contact() {
        use crate::ai_guard::{AiGuardStatus, GuardTargetState};
        let guard = AiGuardStatus {
            ready: false,
            codex: GuardTargetState::Outdated,
            claude: GuardTargetState::Enforced,
            guarded_paths: Vec::new(),
        };
        assert_eq!(
            provider_policy_with_guard(DistillationAiProvider::Codex, Some(&guard)),
            Err(AiRunError::GuardOutdated)
        );
        assert_eq!(
            provider_policy_with_guard(DistillationAiProvider::ClaudeCode, Some(&guard)),
            Ok(())
        );
        assert_eq!(
            provider_policy_with_guard(DistillationAiProvider::ClaudeCode, None),
            Err(AiRunError::GuardUnavailable)
        );
        let development = AiGuardStatus {
            codex: GuardTargetState::Development,
            ..guard
        };
        assert_eq!(
            provider_policy_with_guard(DistillationAiProvider::Codex, Some(&development)),
            Err(AiRunError::GuardUnavailable)
        );
    }

    #[test]
    fn codex_uses_a_restricted_profile_without_legacy_sandbox_override() {
        let temp = tempfile::tempdir().unwrap();
        let settings = DistillationAiSettings {
            provider: Some(DistillationAiProvider::Codex),
            ..Default::default()
        };
        let command = command_for(
            DistillationAiProvider::Codex,
            Path::new("/fake/codex"),
            &settings,
            &serde_json::json!({"type":"object"}),
            temp.path(),
        )
        .unwrap();
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--ignore-user-config".to_string()));
        assert!(args.contains(&"--ephemeral".to_string()));
        assert!(args.contains(&"default_permissions=\"kb_app_distillation\"".to_string()));
        assert!(
            !args
                .iter()
                .any(|arg| arg.starts_with("permissions.kb_app_distillation."))
        );
        assert!(!args.contains(&"features.hooks=false".to_string()));
        assert!(!args.iter().any(|arg| arg == "--sandbox"
            || arg.contains("bypass")
            || arg.contains("ignore-rules")));
        assert!(args.contains(&"mcp_servers={}".to_string()));
        assert!(args.contains(&"features.shell_tool=false".to_string()));
        assert!(args.contains(&"features.plugins=false".to_string()));
        assert_eq!(command.get_current_dir(), Some(temp.path()));
    }

    #[test]
    fn unknown_or_older_codex_versions_fail_closed() {
        assert!(!codex_version_supported("codex-cli 0.138.0"));
        assert!(!codex_version_supported("codex-cli 0.144.2"));
        assert!(!codex_version_supported("codex-cli 1.0.0"));
        assert!(!codex_version_supported("arbitrary wrapper"));
        assert!(codex_version_supported("codex-cli 0.144.3\n"));
        assert!(codex_version_supported("codex-cli 0.145.0"));
    }

    /// 2026-09-07: 公式Codex.app同梱版のalpha suffixを数値として扱い、
    /// 検証済みCLIまで非対応と判定していた。
    #[test]
    fn official_codex_prereleases_respect_the_stable_minimum() {
        assert!(codex_version_supported("codex-cli 0.147.0-alpha.6.6"));
        assert!(codex_version_supported("codex-cli 0.145.0-beta.1"));
        assert!(codex_version_supported("codex-cli 0.145.0-rc.1"));
        for version in [
            "0.144.3-alpha.1",
            "0.144.3-rc.1",
            "0.144.2-rc.9",
            "1.0.0-alpha.1",
            "0.147.invalid",
            "0.147.0-malformed.1",
            "0.147.0-alpha..1",
            "0.147.0-alpha.01",
            "0.147.0-alpha.1 extra",
            "0.0147.0",
            "0.147.+1",
        ] {
            assert!(
                !codex_version_supported(&format!("codex-cli {version}")),
                "{version}"
            );
        }
    }

    /// 2026-09-07: Finder起動では旧Homebrew CLIだけが見つかり、
    /// 公式アプリ同梱CLIが探索対象外だった。探索順を両経路で共通にする。
    #[cfg(unix)]
    #[test]
    fn bundled_codex_precedes_path_and_missing_bundle_falls_back() {
        let temp = tempfile::tempdir().unwrap();
        let bundled_dir = temp.path().join("bundle");
        let path_dir = temp.path().join("path");
        fs::create_dir_all(&bundled_dir).unwrap();
        fs::create_dir_all(&path_dir).unwrap();
        let bundled = fake_cli(&bundled_dir, "exit 0");
        let old = fake_cli(&path_dir, "exit 0");
        let old_codex = path_dir.join("codex");
        fs::rename(old, &old_codex).unwrap();
        let selected = |bundles| {
            executable_candidates(
                DistillationAiProvider::Codex,
                vec![PathBuf::from("relative/path"), path_dir.clone()],
                None,
                bundles,
            )
            .into_iter()
            .find(|candidate| executable_file(candidate))
        };
        assert_eq!(selected(vec![bundled.clone()]), Some(bundled.clone()));
        assert_eq!(
            selected(vec![temp.path().join("missing-bundle")]),
            Some(old_codex)
        );
        assert!(
            !executable_candidates(
                DistillationAiProvider::ClaudeCode,
                vec![path_dir],
                None,
                vec![bundled.clone()],
            )
            .contains(&bundled)
        );
    }

    #[cfg(unix)]
    fn fake_cli(temp: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = temp.join("fake-ai");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn prompt_goes_to_stdin_and_tools_are_disabled() {
        let temp = tempfile::tempdir().unwrap();
        let script = fake_cli(
            temp.path(),
            r#"
previous=''
for argument in "$@"; do
  [ "$argument" != 'private prompt' ] || exit 91
  if [ "$previous" = '--tools' ]; then [ -z "$argument" ] || exit 92; fi
  previous="$argument"
done
input=$(/bin/cat)
[ "$input" = 'private prompt' ] || exit 93
printf '%s' '{"subtype":"success","is_error":false,"structured_output":{"outcome":"keep"}}'
"#,
        );
        let settings = DistillationAiSettings {
            enabled: true,
            provider: Some(DistillationAiProvider::ClaudeCode),
            ..Default::default()
        };
        let result = run_at(
            &settings,
            &script,
            "private prompt",
            &serde_json::json!({"type":"object"}),
            || false,
        )
        .unwrap();
        assert_eq!(result["outcome"], "keep");
    }

    #[cfg(unix)]
    /// 2026-09-06: workspace全体の並列テストでは、fixtureのPIDファイル作成より先に
    /// timeoutが到達した。OSが返すPIDを使い、fixtureの起動速度を前提にしない。
    #[test]
    fn timeout_and_cancellation_reap_the_launched_process() {
        let child = spawn_process(Command::new("/bin/sleep").arg("30")).unwrap();
        let pid = child.id();
        let result = monitor_process(
            child,
            vec![0; 100_000],
            Duration::from_millis(10),
            100_000,
            || false,
        );
        assert_eq!(result, Err(AiRunError::TimedOut));
        assert_process_reaped(pid);

        let child = spawn_process(Command::new("/bin/sleep").arg("30")).unwrap();
        let pid = child.id();
        let polls = std::cell::Cell::new(0);
        let result = monitor_process(child, Vec::new(), Duration::from_secs(30), 100_000, || {
            polls.set(polls.get() + 1);
            polls.get() >= 2
        });
        assert_eq!(result, Err(AiRunError::Cancelled));
        assert!(polls.get() >= 2);
        assert_process_reaped(pid);
    }

    #[cfg(unix)]
    fn assert_process_reaped(pid: u32) {
        assert!(
            !Command::new("/bin/kill")
                .args(["-0", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        );
    }

    #[cfg(unix)]
    #[test]
    fn both_streams_are_drained_and_share_the_output_limit() {
        let temp = tempfile::tempdir().unwrap();
        let script = fake_cli(
            temp.path(),
            "while :; do printf '01234567890123456789'; printf '01234567890123456789' >&2; done",
        );
        let result = execute(
            &mut Command::new(&script),
            Vec::new(),
            Duration::from_secs(3),
            4096,
            || false,
        );
        assert_eq!(result, Err(AiRunError::OutputLimit));
    }
}
