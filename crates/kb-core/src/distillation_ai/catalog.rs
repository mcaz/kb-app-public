//! モデル候補はCLIの正式な一覧だけを使い、推論を開始せず取得する。
//! https://learn.chatgpt.com/docs/app-server#list-models-modellist
//! https://github.com/anthropics/claude-agent-sdk-python/blob/main/src/claude_agent_sdk/_internal/query.py

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};

use super::*;

// 一覧は設定画面で読むため、推論本体より短く止める。ページ循環も終了条件にする。
const CATALOG_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_PAGES: usize = 10;
const MAX_MODELS: usize = 500;

pub(super) fn read(
    provider: DistillationAiProvider,
    program: &Path,
    include_hidden: bool,
    cancelled: impl Fn() -> bool,
) -> std::result::Result<Vec<DistillationModel>, AiRunError> {
    verify_cli(provider, program, &cancelled)?;
    let working = tempfile::Builder::new()
        .prefix("kb-model-catalog-")
        .tempdir()
        .map_err(|_| AiRunError::Io)?;
    let mut command = Command::new(program);
    command.current_dir(working.path());
    match provider {
        DistillationAiProvider::Codex => command.args([
            "app-server",
            "--stdio",
            // 推論側は--ignore-user-configで標準providerを使う。一覧も同じ対象に揃える。
            "--config",
            "model_provider=\"openai\"",
            "--config",
            "mcp_servers={}",
            "--config",
            "features.plugins=false",
            "--config",
            "features.apps=false",
        ]),
        DistillationAiProvider::ClaudeCode => command.args([
            "--print",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--verbose",
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
        ]),
    };
    read_command(
        provider,
        &mut command,
        include_hidden,
        CATALOG_TIMEOUT,
        cancelled,
    )
}

fn read_command(
    provider: DistillationAiProvider,
    command: &mut Command,
    include_hidden: bool,
    timeout: Duration,
    cancelled: impl Fn() -> bool,
) -> std::result::Result<Vec<DistillationModel>, AiRunError> {
    if cancelled() {
        return Err(AiRunError::Cancelled);
    }
    let mut child = spawn_process(command)?;
    let result = exchange(provider, &mut child, include_hidden, timeout, cancelled);
    // app-serverは長寿命プロセス。成功時も自分が起動したgroupを回収する。
    stop_process(&mut child);
    result
}

fn exchange(
    provider: DistillationAiProvider,
    child: &mut Child,
    include_hidden: bool,
    timeout: Duration,
    cancelled: impl Fn() -> bool,
) -> std::result::Result<Vec<DistillationModel>, AiRunError> {
    let stdout = child.stdout.take().ok_or(AiRunError::Io)?;
    let stderr = child.stderr.take().ok_or(AiRunError::Io)?;
    let mut stdin = child.stdin.take().ok_or(AiRunError::Io)?;
    let total = Arc::new(AtomicUsize::new(0));
    let (sender, receiver) = mpsc::sync_channel(32);
    let stdout_total = total.clone();
    let stdout_sender = sender.clone();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout).take((MAX_OUTPUT_BYTES + 1) as u64);
        loop {
            let mut line = Vec::new();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => break,
                Ok(count) => {
                    if stdout_total.fetch_add(count, Ordering::Relaxed) + count > MAX_OUTPUT_BYTES {
                        let _ = stdout_sender.send(Err(AiRunError::OutputLimit));
                        break;
                    }
                    let parsed =
                        serde_json::from_slice(&line).map_err(|_| AiRunError::InvalidResponse);
                    if stdout_sender.send(parsed).is_err() {
                        break;
                    }
                }
                Err(_) => {
                    let _ = stdout_sender.send(Err(AiRunError::Io));
                    break;
                }
            }
        }
    });
    let stderr_total = total.clone();
    thread::spawn(move || {
        if drain(stderr, &stderr_total, MAX_OUTPUT_BYTES).is_err() {
            let _ = sender.send(Err(AiRunError::Io));
        }
    });
    let initialize = match provider {
        DistillationAiProvider::Codex => serde_json::json!({
            "id": 1,
            "method": "initialize",
            "params": {
                "clientInfo": {"name":"kb_app_model_catalog", "version":env!("CARGO_PKG_VERSION")},
                "capabilities": {"experimentalApi":true}
            }
        }),
        // SDKのconnect(prompt=None)と同じ順序。userメッセージは一切送らない。
        DistillationAiProvider::ClaudeCode => serde_json::json!({
            "type":"control_request",
            "request_id":"kb-app-models",
            "request":{"subtype":"initialize","hooks":null}
        }),
    };
    send(&mut stdin, &initialize)?;
    let started = Instant::now();
    let mut response_id = 1;
    let mut pages = 0;
    let mut cursors = BTreeSet::new();
    let mut result = Vec::new();
    loop {
        if cancelled() {
            return Err(AiRunError::Cancelled);
        }
        if started.elapsed() >= timeout {
            return Err(AiRunError::TimedOut);
        }
        if total.load(Ordering::Relaxed) > MAX_OUTPUT_BYTES {
            return Err(AiRunError::OutputLimit);
        }
        let event: Value = match receiver.recv_timeout(POLL_INTERVAL) {
            Ok(event) => event?,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if child.try_wait().map_err(|_| AiRunError::Io)?.is_some() {
                    return Err(AiRunError::ProcessFailed);
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(AiRunError::ProcessFailed),
        };
        if provider == DistillationAiProvider::ClaudeCode {
            if event["type"] != "control_response"
                || event["response"]["request_id"] != "kb-app-models"
            {
                continue;
            }
            return match event["response"]["subtype"].as_str() {
                Some("success") => parse_claude_models(&event["response"]["response"]),
                Some("error") => Err(AiRunError::ProcessFailed),
                _ => Err(AiRunError::InvalidResponse),
            };
        }
        if event.get("id").and_then(Value::as_u64) != Some(response_id) {
            continue;
        }
        if event.get("error").is_some() {
            return Err(AiRunError::ProcessFailed);
        }
        let response = event.get("result").ok_or(AiRunError::InvalidResponse)?;
        let cursor = if response_id == 1 {
            send(&mut stdin, &serde_json::json!({"method":"initialized"}))?;
            None
        } else {
            pages += 1;
            let page = parse_page(response, include_hidden)?;
            result.extend(page.models);
            if result.len() > MAX_MODELS {
                return Err(AiRunError::OutputLimit);
            }
            match page.next_cursor {
                None => return unique_models(result),
                Some(cursor) if pages < MAX_PAGES && cursors.insert(cursor.clone()) => Some(cursor),
                Some(_) => return Err(AiRunError::InvalidResponse),
            }
        };
        response_id += 1;
        send(
            &mut stdin,
            &serde_json::json!({
                "id":response_id,
                "method":"model/list",
                "params":{"cursor":cursor,"limit":100,"includeHidden":include_hidden}
            }),
        )?;
    }
}

fn send(stream: &mut impl Write, message: &Value) -> std::result::Result<(), AiRunError> {
    serde_json::to_writer(&mut *stream, message).map_err(|_| AiRunError::Io)?;
    stream.write_all(b"\n").map_err(|_| AiRunError::Io)?;
    stream.flush().map_err(|_| AiRunError::Io)
}

struct Page {
    models: Vec<DistillationModel>,
    next_cursor: Option<String>,
}

fn parse_claude_models(value: &Value) -> std::result::Result<Vec<DistillationModel>, AiRunError> {
    let models = value["models"]
        .as_array()
        .ok_or(AiRunError::InvalidResponse)?;
    if models.len() > MAX_MODELS {
        return Err(AiRunError::OutputLimit);
    }
    let mut normalized = Vec::new();
    for model in models {
        let efforts = match model.get("supportedEffortLevels") {
            None => Vec::new(),
            Some(Value::Array(efforts)) if efforts.len() <= 32 => efforts
                .iter()
                .map(|effort| serde_json::json!({"reasoningEffort":effort}))
                .collect(),
            _ => return Err(AiRunError::InvalidResponse),
        };
        // ClaudeのModelInfoには既定モデル・既定強度がない。順序から補わない。
        normalized.push(serde_json::json!({
            "model":model["value"],
            "displayName":model["displayName"],
            "supportedReasoningEfforts":efforts,
            "defaultReasoningEffort":null,
            "isDefault":false,
            "hidden":false
        }));
    }
    unique_models(
        parse_page(
            &serde_json::json!({"data":normalized,"nextCursor":null}),
            true,
        )?
        .models,
    )
}

fn parse_page(value: &Value, include_hidden: bool) -> std::result::Result<Page, AiRunError> {
    let data = value["data"]
        .as_array()
        .ok_or(AiRunError::InvalidResponse)?;
    if data.len() > MAX_MODELS {
        return Err(AiRunError::OutputLimit);
    }
    let next_cursor = match value.get("nextCursor") {
        Some(Value::Null) | None => None,
        Some(Value::String(cursor)) if !cursor.is_empty() && cursor.len() <= 1024 => {
            Some(cursor.clone())
        }
        _ => return Err(AiRunError::InvalidResponse),
    };
    let mut models = Vec::new();
    for entry in data {
        if !include_hidden && entry["hidden"].as_bool() == Some(true) {
            continue;
        }
        let model = entry["model"].as_str().ok_or(AiRunError::InvalidResponse)?;
        let display_name = entry["displayName"]
            .as_str()
            .ok_or(AiRunError::InvalidResponse)?;
        let supported = entry["supportedReasoningEfforts"]
            .as_array()
            .ok_or(AiRunError::InvalidResponse)?;
        if display_name.is_empty() || display_name.len() > 200 || supported.len() > 32 {
            return Err(AiRunError::InvalidResponse);
        }
        let mut supported_reasoning_efforts = Vec::new();
        for option in supported {
            let effort = option["reasoningEffort"]
                .as_str()
                .ok_or(AiRunError::InvalidResponse)?;
            let settings = DistillationAiSettings {
                provider: Some(DistillationAiProvider::Codex),
                model: Some(model.into()),
                reasoning_effort: Some(effort.into()),
                ..Default::default()
            };
            settings
                .validate()
                .map_err(|_| AiRunError::InvalidResponse)?;
            if !supported_reasoning_efforts
                .iter()
                .any(|existing| existing == effort)
            {
                supported_reasoning_efforts.push(effort.to_owned());
            }
        }
        DistillationAiSettings {
            model: Some(model.into()),
            ..Default::default()
        }
        .validate()
        .map_err(|_| AiRunError::InvalidResponse)?;
        let default_reasoning_effort = match entry.get("defaultReasoningEffort") {
            None | Some(Value::Null) => None,
            Some(Value::String(effort)) if supported_reasoning_efforts.contains(effort) => {
                Some(effort.clone())
            }
            _ => return Err(AiRunError::InvalidResponse),
        };
        models.push(DistillationModel {
            model: model.into(),
            display_name: display_name.into(),
            supported_reasoning_efforts,
            default_reasoning_effort,
            is_default: entry["isDefault"]
                .as_bool()
                .ok_or(AiRunError::InvalidResponse)?,
        });
    }
    Ok(Page {
        models,
        next_cursor,
    })
}

fn unique_models(
    models: Vec<DistillationModel>,
) -> std::result::Result<Vec<DistillationModel>, AiRunError> {
    let mut identifiers = BTreeSet::new();
    if models.iter().any(|model| !identifiers.insert(&model.model)) {
        return Err(AiRunError::InvalidResponse);
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(model: &str, hidden: bool) -> Value {
        serde_json::json!({
            "model":model,
            "displayName":"Test model",
            "supportedReasoningEfforts":[{"reasoningEffort":"high"},{"reasoningEffort":"ultra"}],
            "defaultReasoningEffort":"high",
            "isDefault":false,
            "hidden":hidden
        })
    }

    #[test]
    fn catalog_preserves_exact_supported_efforts_and_hides_hidden_models() {
        let value = serde_json::json!({"data":[entry("test-model",false),entry("hidden-model",true)],"nextCursor":null});
        let visible = parse_page(&value, false).unwrap();
        assert_eq!(visible.models.len(), 1);
        assert_eq!(
            visible.models[0].supported_reasoning_efforts,
            ["high", "ultra"]
        );
        assert_eq!(
            visible.models[0].default_reasoning_effort.as_deref(),
            Some("high")
        );
        assert_eq!(parse_page(&value, true).unwrap().models.len(), 2);
        let mut invalid = value;
        invalid["data"][0]["defaultReasoningEffort"] = serde_json::json!("unknown");
        assert!(matches!(
            parse_page(&invalid, false),
            Err(AiRunError::InvalidResponse)
        ));
    }

    #[test]
    fn claude_catalog_keeps_provider_fields_without_guessing_defaults() {
        let value = serde_json::json!({"models":[
            {"value":"test-claude","displayName":"Test Claude","supportedEffortLevels":["low","high","max"]},
            {"value":"without-effort","displayName":"No effort"}
        ]});
        let models = parse_claude_models(&value).unwrap();
        assert_eq!(
            models[0].supported_reasoning_efforts,
            ["low", "high", "max"]
        );
        assert_eq!(models[0].default_reasoning_effort, None);
        assert!(!models[0].is_default);
        assert!(models[1].supported_reasoning_efforts.is_empty());
        for invalid in [
            serde_json::json!({}),
            serde_json::json!({"models":[{"value":"test","displayName":"test","supportedEffortLevels":"max"}]}),
        ] {
            assert_eq!(
                parse_claude_models(&invalid),
                Err(AiRunError::InvalidResponse)
            );
        }
    }

    #[cfg(unix)]
    fn fake_command(temp: &Path, body: &str) -> Command {
        use std::os::unix::fs::PermissionsExt;
        let path = temp.join("fake-model-catalog");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Command::new(path)
    }

    #[cfg(unix)]
    #[test]
    fn protocol_initializes_and_paginates_without_starting_a_turn() {
        let temp = tempfile::tempdir().unwrap();
        let first = serde_json::json!({"id":2,"result":{"data":[entry("test-a",false)],"nextCursor":"page-two"}});
        let second =
            serde_json::json!({"id":3,"result":{"data":[entry("test-b",false)],"nextCursor":null}});
        let body = format!(
            r#"
IFS= read -r request
case "$request" in *'"method":"initialize"'*) ;; *) exit 91;; esac
printf '%s\n' '{{"id":1,"result":{{}}}}'
IFS= read -r request
case "$request" in *'"method":"initialized"'*) ;; *) exit 92;; esac
IFS= read -r request
case "$request" in *'"method":"model/list"'*) ;; *) exit 93;; esac
printf '%s\n' '{first}'
IFS= read -r request
case "$request" in *'"cursor":"page-two"'*) ;; *) exit 94;; esac
printf '%s\n' '{second}'
/bin/sleep 30
"#
        );
        let models = read_command(
            DistillationAiProvider::Codex,
            &mut fake_command(temp.path(), &body),
            false,
            Duration::from_secs(3),
            || false,
        )
        .unwrap();
        assert_eq!(
            models
                .iter()
                .map(|entry| entry.model.as_str())
                .collect::<Vec<_>>(),
            ["test-a", "test-b"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn claude_protocol_only_initializes_and_never_sends_a_user_message() {
        let temp = tempfile::tempdir().unwrap();
        let body = r#"
IFS= read -r request
case "$request" in *'"type":"control_request"'*) ;; *) exit 91;; esac
case "$request" in *'"subtype":"initialize"'*) ;; *) exit 92;; esac
case "$request" in *'"hooks":null'*) ;; *) exit 93;; esac
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"kb-app-models","response":{"models":[{"value":"test-claude","displayName":"Test Claude","supportedEffortLevels":["high","max"]}]}}}'
IFS= read -r unexpected
exit 94
"#;
        let models = read_command(
            DistillationAiProvider::ClaudeCode,
            &mut fake_command(temp.path(), body),
            false,
            Duration::from_secs(3),
            || false,
        )
        .unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model, "test-claude");
        assert_eq!(models[0].supported_reasoning_efforts, ["high", "max"]);
    }

    #[cfg(unix)]
    #[test]
    fn claude_protocol_rejects_errors_and_partial_model_responses() {
        let temp = tempfile::tempdir().unwrap();
        for (subtype, payload, expected) in [
            ("error", serde_json::json!({}), AiRunError::ProcessFailed),
            (
                "success",
                serde_json::json!({}),
                AiRunError::InvalidResponse,
            ),
            (
                "unknown",
                serde_json::json!({"models":[]}),
                AiRunError::InvalidResponse,
            ),
        ] {
            let response = serde_json::json!({"type":"control_response","response":{"subtype":subtype,"request_id":"kb-app-models","response":payload}});
            let body = format!("IFS= read -r request\nprintf '%s\\n' '{response}'\n/bin/sleep 30");
            assert_eq!(
                read_command(
                    DistillationAiProvider::ClaudeCode,
                    &mut fake_command(temp.path(), &body),
                    false,
                    Duration::from_secs(3),
                    || false
                ),
                Err(expected)
            );
        }
        let polls = std::cell::Cell::new(0);
        assert_eq!(
            read_command(
                DistillationAiProvider::ClaudeCode,
                &mut fake_command(temp.path(), "/bin/sleep 30"),
                false,
                Duration::from_secs(3),
                || {
                    polls.set(polls.get() + 1);
                    polls.get() > 2
                }
            ),
            Err(AiRunError::Cancelled)
        );
    }

    #[cfg(unix)]
    #[test]
    fn catalog_stops_on_timeout_cancellation_and_output_overflow() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(
            read_command(
                DistillationAiProvider::Codex,
                &mut fake_command(temp.path(), "/bin/sleep 30"),
                false,
                Duration::from_millis(20),
                || false
            ),
            Err(AiRunError::TimedOut)
        );
        let polls = std::cell::Cell::new(0);
        assert_eq!(
            read_command(
                DistillationAiProvider::Codex,
                &mut fake_command(temp.path(), "/bin/sleep 30"),
                false,
                Duration::from_secs(2),
                || {
                    polls.set(polls.get() + 1);
                    polls.get() > 2
                }
            ),
            Err(AiRunError::Cancelled)
        );
        let body = "IFS= read -r request\n/usr/bin/head -c 2100000 /dev/zero";
        let result = read_command(
            DistillationAiProvider::Codex,
            &mut fake_command(temp.path(), body),
            false,
            Duration::from_secs(3),
            || false,
        );
        assert_eq!(result, Err(AiRunError::OutputLimit));
    }

    #[cfg(unix)]
    #[test]
    fn repeated_page_cursor_is_rejected_without_partial_catalog() {
        let temp = tempfile::tempdir().unwrap();
        let body = r#"
IFS= read -r request
printf '%s\n' '{"id":1,"result":{}}'
IFS= read -r request
IFS= read -r request
printf '%s\n' '{"id":2,"result":{"data":[],"nextCursor":"again"}}'
IFS= read -r request
printf '%s\n' '{"id":3,"result":{"data":[],"nextCursor":"again"}}'
/bin/sleep 30
"#;
        assert_eq!(
            read_command(
                DistillationAiProvider::Codex,
                &mut fake_command(temp.path(), body),
                false,
                Duration::from_secs(3),
                || false
            ),
            Err(AiRunError::InvalidResponse)
        );
    }
}
