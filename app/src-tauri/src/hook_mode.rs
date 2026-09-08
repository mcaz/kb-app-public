//! `UserPromptSubmit` の本文取得と、Claude `SessionStart` の開始観測を取り次ぐ。
//!
//! フックが CLI や Vault を直に読むと契約8の取次口が二重化するため、同じ実行ファイルを
//! MCP server として子起動し、JSON-RPC だけで取得する。initialize の ON/OFF metadata と
//! tools/call の構造化終端応答に従い、OFF・ON不明では観測台帳も開かない。

#[cfg(test)]
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::Instant;

use anyhow::{Context, Result};
use kb_core::client_notice::ClientNotice;
use kb_core::client_surface::ClientSurface;
use kb_core::hook_delivery::{HookDelivery, render_hook_delivery};
use kb_core::session_ledger::{
    self, AppendOutcome, CapAssumption, EventContext, FinalOutputFlags, HookEmissionOutcome,
    HookErrorStage, HookFilterReason, HookObservation, HookReceipt, HookTimings, LedgerEvent,
    MeasurementContext, ObservationArm, PermissionMode,
};
use serde_json::{Value, json};

const MAX_QUERY_CHARS: usize = 300;
const SEARCH_SEED_LIMIT: usize = 5;
const PROTOCOL_VERSION: &str = "2025-06-18";
const KB_DISABLED_CODE: &str = "kb_disabled";
// 通常出力と同様、JSONに見える `[` から始めるとhostに劣化通知まで拒否される。
const RETRIEVAL_FAILURE: &str = "# [kb-app 自動retrieval — 劣化] MCP検索を実行できなかった。正常な該当なしと区別し、ユーザーへ知らせること。\n";
const WORKSPACE_UNVERIFIED: &str = "# [kb-app 自動retrieval — 劣化] workspace_unverified: 接続先のKBを確認できなかったため、本文配信を停止した。kb-appの接続設定を確認して再接続する必要があると、ユーザーへ知らせること。\n";
const VAULT_MISMATCH: &str = "# [kb-app 自動retrieval — 劣化] vault_mismatch: 接続時に登録したKBと現在のKBが一致しないため、本文配信を停止した。kb-appの接続設定を確認して再接続する必要があると、ユーザーへ知らせること。\n";
const LEDGER_FAILURE: &str =
    "⚠ 劣化: session_ledger 計測台帳へ記録できなかった。取得・出力の計測は未確認。\n";
const SESSION_START_FAILURE: &str =
    "kb-app: session_start_observation_unavailable; 会話の開始計測を確認できなかった。\n";

fn child_mcp_args(client: &str) -> [&str; 10] {
    [
        "--mcp",
        "--no-remote-sync",
        "--hook-context",
        // Vaultの選択とID照合は、ON確認後の子MCPに任せる。
        "--require-client-binding",
        "--mcp-surface",
        "read",
        "--client",
        client,
        // host の read 面と同じ surface なので、hook 用の配信 profile(契約 8 の数値)は
        // 引数で明示する。host 既定(session-auto)と同値だが、既定の変更が hook 経路へ
        // 波及しないよう明示のまま維持する。
        "--retrieval-profile",
        "session-auto",
    ]
}

/// 管理hookモードなら実行して true を返す。通常起動なら false。
pub fn run_if_requested() -> bool {
    let args: Vec<String> = std::env::args().collect();
    let retrieval = args.iter().any(|arg| arg == "--hook-auto-retrieve");
    let session_start = args.iter().any(|arg| arg == "--hook-session-start");
    if !retrieval && !session_start {
        return false;
    }

    let flag = |name: &str| {
        args.iter()
            .position(|arg| arg == name)
            .and_then(|index| args.get(index + 1))
            .cloned()
    };
    let client = flag("--client").unwrap_or_else(|| "mcp-client/unknown".into());
    if session_start {
        let result = if retrieval {
            Err(anyhow::anyhow!("conflicting hook modes"))
        } else {
            run_session_start(&client, flag("--vault").as_deref())
        };
        report_session_start_result(result, &mut std::io::stderr().lock());
        return true;
    }
    if let Err(error) = run(&client, flag("--vault").as_deref()) {
        eprintln!("kb-app auto retrieval: {error:#}");
        let _ = write_output(&mut std::io::stdout().lock(), RETRIEVAL_FAILURE);
    }
    true
}

fn report_session_start_result(result: Result<()>, errors: &mut impl Write) {
    if result.is_err() {
        // 開始計測の失敗は会話を止めず、入力や内部診断をhostへ転記しない。
        let _ = errors.write_all(SESSION_START_FAILURE.as_bytes());
    }
}

fn run_session_start(client: &str, vault: Option<&str>) -> Result<()> {
    // 子MCPの起動・接続時間を開始時刻へ足さない。host自身の生成時刻とは区別する。
    let observed_at_ms = session_ledger::now_ms();
    if ClientSurface::from_hint(client) != ClientSurface::ClaudeCode {
        return Ok(());
    }
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let payload: Value = serde_json::from_str(&input).context("hook input parse")?;
    if !is_session_start(&payload) {
        return Ok(());
    }
    let exe = std::env::current_exe().context("current executable")?;
    let mut rpc = ChildMcp::start_with_stderr(&exe, client, vault, Stdio::null())?;
    execute_session_start(
        &mut rpc,
        &payload,
        client,
        observed_at_ms,
        &mut DefaultSessionStartSink,
    )
}

fn is_session_start(payload: &Value) -> bool {
    payload.get("hook_event_name").and_then(Value::as_str) == Some("SessionStart")
}

trait SessionStartSink {
    fn record(
        &mut self,
        workspace_id: &str,
        session_id: Option<&str>,
        source: Option<&str>,
        observed_at_ms: i64,
    ) -> Result<()>;
}

struct DefaultSessionStartSink;

impl SessionStartSink for DefaultSessionStartSink {
    fn record(
        &mut self,
        workspace_id: &str,
        session_id: Option<&str>,
        source: Option<&str>,
        observed_at_ms: i64,
    ) -> Result<()> {
        session_ledger::record_session_start(workspace_id, session_id, source, observed_at_ms)
    }
}

fn execute_session_start(
    rpc: &mut impl Rpc,
    payload: &Value,
    client: &str,
    observed_at_ms: i64,
    sink: &mut impl SessionStartSink,
) -> Result<()> {
    if ClientSurface::from_hint(client) != ClientSurface::ClaudeCode || !is_session_start(payload) {
        return Ok(());
    }
    let initialized = rpc.request(
        "initialize",
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "kb_app_session_observation": true,
            "clientInfo": {"name": "kb-app-session-start-observation", "version": env!("CARGO_PKG_VERSION")}
        }),
    )?;
    if initialized.get("error").is_some() {
        anyhow::bail!("session start initialize failed");
    }
    let metadata = &initialized["result"]["capabilities"]["experimental"]["kbApp"];
    if metadata.get("kb_enabled").and_then(Value::as_bool) != Some(true) {
        return Ok(());
    }
    let binding = &metadata["session_observation_binding"];
    if binding.get("verified").and_then(Value::as_bool) != Some(true) {
        return Ok(());
    }
    let Some(workspace_id) = binding
        .get("workspace_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    else {
        return Ok(());
    };
    sink.record(
        workspace_id,
        payload.get("session_id").and_then(Value::as_str),
        payload.get("source").and_then(Value::as_str),
        observed_at_ms,
    )
}

fn run(client: &str, vault: Option<&str>) -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let payload: Value = serde_json::from_str(&input).context("hook input parse")?;
    if !is_user_prompt_submit(&payload) {
        return Ok(());
    }

    let exe = std::env::current_exe().context("current executable")?;
    let mut rpc = ChildMcp::start(&exe, client, vault)?;
    // 部分的にstdoutへ書けた後の失敗へ、別の警告を追記すると予算も計測も壊れる。
    // 出力失敗はここで止め、上位の検索失敗用fallbackを再出力しない。
    if execute_hook(
        &mut rpc,
        &payload,
        client,
        &mut DefaultLedger,
        &mut std::io::stdout().lock(),
    )
    .is_err()
    {
        eprintln!("kb-app auto retrieval: stdout_write_failed");
    }
    Ok(())
}

fn is_user_prompt_submit(payload: &Value) -> bool {
    payload.get("hook_event_name").and_then(Value::as_str) == Some("UserPromptSubmit")
}

fn filtered_query(payload: &Value) -> std::result::Result<String, HookFilterReason> {
    if !is_user_prompt_submit(payload) {
        return Err(HookFilterReason::NonUserEvent);
    }
    let prompt = payload
        .get("prompt")
        .and_then(Value::as_str)
        .ok_or(HookFilterReason::MissingPrompt)?
        .trim();
    if prompt.chars().count() < 4 {
        return Err(HookFilterReason::ShortPrompt);
    }
    if prompt.starts_with('/') {
        return Err(HookFilterReason::SlashCommand);
    }
    if prompt.contains("[SYSTEM NOTIFICATION") {
        return Err(HookFilterReason::SystemNotification);
    }
    if prompt.contains("<task-notification>") {
        return Err(HookFilterReason::TaskNotification);
    }
    Ok(prompt
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_QUERY_CHARS)
        .collect())
}

#[cfg(test)]
fn query_from_payload(payload: &Value) -> Option<String> {
    filtered_query(payload).ok()
}

trait Rpc {
    fn request(&mut self, method: &str, params: Value) -> Result<Value>;
}

struct RetrievedContext {
    structured: Value,
    delivery: HookDelivery,
}

struct RetrievalAttempt {
    enabled: bool,
    arm: ObservationArm,
    workspace_id: Option<String>,
    timings: HookTimings,
    error_stage: HookErrorStage,
    failure_notice: Option<&'static str>,
    connection_notice: Option<ClientNotice>,
    result: Result<Option<RetrievedContext>>,
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn retrieve_attempt(rpc: &mut impl Rpc, query: Option<&str>, client: &str) -> RetrievalAttempt {
    let mut attempt = RetrievalAttempt {
        enabled: false,
        arm: ObservationArm::Unknown,
        workspace_id: None,
        timings: HookTimings {
            initialize_ms: None,
            search_ms: None,
            render_ms: None,
        },
        error_stage: HookErrorStage::Initialize,
        failure_notice: None,
        connection_notice: None,
        result: Ok(None),
    };
    attempt.result = retrieve_context(rpc, query, client, &mut attempt);
    attempt
}

fn retrieve_context(
    rpc: &mut impl Rpc,
    query: Option<&str>,
    client: &str,
    attempt: &mut RetrievalAttempt,
) -> Result<Option<RetrievedContext>> {
    let started = Instant::now();
    let initialized = rpc.request(
        "initialize",
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "kb-app-auto-retrieval", "version": env!("CARGO_PKG_VERSION")}
        }),
    );
    attempt.timings.initialize_ms = Some(elapsed_ms(started));
    let initialized = initialized?;
    let enabled = initialized
        .pointer("/result/capabilities/experimental/kbApp/kb_enabled")
        .and_then(Value::as_bool);
    attempt.enabled = enabled == Some(true);
    if attempt.enabled {
        attempt.arm = observation_arm(
            &initialized["result"]["capabilities"]["experimental"]["kbApp"]["observation_measurement"],
        );
    }
    if enabled == Some(false) {
        attempt.connection_notice = guard_notice(
            &initialized["result"]["capabilities"]["experimental"]["kbApp"]["client_notices"],
            client,
        );
        return Ok(None);
    }
    if enabled == Some(true)
        && !initialized
            .pointer("/result/capabilities/tools")
            .is_some_and(Value::is_object)
    {
        attempt.error_stage = HookErrorStage::Protocol;
        anyhow::bail!("enabled MCP response has no valid tools capability");
    }
    let Some(query) = query else {
        return Ok(None);
    };
    if initialized.pointer("/result/capabilities/tools").is_none() {
        return Ok(None);
    }

    attempt.error_stage = HookErrorStage::Search;
    // 設定は要求ごとに再評価される。検索応答の欠落をinitialize時の状態で埋めない。
    attempt.arm = ObservationArm::Unknown;
    let started = Instant::now();
    let searched = rpc.request(
        "tools/call",
        json!({
            "name": "search",
            "arguments": {
                "query": query,
                "limit": SEARCH_SEED_LIMIT,
                "any": true,
                "include_documents": true
            }
        }),
    );
    attempt.timings.search_ms = Some(elapsed_ms(started));
    let mut searched = searched?;
    if tool_is_authoritatively_disabled(&searched) {
        attempt.enabled = false;
        attempt.connection_notice = guard_notice(
            &searched["result"]["structuredContent"]["client_notices"],
            client,
        );
        return Ok(None);
    }
    if attempt.enabled {
        attempt.arm =
            observation_arm(&searched["result"]["structuredContent"]["observation_measurement"]);
    }
    attempt.failure_notice = binding_failure_notice(&searched);
    ensure_tool_succeeded(&searched, "search")?;
    attempt.workspace_id = match searched.pointer("/result/structuredContent/workspace_id") {
        Some(Value::String(id)) => Some(id.clone()),
        None | Some(Value::Null) => None,
        Some(_) => {
            attempt.error_stage = HookErrorStage::Protocol;
            anyhow::bail!("search response workspace ID has an invalid type");
        }
    };
    attempt.error_stage = HookErrorStage::Render;
    let started = Instant::now();
    let result = (|| {
        let mut structured = searched
            .pointer_mut("/result/structuredContent")
            .context("search response has no structured content")?
            .take();
        let object = structured
            .as_object_mut()
            .context("search structured content is not an object")?;
        // 子MCPの固定codeだけを採用する。clientInfo.versionは子の版でありhostの証拠ではない。
        if attempt.enabled
            && is_coding_client(client)
            && ClientNotice::HostCapabilityUnverified.is_present_in(
                &initialized["result"]["capabilities"]["experimental"]["kbApp"]["client_notices"],
            )
        {
            object.insert(
                "client_notices".into(),
                json!([ClientNotice::HostCapabilityUnverified.value()]),
            );
        } else {
            object.remove("client_notices");
        }
        let budget = ClientSurface::from_hint(client).hook_output_budget();
        let delivery = render_hook_delivery(&structured, budget)?;
        Ok(Some(RetrievedContext {
            structured,
            delivery,
        }))
    })();
    attempt.timings.render_ms = Some(elapsed_ms(started));
    result
}

fn is_coding_client(client: &str) -> bool {
    matches!(
        ClientSurface::from_hint(client),
        ClientSurface::CodexCli | ClientSurface::ClaudeCode
    )
}

fn observation_arm(metadata: &Value) -> ObservationArm {
    metadata
        .get("arm")
        .and_then(|arm| serde_json::from_value(arm.clone()).ok())
        .unwrap_or_default()
}

fn guard_notice(notices: &Value, client: &str) -> Option<ClientNotice> {
    (is_coding_client(client) && ClientNotice::GuardOutdated.is_present_in(notices))
        .then_some(ClientNotice::GuardOutdated)
}

#[cfg(test)]
fn retrieve(rpc: &mut impl Rpc, query: &str, client: &str) -> Result<Option<String>> {
    retrieve_attempt(rpc, Some(query), client)
        .result
        .map(|retrieved| retrieved.map(|retrieved| retrieved.delivery.text))
}

trait LedgerSink {
    fn session_start(
        &mut self,
        _context: EventContext<'_>,
        measurement: MeasurementContext,
    ) -> Result<MeasurementContext> {
        Ok(measurement)
    }
    fn harvest(
        &mut self,
        context: EventContext<'_>,
        cadence: &Value,
    ) -> Result<kb_core::harvest::PreparedStatus>;
    fn harvest_emitted(&mut self, emission: &kb_core::harvest::Emission) -> Result<()>;

    fn append(&mut self, event: &LedgerEvent) -> Result<AppendOutcome>;
    fn finalize(&mut self, receipt: &HookReceipt, outcome: HookEmissionOutcome) -> Result<()>;
}

struct DefaultLedger;

impl LedgerSink for DefaultLedger {
    fn session_start(
        &mut self,
        context: EventContext<'_>,
        measurement: MeasurementContext,
    ) -> Result<MeasurementContext> {
        let evidence = if context.surface == ClientSurface::ClaudeCode {
            context
                .workspace_id
                .map(|workspace| session_ledger::read_session_start(workspace, context.session_id))
                .transpose()?
                .flatten()
        } else {
            None
        };
        Ok(measurement.with_session_start(evidence))
    }
    fn harvest(
        &mut self,
        context: EventContext<'_>,
        cadence: &Value,
    ) -> Result<kb_core::harvest::PreparedStatus> {
        kb_core::harvest::prepare(context, cadence)
    }
    fn harvest_emitted(&mut self, emission: &kb_core::harvest::Emission) -> Result<()> {
        kb_core::harvest::mark_emitted(emission)
    }

    fn append(&mut self, event: &LedgerEvent) -> Result<AppendOutcome> {
        session_ledger::append(event)
    }

    fn finalize(&mut self, receipt: &HookReceipt, outcome: HookEmissionOutcome) -> Result<()> {
        session_ledger::finalize_hook(receipt, outcome)
    }
}

fn event_context<'a>(
    payload: &'a Value,
    surface: ClientSurface,
    workspace_id: Option<&'a str>,
) -> EventContext<'a> {
    EventContext {
        surface,
        workspace_id,
        session_id: payload.get("session_id").and_then(Value::as_str),
        turn_id: payload.get("turn_id").and_then(Value::as_str),
        prompt_id: payload.get("prompt_id").and_then(Value::as_str),
        permission_mode: payload
            .get("permission_mode")
            .and_then(Value::as_str)
            .and_then(PermissionMode::from_hint),
    }
}

fn append_observation(
    ledger: &mut impl LedgerSink,
    context: EventContext<'_>,
    measurement: MeasurementContext,
    observation: HookObservation,
) -> Result<AppendOutcome> {
    let event = LedgerEvent::hook(context, session_ledger::now_ms(), observation)?
        .with_measurement(measurement)?;
    ledger.append(&event)
}

fn write_output(output: &mut impl Write, text: &str) -> Result<()> {
    output
        .write_all(text.as_bytes())
        .context("hook stdout write")?;
    output.flush().context("hook stdout flush")
}

fn execute_hook(
    rpc: &mut impl Rpc,
    payload: &Value,
    client: &str,
    ledger: &mut impl LedgerSink,
    output: &mut impl Write,
) -> Result<()> {
    execute_hook_with_measurement(
        rpc,
        payload,
        client,
        MeasurementContext::from_environment(payload.get("session_id").and_then(Value::as_str)),
        ledger,
        output,
    )
}

fn execute_hook_with_measurement(
    rpc: &mut impl Rpc,
    payload: &Value,
    client: &str,
    mut measurement: MeasurementContext,
    ledger: &mut impl LedgerSink,
    output: &mut impl Write,
) -> Result<()> {
    if !is_user_prompt_submit(payload) {
        return Ok(());
    }
    let query = filtered_query(payload);
    let attempt = retrieve_attempt(rpc, query.as_deref().ok(), client);
    measurement.arm = attempt.arm;
    let surface = ClientSurface::from_hint(client);
    let context = event_context(payload, surface, attempt.workspace_id.as_deref());
    let start_lookup_failed = if attempt.enabled
        && surface == ClientSurface::ClaudeCode
        && context.workspace_id.is_some()
    {
        match ledger.session_start(context, measurement) {
            Ok(resolved) => {
                measurement = resolved;
                false
            }
            Err(_) => {
                measurement = measurement.with_session_start(None);
                eprintln!("kb-app auto retrieval: session_start_lookup_failed");
                true
            }
        }
    } else {
        measurement = measurement.with_session_start(None);
        false
    };
    let mut retrieved = match attempt.result {
        Ok(Some(retrieved)) => retrieved,
        Ok(None) => {
            if let Some(notice) = attempt.connection_notice {
                // 拒否理由の通知だけで検索・計測は始めない。明示OFFには通知code自体がない。
                return write_output(output, &notice.hook_line());
            }
            // フィルター済みUPSはVaultへ触れず、明示ONのときだけ帰属先不明で数える。
            if attempt.enabled
                && let Err(reason) = query
                && append_observation(
                    ledger,
                    context,
                    measurement,
                    HookObservation::Filtered {
                        reason,
                        timings: attempt.timings,
                    },
                )
                .is_err()
            {
                write_output(output, LEDGER_FAILURE)?;
            }
            return Ok(());
        }
        Err(_) => {
            eprintln!("kb-app auto retrieval: {:?}", attempt.error_stage);
            let ledger_failed = attempt.enabled
                && append_observation(
                    ledger,
                    context,
                    measurement,
                    HookObservation::Error {
                        stage: attempt.error_stage,
                        timings: attempt.timings,
                    },
                )
                .is_err();
            let failure_notice = attempt.failure_notice.unwrap_or(RETRIEVAL_FAILURE);
            let notice = if ledger_failed {
                format!("{failure_notice}{LEDGER_FAILURE}")
            } else {
                failure_notice.to_string()
            };
            return write_output(output, &notice);
        }
    };

    if start_lookup_failed {
        let warning = json!({"code":"session_ledger", "detail":"開始計測を取得できなかった。会話開始は未確認。"});
        if let Some(warnings) = retrieved.structured["degraded"].as_array_mut() {
            warnings.insert(0, warning);
        } else {
            retrieved.structured["degraded"] = json!([warning]);
        }
        retrieved.delivery =
            render_hook_delivery(&retrieved.structured, surface.hook_output_budget())?;
    }
    let mut harvest_emission = None;
    let mut prepared_status = false;
    let mut prepared_cadence = false;
    if attempt.enabled && retrieved.structured["harvest_status_line"] == true {
        let text = match ledger.harvest(context, &retrieved.structured["cadence_digest"]) {
            Ok(prepared) => {
                harvest_emission = prepared.emission;
                prepared.text
            }
            Err(_) => "KB記録: 未確認（状態行の集計または出力履歴を取得できない）。\n".into(),
        };
        prepared_status = true;
        prepared_cadence = text.lines().any(|line| line.starts_with("KB手入れ:"));
        retrieved.structured["harvest_text"] = json!(text);
        retrieved.delivery =
            render_hook_delivery(&retrieved.structured, surface.hook_output_budget())?;
    }
    let mut receipt = None;
    if attempt.enabled {
        // 本文中の同名文字列は数えず、自前の状態行と最終rendererの採否だけを記録する。
        measurement.final_output = Some(FinalOutputFlags {
            status_line_present: prepared_status && retrieved.delivery.harvest_emitted,
            cadence_line_present: prepared_cadence && retrieved.delivery.harvest_emitted,
            status_omitted_for_budget: prepared_status && !retrieved.delivery.harvest_emitted,
        });
        match append_observation(
            ledger,
            context,
            measurement,
            HookObservation::OutputPrepared {
                stats: retrieved.delivery.stats.clone(),
                timings: attempt.timings,
                cap_assumption: CapAssumption::for_surface(surface),
            },
        ) {
            Ok(appended) => receipt = appended.receipt,
            Err(_) => {
                // 台帳の原因にはパス等が入り得るため、固定code/detailだけをモデルへ渡す。
                let warning = json!({
                    "code": "session_ledger",
                    "detail": "計測台帳へ記録できなかった。取得・出力の計測は未確認。"
                });
                if let Some(warnings) = retrieved.structured["degraded"].as_array_mut() {
                    warnings.insert(0, warning);
                } else {
                    retrieved.structured["degraded"] = json!([warning]);
                }
                // stdoutへ追記せず全体を再計測する。失敗しても取得済み本文は保持する。
                match render_hook_delivery(&retrieved.structured, surface.hook_output_budget()) {
                    Ok(delivery) => retrieved.delivery = delivery,
                    Err(_) => {
                        eprintln!("kb-app auto retrieval: session_ledger_notice_render_failed")
                    }
                }
            }
        }
    }

    let emitted = write_output(output, &retrieved.delivery.text);
    if emitted.is_ok()
        && retrieved.delivery.harvest_emitted
        && let Some(emission) = harvest_emission
        && ledger.harvest_emitted(&emission).is_err()
    {
        // 出力済みstdoutへ追記しない。次回もdigestを出し、未出力を出力済みにしない。
        eprintln!("kb-app auto retrieval: harvest_emission_record_failed");
    }
    if let Some(receipt) = receipt {
        let outcome = if emitted.is_ok() {
            HookEmissionOutcome::Emitted
        } else {
            HookEmissionOutcome::StdoutFailed
        };
        if ledger.finalize(&receipt, outcome).is_err() {
            // stdoutは既に確定している。追記せずPreparedのまま残し、受信成功と数えない。
            eprintln!("kb-app auto retrieval: session_ledger_finalize_failed");
        }
    }
    emitted
}

fn tool_is_authoritatively_disabled(response: &Value) -> bool {
    response
        .pointer("/result/structuredContent/code")
        .and_then(Value::as_str)
        == Some(KB_DISABLED_CODE)
        && response
            .pointer("/result/structuredContent/authoritative")
            .and_then(Value::as_bool)
            == Some(true)
        && response
            .pointer("/result/structuredContent/retryable")
            .and_then(Value::as_bool)
            == Some(false)
}

fn binding_failure_notice(response: &Value) -> Option<&'static str> {
    // 診断詳細や返却本文は取り込まず、coreの構造化終端応答だけを固定案内にする。
    if response.pointer("/result/isError").and_then(Value::as_bool) != Some(true)
        || response
            .pointer("/result/structuredContent/authoritative")
            .and_then(Value::as_bool)
            != Some(true)
        || response
            .pointer("/result/structuredContent/retryable")
            .and_then(Value::as_bool)
            != Some(false)
    {
        return None;
    }
    match response
        .pointer("/result/structuredContent/code")
        .and_then(Value::as_str)
    {
        Some("workspace_unverified") => Some(WORKSPACE_UNVERIFIED),
        Some("vault_mismatch") => Some(VAULT_MISMATCH),
        _ => None,
    }
}

fn ensure_tool_succeeded(response: &Value, tool: &str) -> Result<()> {
    if response.pointer("/result/isError").and_then(Value::as_bool) == Some(true) {
        anyhow::bail!("{tool} returned isError");
    }
    if let Some(error) = response.get("error") {
        anyhow::bail!("{tool} RPC error: {error}");
    }
    Ok(())
}

struct ChildMcp {
    child: Child,
    input: ChildStdin,
    output: std::sync::mpsc::Receiver<Result<String>>,
    deadline: Instant,
    next_id: u64,
}

impl ChildMcp {
    fn start(exe: &Path, client: &str, vault: Option<&str>) -> Result<Self> {
        Self::start_with_stderr(exe, client, vault, Stdio::inherit())
    }

    fn start_with_stderr(
        exe: &Path,
        client: &str,
        vault: Option<&str>,
        stderr: Stdio,
    ) -> Result<Self> {
        let mut command = Command::new(exe);
        command
            // 自動retrievalは発話ごとに短命processを起動する。通常MCPと同じ
            // message-time pullを行うと、GitHub credentialのKeychain確認まで
            // 発話回数に比例して発生する。同期はGUI/常設MCPへ任せ、ここでは
            // 手元の正本だけをMCP経由で読む。
            .args(child_mcp_args(client))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr);
        if let Some(vault) = vault {
            command.args(["--vault", vault]);
        }
        let mut child = command.spawn().context("start kb-app MCP")?;
        let input = child.stdin.take().context("MCP stdin unavailable")?;
        let mut reader = BufReader::new(child.stdout.take().context("MCP stdout unavailable")?);
        let (sender, output) = std::sync::mpsc::channel();
        // blocking readを親から切り離す。hostの30秒killより前に子を回収して劣化を返す。
        std::thread::spawn(move || {
            loop {
                let mut line = String::new();
                let result = reader
                    .read_line(&mut line)
                    .map(|_| line)
                    .map_err(anyhow::Error::from);
                let terminal = result.as_ref().map_or(true, |line| line.is_empty());
                if sender.send(result).is_err() || terminal {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            input,
            output,
            next_id: 1,
            deadline: Instant::now() + std::time::Duration::from_secs(20),
        })
    }
}

impl Rpc for ChildMcp {
    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        writeln!(
            self.input,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )?;
        self.input.flush()?;

        let line = receive_before(&self.output, self.deadline)?;
        if line.is_empty() {
            anyhow::bail!("MCP closed before responding to {method}");
        }
        let response: Value = serde_json::from_str(&line).context("MCP response parse")?;
        if response.get("id").and_then(Value::as_u64) != Some(id) {
            anyhow::bail!("MCP response id mismatch");
        }
        Ok(response)
    }
}

fn receive_before(
    output: &std::sync::mpsc::Receiver<Result<String>>,
    deadline: Instant,
) -> Result<String> {
    output
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .context("MCP response deadline exceeded")?
}

impl Drop for ChildMcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-09-05: 検索失敗・接続先拒否の通知もhostのJSON解析へ入らない先頭にする。
    #[test]
    fn failure_notices_are_plain_text_and_within_host_budgets() {
        for surface in [ClientSurface::CodexCli, ClientSurface::ClaudeCode] {
            for notice in [RETRIEVAL_FAILURE, WORKSPACE_UNVERIFIED, VAULT_MISMATCH] {
                assert!(notice.starts_with("# [kb-app 自動retrieval — 劣化]"));
                let with_ledger = format!("{notice}{LEDGER_FAILURE}");
                let budget = surface.hook_output_budget();
                assert!(budget.measure(&with_ledger) <= budget.limit);
            }
        }
    }

    #[derive(Default)]
    struct FakeRpc {
        responses: VecDeque<Value>,
        calls: Vec<(String, Value)>,
    }

    impl Rpc for FakeRpc {
        fn request(&mut self, method: &str, params: Value) -> Result<Value> {
            self.calls.push((method.to_string(), params));
            self.responses
                .pop_front()
                .context("fake response unavailable")
        }
    }

    const WORKSPACE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

    #[derive(Debug, Eq, PartialEq)]
    struct TestSessionStartRecord {
        workspace_id: String,
        session_id: Option<String>,
        source: Option<String>,
        observed_at_ms: i64,
    }

    #[derive(Default)]
    struct TestSessionStartSink {
        records: Vec<TestSessionStartRecord>,
        fail: bool,
    }

    impl SessionStartSink for TestSessionStartSink {
        fn record(
            &mut self,
            workspace_id: &str,
            session_id: Option<&str>,
            source: Option<&str>,
            observed_at_ms: i64,
        ) -> Result<()> {
            self.records.push(TestSessionStartRecord {
                workspace_id: workspace_id.to_string(),
                session_id: session_id.map(str::to_string),
                source: source.map(str::to_string),
                observed_at_ms,
            });
            if self.fail {
                anyhow::bail!("PRIVATE_START_ERROR");
            }
            Ok(())
        }
    }

    fn session_start_payload(source: Option<&str>) -> Value {
        json!({
            "hook_event_name": "SessionStart",
            "source": source,
            "session_id": "PRIVATE_SESSION_MARKER",
            "transcript_path": "/never/read/PRIVATE_TRANSCRIPT_MARKER.jsonl",
            "cwd": "/never/read/PRIVATE_CWD_MARKER",
            "prompt": "PRIVATE_PROMPT_MARKER"
        })
    }

    fn session_start_initialized() -> Value {
        json!({"result": {"capabilities": {"experimental": {"kbApp": {
            "kb_enabled": true,
            "session_observation_binding": {"verified": true, "workspace_id": WORKSPACE}
        }}}}})
    }

    /// 2026-09-06: 再開・clearをstartupだけの登録で取り逃すと、古いMCP IDが有効に残る。
    #[test]
    fn session_start_passes_all_sources_to_core_after_initialize_only() {
        for source in [
            Some("startup"),
            Some("resume"),
            Some("clear"),
            Some("compact"),
            Some("fork"),
            Some("future-source"),
            None,
        ] {
            let mut rpc = FakeRpc {
                responses: VecDeque::from([session_start_initialized()]),
                ..Default::default()
            };
            let mut sink = TestSessionStartSink::default();
            let result = execute_session_start(
                &mut rpc,
                &session_start_payload(source),
                "claude-code/claude",
                123,
                &mut sink,
            );
            let mut errors = Vec::new();
            report_session_start_result(result, &mut errors);
            assert!(errors.is_empty());
            assert_eq!(
                sink.records,
                [TestSessionStartRecord {
                    workspace_id: WORKSPACE.into(),
                    session_id: Some("PRIVATE_SESSION_MARKER".into()),
                    source: source.map(str::to_string),
                    observed_at_ms: 123,
                }]
            );
            assert_eq!(rpc.calls.len(), 1);
            assert_eq!(rpc.calls[0].0, "initialize");
            assert_eq!(rpc.calls[0].1["kb_app_session_observation"], true);
            assert!(
                !serde_json::to_string(&rpc.calls)
                    .unwrap()
                    .contains("PRIVATE_")
            );
        }
    }

    #[test]
    fn session_start_requires_enabled_and_verified_workspace_before_storage() {
        let accepted = session_start_initialized();
        let mut fixtures = vec![json!({}), initialized(Some(false)), initialized(None)];
        for enabled in [json!(false), json!(null), json!("true"), json!(1)] {
            let mut response = accepted.clone();
            response["result"]["capabilities"]["experimental"]["kbApp"]["kb_enabled"] = enabled;
            fixtures.push(response);
        }
        for binding in [
            json!(null),
            json!({"verified": false, "workspace_id": WORKSPACE}),
            json!({"verified": "true", "workspace_id": WORKSPACE}),
            json!({"verified": true}),
            json!({"verified": true, "workspace_id": ""}),
            json!({"verified": true, "workspace_id": 123}),
            json!({"code": "workspace_unverified"}),
            json!({"code": "vault_mismatch"}),
        ] {
            let mut response = accepted.clone();
            response["result"]["capabilities"]["experimental"]["kbApp"]["session_observation_binding"] =
                binding;
            fixtures.push(response);
        }
        for response in fixtures {
            let mut rpc = FakeRpc {
                responses: VecDeque::from([response]),
                ..Default::default()
            };
            let mut sink = TestSessionStartSink::default();
            execute_session_start(
                &mut rpc,
                &session_start_payload(Some("startup")),
                "claude-code/claude",
                123,
                &mut sink,
            )
            .unwrap();
            assert!(sink.records.is_empty());
            assert_eq!(rpc.calls.len(), 1);
        }
    }

    #[test]
    fn session_start_and_retrieval_modes_do_not_accept_each_others_events() {
        for client in [
            "codex-cli/gpt",
            "claude-desktop/claude",
            "future-client/claude",
            "mcp-client/unknown",
        ] {
            let mut rpc = FakeRpc::default();
            let mut sink = TestSessionStartSink::default();
            execute_session_start(
                &mut rpc,
                &session_start_payload(Some("startup")),
                client,
                123,
                &mut sink,
            )
            .unwrap();
            assert!(rpc.calls.is_empty());
            assert!(sink.records.is_empty());
        }
        for event in ["UserPromptSubmit", "Stop", "SessionEnd", "", "sessionStart"] {
            let mut payload = session_start_payload(Some("startup"));
            payload["hook_event_name"] = json!(event);
            let mut rpc = FakeRpc::default();
            let mut sink = TestSessionStartSink::default();
            execute_session_start(&mut rpc, &payload, "claude-code/claude", 123, &mut sink)
                .unwrap();
            assert!(rpc.calls.is_empty());
            assert!(sink.records.is_empty());
        }
        let mut rpc = FakeRpc::default();
        let mut ledger = TestLedger::new();
        let mut output = Vec::new();
        execute_hook(
            &mut rpc,
            &session_start_payload(Some("startup")),
            "claude-code/claude",
            &mut ledger,
            &mut output,
        )
        .unwrap();
        assert!(rpc.calls.is_empty());
        assert!(ledger.trace.borrow().is_empty());
        assert!(output.is_empty());
        assert!(!ledger.path.exists());
    }

    #[test]
    fn session_start_failures_report_only_a_fixed_warning() {
        for responses in [
            VecDeque::new(),
            VecDeque::from([json!({"error": {"message": "PRIVATE_RPC_ERROR"}})]),
            VecDeque::from([session_start_initialized()]),
        ] {
            let mut rpc = FakeRpc {
                responses,
                ..Default::default()
            };
            let mut sink = TestSessionStartSink {
                fail: true,
                ..Default::default()
            };
            let result = execute_session_start(
                &mut rpc,
                &session_start_payload(Some("startup")),
                "claude-code/claude",
                123,
                &mut sink,
            );
            assert!(result.is_err());
            let mut errors = Vec::new();
            report_session_start_result(result, &mut errors);
            assert_eq!(String::from_utf8(errors).unwrap(), SESSION_START_FAILURE);
        }
    }

    struct TestLedger {
        _directory: tempfile::TempDir,
        path: std::path::PathBuf,
        events: Vec<Value>,
        finalized: Vec<HookEmissionOutcome>,
        start_lookups: Vec<(ClientSurface, Option<String>, Option<String>)>,
        start_evidence: Option<Option<session_ledger::SessionStartEvidence>>,
        fail_start_lookup: bool,
        fail_append: bool,
        fail_finalize: bool,
        harvest_text: String,
        trace: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
    }

    impl TestLedger {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            Self {
                path: directory.path().join("ledger.sqlite3"),
                _directory: directory,
                events: Vec::new(),
                finalized: Vec::new(),
                start_lookups: Vec::new(),
                start_evidence: None,
                fail_start_lookup: false,
                fail_append: false,
                fail_finalize: false,
                harvest_text: "KB記録: fixture。\n".into(),
                trace: Default::default(),
            }
        }
    }

    impl LedgerSink for TestLedger {
        fn session_start(
            &mut self,
            context: EventContext<'_>,
            measurement: MeasurementContext,
        ) -> Result<MeasurementContext> {
            self.start_lookups.push((
                context.surface,
                context.workspace_id.map(str::to_string),
                context.session_id.map(str::to_string),
            ));
            if self.fail_start_lookup {
                anyhow::bail!("PRIVATE_START_LOOKUP_ERROR");
            }
            Ok(match self.start_evidence {
                Some(evidence) => measurement.with_session_start(evidence),
                None => measurement,
            })
        }

        fn harvest(
            &mut self,
            _: EventContext<'_>,
            _: &Value,
        ) -> Result<kb_core::harvest::PreparedStatus> {
            Ok(kb_core::harvest::PreparedStatus {
                text: self.harvest_text.clone(),
                emission: None,
            })
        }
        fn harvest_emitted(&mut self, _: &kb_core::harvest::Emission) -> Result<()> {
            Ok(())
        }

        fn append(&mut self, event: &LedgerEvent) -> Result<AppendOutcome> {
            self.trace.borrow_mut().push("append");
            self.events.push(serde_json::to_value(event)?);
            if self.fail_append {
                anyhow::bail!("sensitive ledger error never enters stdout");
            }
            session_ledger::append_at(&self.path, event)
        }

        fn finalize(&mut self, receipt: &HookReceipt, outcome: HookEmissionOutcome) -> Result<()> {
            self.trace.borrow_mut().push("finalize");
            self.finalized.push(outcome);
            if self.fail_finalize {
                anyhow::bail!("sensitive finalization error never enters stdout");
            }
            session_ledger::finalize_hook_at(&self.path, receipt, outcome)
        }
    }

    struct TestOutput {
        bytes: Vec<u8>,
        fail_write: bool,
        fail_flush: bool,
        trace: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
    }

    impl TestOutput {
        fn new(ledger: &TestLedger) -> Self {
            Self {
                bytes: Vec::new(),
                fail_write: false,
                fail_flush: false,
                trace: ledger.trace.clone(),
            }
        }
    }

    impl Write for TestOutput {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.trace.borrow_mut().push("write");
            if self.fail_write {
                return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.trace.borrow_mut().push("flush");
            if self.fail_flush {
                return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
            }
            Ok(())
        }
    }

    fn initialized(enabled: Option<bool>) -> Value {
        let mut response = json!({"result": {"capabilities": {"tools": {}}}});
        if let Some(enabled) = enabled {
            response["result"]["capabilities"]["experimental"] =
                json!({"kbApp": {"kb_enabled": enabled}});
        }
        response
    }

    fn search_response() -> Value {
        json!({"result": {"structuredContent": {
            "workspace_id": WORKSPACE,
            "hits": [{"id": "notes/fixture"}],
            "documents": [{"id": "notes/fixture", "text": "本文PRIVATE_BODY_MARKER"}],
            "degraded": []
        }}})
    }

    fn user_payload() -> Value {
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "PRIVATE_SESSION_MARKER",
            "turn_id": "PRIVATE_TURN_MARKER",
            "prompt_id": "PRIVATE_PROMPT_ID_MARKER",
            "permission_mode": "acceptEdits",
            "prompt": "過去の配備手順を確認 PRIVATE_PROMPT_MARKER",
            "transcript_path": "/never/read/PRIVATE_TRANSCRIPT_MARKER.jsonl",
            "cwd": "/never/read/PRIVATE_CWD_MARKER"
        })
    }

    fn enabled_search() -> FakeRpc {
        FakeRpc {
            responses: VecDeque::from([initialized(Some(true)), search_response()]),
            ..Default::default()
        }
    }

    /// 2026-09-06: 開始証拠の照合もOFF・接続未確認のときは保存先を調べない。
    #[test]
    fn ups_start_lookup_requires_enabled_claude_and_verified_workspace() {
        let mut unbound = search_response();
        unbound["result"]["structuredContent"]
            .as_object_mut()
            .unwrap()
            .remove("workspace_id");
        let mut explicit_off = initialized(Some(false));
        explicit_off["result"]["capabilities"]["experimental"]["kbApp"]["session_observation_binding"] =
            json!({"verified": true, "workspace_id": WORKSPACE});
        for (client, initialize, search, filtered, expected_lookups) in [
            (
                "claude-code/claude",
                initialized(Some(true)),
                search_response(),
                false,
                1,
            ),
            (
                "claude-code/claude",
                initialized(Some(true)),
                unbound,
                false,
                0,
            ),
            (
                "claude-code/claude",
                explicit_off,
                search_response(),
                false,
                0,
            ),
            (
                "claude-code/claude",
                initialized(None),
                search_response(),
                false,
                0,
            ),
            (
                "claude-code/claude",
                initialized(Some(true)),
                binding_failure("workspace_unverified"),
                false,
                0,
            ),
            (
                "claude-code/claude",
                initialized(Some(true)),
                binding_failure("vault_mismatch"),
                false,
                0,
            ),
            (
                "claude-code/claude",
                initialized(Some(true)),
                binding_failure("kb_disabled"),
                false,
                0,
            ),
            (
                "claude-code/claude",
                initialized(Some(true)),
                search_response(),
                true,
                0,
            ),
            (
                "codex-cli/gpt",
                initialized(Some(true)),
                search_response(),
                false,
                0,
            ),
            (
                "claude-desktop/claude",
                initialized(Some(true)),
                search_response(),
                false,
                0,
            ),
            (
                "future-client/claude",
                initialized(Some(true)),
                search_response(),
                false,
                0,
            ),
        ] {
            let mut rpc = FakeRpc {
                responses: VecDeque::from([initialize, search]),
                ..Default::default()
            };
            let mut payload = user_payload();
            if filtered {
                payload["prompt"] = json!("ok");
            }
            let mut ledger = TestLedger::new();
            let measurement = MeasurementContext {
                purpose: session_ledger::MeasurementPurpose::Normal,
                session_started_at_ms: Some(1),
                session_start_source: Some(session_ledger::SessionStartSource::Launcher),
                ..Default::default()
            };
            execute_hook_with_measurement(
                &mut rpc,
                &payload,
                client,
                measurement,
                &mut ledger,
                &mut Vec::new(),
            )
            .unwrap();
            assert_eq!(ledger.start_lookups.len(), expected_lookups, "{client}");
            if expected_lookups == 1 {
                assert_eq!(
                    ledger.start_lookups[0],
                    (
                        ClientSurface::ClaudeCode,
                        Some(WORKSPACE.into()),
                        Some("PRIVATE_SESSION_MARKER".into())
                    )
                );
            } else {
                for event in &ledger.events {
                    assert!(event["measurement"]["session_started_at_ms"].is_null());
                    assert!(event["measurement"]["session_start_source"].is_null());
                    assert!(event["measurement"]["session_start_generation"].is_null());
                }
            }
        }
    }

    #[test]
    fn ups_uses_current_start_evidence_and_does_not_restore_missing_evidence_from_launcher() {
        for evidence in [
            Some(session_ledger::SessionStartEvidence {
                observed_at_ms: 123,
                generation: 7,
            }),
            None,
        ] {
            let mut rpc = enabled_search();
            let mut ledger = TestLedger::new();
            ledger.start_evidence = Some(evidence);
            let measurement = MeasurementContext {
                purpose: session_ledger::MeasurementPurpose::Normal,
                session_started_at_ms: Some(1),
                session_start_source: Some(session_ledger::SessionStartSource::Launcher),
                ..Default::default()
            };
            let mut output = Vec::new();
            execute_hook_with_measurement(
                &mut rpc,
                &user_payload(),
                "claude-code/claude",
                measurement,
                &mut ledger,
                &mut output,
            )
            .unwrap();
            let measured = &ledger.events[0]["measurement"];
            assert_eq!(
                measured["session_started_at_ms"],
                json!(evidence.map(|value| value.observed_at_ms))
            );
            assert_eq!(
                measured["session_start_generation"],
                json!(evidence.map(|value| value.generation))
            );
            assert_eq!(
                measured["session_start_source"],
                json!(evidence.map(|_| "host_start_event"))
            );
            assert_eq!(measured["launcher_started_at_ms"], 1);
            assert_eq!(ledger.start_lookups.len(), 1);
            assert!(
                !String::from_utf8(output)
                    .unwrap()
                    .contains("session_ledger")
            );
        }
    }

    /// 2026-09-06: 開始DBが壊れても検索本文を失わず、失敗通知込みの最終予算を計測する。
    #[test]
    fn ups_start_lookup_failure_clears_start_and_rerenders_degradation_within_budget() {
        for long_body in [false, true] {
            let mut search = search_response();
            search["result"]["structuredContent"]["harvest_status_line"] = json!(true);
            search["result"]["structuredContent"]["documents"][0]["text"] = if long_body {
                json!(format!("RETRIEVED_BODY{}", "本文😀".repeat(8_000)))
            } else {
                json!("RETRIEVED_BODY")
            };
            let mut rpc = FakeRpc {
                responses: VecDeque::from([initialized(Some(true)), search]),
                ..Default::default()
            };
            let mut ledger = TestLedger::new();
            ledger.fail_start_lookup = true;
            let measurement = MeasurementContext {
                purpose: session_ledger::MeasurementPurpose::Normal,
                session_started_at_ms: Some(1),
                session_start_source: Some(session_ledger::SessionStartSource::Launcher),
                ..Default::default()
            };
            let mut output = Vec::new();
            execute_hook_with_measurement(
                &mut rpc,
                &user_payload(),
                "claude-code/claude",
                measurement,
                &mut ledger,
                &mut output,
            )
            .unwrap();
            let text = String::from_utf8(output).unwrap();
            let measured = &ledger.events[0]["measurement"];
            assert!(measured["session_started_at_ms"].is_null());
            assert!(measured["session_start_source"].is_null());
            assert!(measured["session_start_generation"].is_null());
            assert_eq!(measured["launcher_started_at_ms"], 1);
            assert!(text.contains("session_ledger"));
            assert!(!text.contains("PRIVATE_START_LOOKUP_ERROR"));
            assert!(!text.contains(RETRIEVAL_FAILURE));
            if !long_body {
                assert!(text.contains("RETRIEVED_BODY"));
            }
            let budget = ClientSurface::ClaudeCode.hook_output_budget();
            assert!(budget.measure(&text) <= budget.limit);
            assert_eq!(
                observation(&ledger.events[0])["stats"]["emitted_bytes"],
                text.len()
            );
            assert_eq!(ledger.finalized, [HookEmissionOutcome::Emitted]);
            assert_eq!(ledger.start_lookups.len(), 1);
            assert_eq!(rpc.calls.len(), 2);
        }
    }

    /// 2026-09-05: 古いguardの拒否は固定文だけを返し、通知のために検索・台帳を開かない。
    #[test]
    fn guard_notices_preserve_disabled_boundaries_at_both_protocol_stages() {
        for client in ["codex-cli/gpt", "claude-code/claude"] {
            for at_initialize in [true, false] {
                let notice = json!([{"code":"guard_outdated", "message":"PRIVATE_DIAGNOSTIC"}]);
                let mut initialize = initialized(Some(!at_initialize));
                let mut responses = VecDeque::new();
                if at_initialize {
                    initialize["result"]["capabilities"]["experimental"]["kbApp"]["client_notices"] =
                        notice;
                    responses.push_back(initialize);
                } else {
                    responses.push_back(initialize);
                    responses.push_back(json!({"result":{"isError":true,"structuredContent":{
                        "code":"kb_disabled","authoritative":true,"retryable":false,
                        "client_notices":notice,
                        "documents":[{"id":"notes/private", "text":"PRIVATE_BODY"}]
                    }}}));
                }
                let mut rpc = FakeRpc {
                    responses,
                    ..Default::default()
                };
                let mut ledger = TestLedger::new();
                let mut output = Vec::new();
                execute_hook(&mut rpc, &user_payload(), client, &mut ledger, &mut output).unwrap();
                let text = String::from_utf8(output).unwrap();
                assert_eq!(text, ClientNotice::GuardOutdated.hook_line());
                assert!(text.starts_with("# "));
                assert!(!text.contains("PRIVATE_"));
                let budget = ClientSurface::from_hint(client).hook_output_budget();
                assert!(budget.measure(&text) <= budget.limit);
                assert_eq!(rpc.calls.len(), if at_initialize { 1 } else { 2 });
                assert!(ledger.trace.borrow().is_empty());
                assert!(!ledger.path.exists());
            }
        }
    }

    /// 2026-09-05: 明示OFF・未知通知・別surfaceから、guard例外を広げない。
    #[test]
    fn disabled_hook_ignores_unrecognized_and_inapplicable_notices() {
        for (client, notices) in [
            ("codex-cli/gpt", json!(null)),
            (
                "codex-cli/gpt",
                json!([{"code":"host_capability_unverified"}]),
            ),
            ("codex-cli/gpt", json!([{"code":"guard_outdated_fake"}])),
            ("codex-cli/gpt", json!({"code":"guard_outdated"})),
            ("codex-cli/gpt", json!(["guard_outdated", {"code":42}])),
            ("chatgpt/openai", json!([{"code":"guard_outdated"}])),
            ("future-client/gpt", json!([{"code":"guard_outdated"}])),
        ] {
            let mut initialize = initialized(Some(false));
            initialize["result"]["capabilities"]["experimental"]["kbApp"]["client_notices"] =
                notices;
            let mut rpc = FakeRpc {
                responses: VecDeque::from([initialize]),
                ..Default::default()
            };
            let mut ledger = TestLedger::new();
            let mut output = Vec::new();
            execute_hook(&mut rpc, &user_payload(), client, &mut ledger, &mut output).unwrap();
            assert!(output.is_empty());
            assert_eq!(rpc.calls.len(), 1);
            assert!(ledger.trace.borrow().is_empty());
            assert!(!ledger.path.exists());
        }
    }

    /// 2026-09-05: host通知も再整形・台帳の実測へ含め、自由文や検証済みの主張を採用しない。
    #[test]
    fn host_notice_is_measured_after_harvest_and_ledger_failure_rerenders() {
        for client in ["codex-cli/gpt", "claude-code/claude"] {
            for fail_append in [false, true] {
                let mut initialize = initialized(Some(true));
                initialize["result"]["capabilities"]["experimental"]["kbApp"]["client_notices"] = json!([{"code":"host_capability_unverified", "message":"PRIVATE_HOST_VERIFIED"}]);
                let mut search = search_response();
                search["result"]["structuredContent"]["harvest_status_line"] = json!(true);
                search["result"]["structuredContent"]["documents"][0]["text"] =
                    json!("本文😀".repeat(3000));
                let mut rpc = FakeRpc {
                    responses: VecDeque::from([initialize, search]),
                    ..Default::default()
                };
                let mut ledger = TestLedger::new();
                ledger.fail_append = fail_append;
                let mut output = Vec::new();
                execute_hook(&mut rpc, &user_payload(), client, &mut ledger, &mut output).unwrap();
                let text = String::from_utf8(output).unwrap();
                assert_eq!(text.matches("host_capability_unverified").count(), 1);
                assert!(!text.contains("PRIVATE_HOST"));
                assert!(text.starts_with("# "));
                let budget = ClientSurface::from_hint(client).hook_output_budget();
                assert!(budget.measure(&text) <= budget.limit);
                if !fail_append {
                    assert_eq!(
                        observation(&ledger.events[0])["stats"]["emitted_bytes"],
                        text.len()
                    );
                    assert_eq!(ledger.finalized, [HookEmissionOutcome::Emitted]);
                } else {
                    assert!(text.contains("session_ledger"));
                }
            }
        }
    }

    /// 2026-09-05: 通知付加前に応答の型を検査し、壊れた応答をpanicへ変えない。
    #[test]
    fn host_notice_does_not_panic_on_non_object_search_content() {
        for structured in [json!([]), json!("PRIVATE_RESPONSE"), json!(42), Value::Null] {
            let mut initialize = initialized(Some(true));
            initialize["result"]["capabilities"]["experimental"]["kbApp"]["client_notices"] =
                json!([ClientNotice::HostCapabilityUnverified.value()]);
            let mut rpc = FakeRpc {
                responses: VecDeque::from([
                    initialize,
                    json!({"result":{"structuredContent":structured}}),
                ]),
                ..Default::default()
            };
            let mut ledger = TestLedger::new();
            let mut output = Vec::new();
            execute_hook(
                &mut rpc,
                &user_payload(),
                "codex-cli/gpt",
                &mut ledger,
                &mut output,
            )
            .unwrap();
            assert_eq!(String::from_utf8(output).unwrap(), RETRIEVAL_FAILURE);
            assert_eq!(observation(&ledger.events[0])["kind"], "error");
            assert!(ledger.finalized.is_empty());
        }
    }

    fn observation(event: &Value) -> &Value {
        &event["observation"]["observation"]
    }

    fn binding_failure(code: &str) -> Value {
        json!({"result": {
            "isError": true,
            "content": [{"type": "text", "text": "PRIVATE_DIAGNOSTIC_MARKER"}],
            "structuredContent": {
                "code": code,
                "authoritative": true,
                "retryable": false,
                "workspace_id": WORKSPACE,
                "documents": [{"id": "notes/fixture", "text": "PRIVATE_BODY_MARKER"}]
            }
        }})
    }

    /// 2026-09-05: 接続先の未確認・不一致を該当なしへ潰さず、本文や診断を配信しない。
    #[test]
    fn binding_failures_emit_only_fixed_notices_and_never_prepare_context() {
        for (code, notice) in [
            ("workspace_unverified", WORKSPACE_UNVERIFIED),
            ("vault_mismatch", VAULT_MISMATCH),
        ] {
            for fail_ledger in [false, true] {
                let mut rpc = FakeRpc {
                    responses: VecDeque::from([initialized(Some(true)), binding_failure(code)]),
                    ..Default::default()
                };
                let mut ledger = TestLedger::new();
                ledger.fail_append = fail_ledger;
                let mut output = Vec::new();
                execute_hook(
                    &mut rpc,
                    &user_payload(),
                    "codex-cli/gpt",
                    &mut ledger,
                    &mut output,
                )
                .unwrap();
                let output = String::from_utf8(output).unwrap();
                assert_eq!(
                    output,
                    format!("{notice}{}", if fail_ledger { LEDGER_FAILURE } else { "" })
                );
                assert!(!output.contains("PRIVATE_"));
                assert_eq!(rpc.calls.len(), 2);
                assert_eq!(ledger.events.len(), 1);
                assert!(ledger.events[0]["workspace_id"].is_null());
                assert_eq!(observation(&ledger.events[0])["kind"], "error");
                assert_eq!(observation(&ledger.events[0])["stage"], "search");
                assert!(observation(&ledger.events[0])["timings"]["render_ms"].is_null());
                assert!(ledger.finalized.is_empty());
                assert!(
                    !serde_json::to_string(&ledger.events)
                        .unwrap()
                        .contains("PRIVATE_")
                );
                for surface in [ClientSurface::ClaudeCode, ClientSurface::CodexCli] {
                    let budget = surface.hook_output_budget();
                    assert!(budget.measure(&output) <= budget.limit);
                }
            }
        }
    }

    #[test]
    fn binding_notices_require_authoritative_terminal_codes_and_off_stays_silent() {
        let valid = binding_failure("workspace_unverified");
        let mut not_authoritative = valid.clone();
        not_authoritative["result"]["structuredContent"]["authoritative"] = json!(false);
        let mut retryable = valid.clone();
        retryable["result"]["structuredContent"]["retryable"] = json!(true);
        for response in [
            not_authoritative,
            retryable,
            binding_failure("PRIVATE_UNKNOWN_CODE"),
        ] {
            let mut rpc = FakeRpc {
                responses: VecDeque::from([initialized(Some(true)), response]),
                ..Default::default()
            };
            let mut output = Vec::new();
            execute_hook(
                &mut rpc,
                &user_payload(),
                "codex-cli/gpt",
                &mut TestLedger::new(),
                &mut output,
            )
            .unwrap();
            assert_eq!(String::from_utf8(output).unwrap(), RETRIEVAL_FAILURE);
        }
        let mut rpc = FakeRpc {
            responses: VecDeque::from([initialized(Some(false)), valid]),
            ..Default::default()
        };
        let mut ledger = TestLedger::new();
        let mut output = Vec::new();
        execute_hook(
            &mut rpc,
            &user_payload(),
            "codex-cli/gpt",
            &mut ledger,
            &mut output,
        )
        .unwrap();
        assert!(output.is_empty());
        assert!(ledger.events.is_empty());
        assert!(!ledger.path.exists());
        assert_eq!(rpc.calls.len(), 1);
    }

    /// 2026-09-05: 計測は出力準備とwrite/flush完了を分け、本文や入力を台帳へ渡さない。
    #[test]
    fn ledger_observes_emission_only_after_write_and_flush() {
        let mut ledger = TestLedger::new();
        let mut output = TestOutput::new(&ledger);
        execute_hook(
            &mut enabled_search(),
            &user_payload(),
            "codex-cli/gpt",
            &mut ledger,
            &mut output,
        )
        .unwrap();
        assert_eq!(
            *ledger.trace.borrow(),
            ["append", "write", "flush", "finalize"]
        );
        assert_eq!(ledger.finalized, [HookEmissionOutcome::Emitted]);
        let event = &ledger.events[0];
        assert_eq!(event["workspace_id"], WORKSPACE);
        assert_eq!(event["permission_mode"], "accept_edits");
        for field in ["session_hash", "turn_hash", "prompt_hash"] {
            assert_eq!(event[field].as_str().unwrap().len(), 64);
        }
        let measured = observation(event);
        assert_eq!(measured["kind"], "output_prepared");
        assert_eq!(measured["stats"]["emitted_bytes"], output.bytes.len());
        for stage in ["initialize_ms", "search_ms", "render_ms"] {
            assert!(measured["timings"][stage].is_u64());
        }
        let saved = serde_json::to_string(&ledger.events).unwrap();
        assert!(!saved.contains("PRIVATE_"));
    }

    /// 2026-09-05: 明示OFF・ON不明と対象外イベントから台帳を開かない。
    #[test]
    fn disabled_unknown_and_unsupported_events_do_not_touch_the_ledger() {
        for enabled in [Some(false), None] {
            let mut ledger = TestLedger::new();
            let mut output = Vec::new();
            let mut rpc = FakeRpc {
                responses: VecDeque::from([initialized(enabled), search_response()]),
                ..Default::default()
            };
            execute_hook(
                &mut rpc,
                &user_payload(),
                "codex-cli/gpt",
                &mut ledger,
                &mut output,
            )
            .unwrap();
            assert!(ledger.events.is_empty());
            assert!(!ledger.path.exists());
            if enabled == Some(false) {
                assert_eq!(rpc.calls.len(), 1);
                assert!(output.is_empty());
            } else {
                assert_eq!(rpc.calls.len(), 2);
                assert!(!output.is_empty());
            }
        }
        let mut ledger = TestLedger::new();
        let mut rpc = FakeRpc::default();
        execute_hook(
            &mut rpc,
            &json!({"hook_event_name":"Stop"}),
            "codex-cli/gpt",
            &mut ledger,
            &mut Vec::new(),
        )
        .unwrap();
        assert!(rpc.calls.is_empty());
        assert!(ledger.events.is_empty());
        assert!(!ledger.path.exists());
    }

    #[test]
    fn enabled_without_tools_is_a_protocol_error_but_legacy_unknown_stays_silent() {
        for enabled in [Some(true), None] {
            let mut initialize = initialized(enabled);
            initialize["result"]["capabilities"]
                .as_object_mut()
                .unwrap()
                .remove("tools");
            let mut rpc = FakeRpc {
                responses: VecDeque::from([initialize]),
                ..Default::default()
            };
            let mut ledger = TestLedger::new();
            let mut output = Vec::new();
            execute_hook(
                &mut rpc,
                &user_payload(),
                "codex-cli/gpt",
                &mut ledger,
                &mut output,
            )
            .unwrap();
            assert_eq!(rpc.calls.len(), 1);
            if enabled == Some(true) {
                assert_eq!(observation(&ledger.events[0])["stage"], "protocol");
                assert!(observation(&ledger.events[0])["timings"]["search_ms"].is_null());
                assert_eq!(String::from_utf8(output).unwrap(), RETRIEVAL_FAILURE);
            } else {
                assert!(ledger.events.is_empty());
                assert!(!ledger.path.exists());
                assert!(output.is_empty());
            }
        }
    }

    #[test]
    fn non_string_workspace_identity_is_not_silently_counted_as_unattributed() {
        for workspace in [json!(42), json!(false), json!([]), json!({})] {
            let mut searched = search_response();
            searched["result"]["structuredContent"]["workspace_id"] = workspace;
            let mut rpc = FakeRpc {
                responses: VecDeque::from([initialized(Some(true)), searched]),
                ..Default::default()
            };
            let mut ledger = TestLedger::new();
            let mut output = Vec::new();
            execute_hook(
                &mut rpc,
                &user_payload(),
                "codex-cli/gpt",
                &mut ledger,
                &mut output,
            )
            .unwrap();
            assert_eq!(observation(&ledger.events[0])["kind"], "error");
            assert_eq!(observation(&ledger.events[0])["stage"], "protocol");
            assert!(ledger.events[0]["workspace_id"].is_null());
            assert!(observation(&ledger.events[0])["timings"]["render_ms"].is_null());
            assert_eq!(String::from_utf8(output).unwrap(), RETRIEVAL_FAILURE);
            assert!(ledger.finalized.is_empty());
        }
    }

    #[test]
    fn authoritative_tool_off_overrides_initialize_before_ledger_access() {
        let mut rpc = FakeRpc {
            responses: VecDeque::from([
                initialized(Some(true)),
                json!({"result":{"isError":true,"structuredContent":{
                    "code":"kb_disabled","authoritative":true,"retryable":false
                }}}),
            ]),
            ..Default::default()
        };
        let mut ledger = TestLedger::new();
        let mut output = Vec::new();
        execute_hook(
            &mut rpc,
            &user_payload(),
            "codex-cli/gpt",
            &mut ledger,
            &mut output,
        )
        .unwrap();
        assert!(ledger.events.is_empty());
        assert!(!ledger.path.exists());
        assert!(output.is_empty());
    }

    #[test]
    fn filtered_prompts_are_unattributed_and_never_search() {
        for (prompt, reason) in [
            (json!("ok"), "short_prompt"),
            (json!("/help"), "slash_command"),
            (json!("[SYSTEM NOTIFICATION event]"), "system_notification"),
            (json!("<task-notification>done"), "task_notification"),
            (Value::Null, "missing_prompt"),
        ] {
            let mut payload = user_payload();
            payload["prompt"] = prompt;
            let mut rpc = FakeRpc {
                responses: VecDeque::from([initialized(Some(true))]),
                ..Default::default()
            };
            let mut ledger = TestLedger::new();
            let mut output = Vec::new();
            execute_hook(
                &mut rpc,
                &payload,
                "claude-code/claude",
                &mut ledger,
                &mut output,
            )
            .unwrap();
            assert_eq!(rpc.calls.len(), 1);
            assert!(output.is_empty());
            assert_eq!(ledger.events.len(), 1);
            assert!(ledger.events[0]["workspace_id"].is_null());
            assert_eq!(observation(&ledger.events[0])["reason"], reason);
            assert!(observation(&ledger.events[0])["timings"]["initialize_ms"].is_u64());
            assert!(observation(&ledger.events[0])["timings"]["search_ms"].is_null());
            assert!(observation(&ledger.events[0])["timings"]["render_ms"].is_null());
            assert!(ledger.finalized.is_empty());
        }
    }

    #[test]
    fn errors_preserve_safe_stage_and_workspace_only_after_discovery() {
        for (response, stage, attributed) in [
            (
                json!({"error":{"message":"PRIVATE_SEARCH_ERROR_MARKER"}}),
                "search",
                false,
            ),
            (
                json!({"result":{"structuredContent":{"workspace_id":WORKSPACE,"hits":"bad"}}}),
                "render",
                true,
            ),
        ] {
            let mut rpc = FakeRpc {
                responses: VecDeque::from([initialized(Some(true)), response]),
                ..Default::default()
            };
            let mut ledger = TestLedger::new();
            let mut output = Vec::new();
            execute_hook(
                &mut rpc,
                &user_payload(),
                "codex-cli/gpt",
                &mut ledger,
                &mut output,
            )
            .unwrap();
            assert_eq!(observation(&ledger.events[0])["kind"], "error");
            assert_eq!(observation(&ledger.events[0])["stage"], stage);
            assert_eq!(ledger.events[0]["workspace_id"].is_string(), attributed);
            assert_eq!(String::from_utf8(output).unwrap(), RETRIEVAL_FAILURE);
            assert!(
                !serde_json::to_string(&ledger.events)
                    .unwrap()
                    .contains("PRIVATE_")
            );
            assert!(ledger.finalized.is_empty());
        }
    }

    #[test]
    fn preparation_failure_rerenders_a_bounded_notice_without_fabricating_emission() {
        for client in ["codex-cli/gpt", "claude-code/claude"] {
            let mut ledger = TestLedger::new();
            ledger.fail_append = true;
            let mut output = Vec::new();
            execute_hook(
                &mut enabled_search(),
                &user_payload(),
                client,
                &mut ledger,
                &mut output,
            )
            .unwrap();
            let output = String::from_utf8(output).unwrap();
            assert!(output.contains("session_ledger"));
            assert!(output.contains("本文PRIVATE_BODY_MARKER"));
            assert!(!output.contains("sensitive ledger error"));
            assert!(ledger.finalized.is_empty());
            let stats: Value = serde_json::from_str(
                output
                    .lines()
                    .find_map(|line| line.strip_prefix("出力統計: "))
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(stats["emitted_bytes"], output.len());
            let budget = ClientSurface::from_hint(client).hook_output_budget();
            assert!(budget.measure(&output) <= budget.limit);
        }
    }

    #[test]
    fn filtered_and_search_error_ledger_failures_have_bounded_safe_notices() {
        for filtered in [true, false] {
            let mut ledger = TestLedger::new();
            ledger.fail_append = true;
            let mut payload = user_payload();
            if filtered {
                payload["prompt"] = json!("ok");
            }
            let mut rpc = FakeRpc {
                responses: VecDeque::from([
                    initialized(Some(true)),
                    json!({"error":{"message":"PRIVATE_ERROR_MARKER"}}),
                ]),
                ..Default::default()
            };
            let mut output = Vec::new();
            execute_hook(
                &mut rpc,
                &payload,
                "codex-cli/gpt",
                &mut ledger,
                &mut output,
            )
            .unwrap();
            let output = String::from_utf8(output).unwrap();
            assert!(output.contains("session_ledger"));
            assert!(!output.contains("PRIVATE_"));
            assert!(!output.contains("sensitive ledger error"));
            let budget = ClientSurface::CodexCli.hook_output_budget();
            assert!(budget.measure(&output) <= budget.limit);
            assert_eq!(rpc.calls.len(), if filtered { 1 } else { 2 });
            assert!(ledger.finalized.is_empty());
        }
    }

    #[test]
    fn write_or_flush_failure_never_finalizes_as_emitted() {
        for fail_write in [true, false] {
            let mut ledger = TestLedger::new();
            let mut output = TestOutput::new(&ledger);
            output.fail_write = fail_write;
            output.fail_flush = !fail_write;
            assert!(
                execute_hook(
                    &mut enabled_search(),
                    &user_payload(),
                    "codex-cli/gpt",
                    &mut ledger,
                    &mut output
                )
                .is_err()
            );
            assert_eq!(ledger.finalized, [HookEmissionOutcome::StdoutFailed]);
            assert_eq!(ledger.events.len(), 1);
        }
    }

    #[test]
    fn finalization_failure_keeps_the_already_emitted_output_unchanged() {
        let mut ledger = TestLedger::new();
        ledger.fail_finalize = true;
        let mut output = Vec::new();
        execute_hook(
            &mut enabled_search(),
            &user_payload(),
            "codex-cli/gpt",
            &mut ledger,
            &mut output,
        )
        .unwrap();
        assert_eq!(ledger.finalized, [HookEmissionOutcome::Emitted]);
        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains("session_ledger"));
        assert_eq!(
            observation(&ledger.events[0])["stats"]["emitted_bytes"],
            output.len()
        );
    }

    #[test]
    fn stable_retries_do_not_finalize_another_observation_and_missing_ids_remain_unique() {
        let mut ledger = TestLedger::new();
        for _ in 0..2 {
            execute_hook(
                &mut enabled_search(),
                &user_payload(),
                "codex-cli/gpt",
                &mut ledger,
                &mut Vec::new(),
            )
            .unwrap();
        }
        assert_eq!(ledger.finalized, [HookEmissionOutcome::Emitted]);
        assert_eq!(ledger.events[0]["event_id"], ledger.events[1]["event_id"]);
        let mut payload = user_payload();
        for field in ["turn_id", "prompt_id"] {
            payload.as_object_mut().unwrap().remove(field);
        }
        for _ in 0..2 {
            execute_hook(
                &mut enabled_search(),
                &payload,
                "claude-code/claude",
                &mut ledger,
                &mut Vec::new(),
            )
            .unwrap();
        }
        assert_ne!(ledger.events[2]["event_id"], ledger.events[3]["event_id"]);
        assert_eq!(
            ledger.events[2]["session_hash"],
            ledger.events[3]["session_hash"]
        );
        assert!(ledger.events[2]["prompt_hash"].is_null());
        assert!(ledger.events[2]["turn_hash"].is_null());
    }

    #[test]
    fn prompt_filter_ignores_non_user_and_control_prompts() {
        assert_eq!(
            query_from_payload(&json!({"hook_event_name": "Stop"})),
            None
        );
        assert_eq!(
            query_from_payload(&json!({
                "hook_event_name": "UserPromptSubmit",
                "prompt": "/help"
            })),
            None
        );
        assert_eq!(
            query_from_payload(&json!({
                "hook_event_name": "UserPromptSubmit",
                "prompt": "  過去の   認証設計を確認して  "
            })),
            Some("過去の 認証設計を確認して".into())
        );
    }

    #[test]
    fn legacy_disabled_initialize_skips_search_without_exposing_context() {
        let mut rpc = FakeRpc {
            responses: VecDeque::from([json!({
                "result": {"capabilities": {}, "instructions": "disabled"}
            })]),
            ..Default::default()
        };

        assert_eq!(
            retrieve(&mut rpc, "認証", "codex-cli/gpt-5-codex").unwrap(),
            None
        );
        assert_eq!(rpc.calls.len(), 1);
    }

    #[test]
    fn authoritative_disabled_tool_result_skips_context_without_degradation() {
        let mut rpc = FakeRpc {
            responses: VecDeque::from([
                json!({"result": {"capabilities": {"tools": {}}}}),
                json!({"result": {
                    "content": [{"type": "text", "text": "disabled"}],
                    "structuredContent": {
                        "code": KB_DISABLED_CODE,
                        "authoritative": true,
                        "retryable": false,
                        "data": []
                    },
                    "isError": true
                }}),
            ]),
            ..Default::default()
        };

        assert_eq!(
            retrieve(&mut rpc, "認証", "codex-cli/gpt-5-codex").unwrap(),
            None
        );
        assert_eq!(
            rpc.calls
                .iter()
                .map(|(method, _)| method.as_str())
                .collect::<Vec<_>>(),
            ["initialize", "tools/call"]
        );
    }

    #[test]
    fn retrieval_gets_ranked_documents_in_one_mcp_search() {
        let mut rpc = FakeRpc {
            responses: VecDeque::from([
                json!({"result": {"capabilities": {"tools": {}}}}),
                json!({"result": {
                    "content": [{"type": "text", "text": "- notes/auth [認証]: snippet"}],
                    "structuredContent": {
                        "hits": [{"id": "notes/auth", "title": "認証"}],
                        "documents": [
                            {"id": "notes/auth", "text": "---\ntitle: 認証\n---\n全文", "source": "search", "depth": 0},
                            {"id": "notes/policy", "text": "---\ntitle: 方針\n---\nリンク先全文", "source": "outgoing_link", "depth": 1}
                        ],
                        "retrieval": {
                            "seed_count": 1,
                            "candidate_count": 2,
                            "selected_count": 2,
                            "estimated_tokens": 80,
                            "elapsed_us": 24
                        },
                        "retrieval_candidates": [
                            {"id": "notes/auth", "title": "認証", "source": "search", "depth": 0, "selected": true},
                            {"id": "notes/policy", "title": "方針", "source": "outgoing_link", "depth": 1, "selected": true},
                            {"id": "notes/deep", "title": "詳細", "source": "outgoing_link", "depth": 2, "selected": false, "omitted_reason": "token_budget"}
                        ]
                    }
                }}),
            ]),
            ..Default::default()
        };

        let context = retrieve(&mut rpc, "認証", "codex-cli/gpt-5-codex")
            .unwrap()
            .unwrap();
        assert!(context.contains("自動retrieval — MCP"));
        assert!(context.contains("title: 認証"));
        assert!(context.contains("title: 方針"));
        assert!(context.contains("source: \"outgoing_link\"; depth: 1"));
        assert!(context.contains("seed=1 / 候補=2 / 本文=2 / 推定token=80"));
        assert!(context.contains("note=\"notes/deep\"; title=\"詳細\""));
        assert!(context.contains("reason=\"token_budget\""));
        assert_eq!(
            rpc.calls
                .iter()
                .map(|(method, _)| method.as_str())
                .collect::<Vec<_>>(),
            ["initialize", "tools/call"]
        );
        assert_eq!(rpc.calls[1].1["arguments"]["any"], true);
        assert_eq!(rpc.calls[1].1["arguments"]["include_documents"], true);
        assert_eq!(rpc.calls[1].1["arguments"]["limit"], SEARCH_SEED_LIMIT);
    }

    /// 2026-09-05: 同じ検索結果でもhost向けの単位付き予算を適用し、警告を末尾へ追いやらない。
    #[test]
    fn retrieval_applies_the_client_budget_without_changing_search() {
        let body = "本文の日本語".repeat(800);
        for (client, expected_documents) in [("claude-code/claude", 1), ("codex-cli/gpt", 0)] {
            let mut rpc = FakeRpc {
                responses: VecDeque::from([
                    json!({"result": {"capabilities": {"tools": {}}}}),
                    json!({"result": {"structuredContent": {
                        "hits": [{"id": "notes/large"}],
                        "documents": [{"id": "notes/large", "text": body}],
                        "degraded": [{"code": "semantic_search", "detail": "索引の再構築待ち"}]
                    }}}),
                ]),
                ..Default::default()
            };
            let context = retrieve(&mut rpc, "日本語", client).unwrap().unwrap();
            let stats: Value = serde_json::from_str(
                context
                    .lines()
                    .find_map(|line| line.strip_prefix("出力統計: "))
                    .unwrap(),
            )
            .unwrap();
            let budget = ClientSurface::from_hint(client).hook_output_budget();
            assert!(budget.measure(&context) <= budget.limit);
            assert_eq!(stats["emitted_bytes"], context.len());
            assert_eq!(stats["emitted_documents"], expected_documents);
            assert!(context.contains("semantic_search"));
            assert_eq!(context.contains(&body), expected_documents == 1);
            assert_eq!(rpc.calls.len(), 2);
            assert_eq!(rpc.calls[1].1["arguments"]["include_documents"], true);
        }
    }

    /// 2026-09-05: 検索失敗の劣化付き0件を早期returnで正常な該当なしへ見せていた。
    #[test]
    fn empty_search_does_not_hide_structured_degradation() {
        let mut rpc = FakeRpc {
            responses: VecDeque::from([
                json!({"result": {"capabilities": {"tools": {}}}}),
                json!({"result": {"structuredContent": {
                    "hits": [], "documents": [],
                    "degraded": [{"code": "main_search", "detail": "検索失敗"}]
                }}}),
            ]),
            ..Default::default()
        };
        let context = retrieve(&mut rpc, "過去の方針", "codex-cli/gpt")
            .unwrap()
            .unwrap();
        assert!(context.contains("main_search"));
        assert!(context.find("検索失敗").unwrap() < context.find("該当なし").unwrap());
    }

    #[test]
    fn auto_retrieval_child_never_runs_remote_sync_and_pins_the_hook_profile() {
        assert_eq!(
            child_mcp_args("codex-cli/gpt-5-codex"),
            [
                "--mcp",
                "--no-remote-sync",
                "--hook-context",
                "--require-client-binding",
                "--mcp-surface",
                "read",
                "--client",
                "codex-cli/gpt-5-codex",
                "--retrieval-profile",
                "session-auto"
            ]
        );
    }
    #[test]
    fn blocked_mcp_reader_has_a_bounded_deadline() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let started = Instant::now();
        let result = receive_before(&receiver, started + std::time::Duration::from_millis(20));
        assert!(result.unwrap_err().to_string().contains("deadline"));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        sender.send(Ok("response".into())).unwrap();
        assert_eq!(
            receive_before(
                &receiver,
                Instant::now() + std::time::Duration::from_secs(1)
            )
            .unwrap(),
            "response"
        );
    }

    #[test]
    fn harvest_lines_are_in_the_final_ledger_measurement_only_when_enabled() {
        for enabled in [true, false] {
            let mut response = search_response();
            response["result"]["structuredContent"]["harvest_status_line"] = json!(enabled);
            let mut rpc = FakeRpc {
                responses: VecDeque::from([initialized(Some(true)), response]),
                ..Default::default()
            };
            let mut ledger = TestLedger::new();
            let mut output = Vec::new();
            execute_hook(
                &mut rpc,
                &user_payload(),
                "codex-cli/gpt",
                &mut ledger,
                &mut output,
            )
            .unwrap();
            let text = String::from_utf8(output.clone()).unwrap();
            assert_eq!(text.contains("KB記録: fixture"), enabled);
            assert_eq!(
                observation(&ledger.events[0])["stats"]["emitted_bytes"],
                output.len()
            );
        }
    }

    /// 2026-09-06: 診断・開始時刻は起動経路の値を保持し、設定は最後の子MCP応答から採る。
    #[test]
    fn measurement_preserves_launch_context_and_uses_latest_policy() {
        let mut initialize = initialized(Some(true));
        initialize["result"]["capabilities"]["experimental"]["kbApp"]["observation_measurement"] =
            json!({"arm": "gui_on"});
        for (metadata, expected_arm) in [
            (json!({"arm": "gui_off"}), "gui_off"),
            (json!({"arm": "environment_off"}), "environment_off"),
            (json!({"arm": "gui_on"}), "gui_on"),
            (json!({"arm": "unknown"}), "unknown"),
            (json!({"arm": "PRIVATE_INVALID_ARM"}), "unknown"),
            (json!({"arm": 42}), "unknown"),
            (Value::Null, "unknown"),
        ] {
            let mut response = search_response();
            response["result"]["structuredContent"]["observation_measurement"] = metadata;
            // 旧応答のharvest_status_line=falseを、GUI OFFという計測事実へ昇格させない。
            response["result"]["structuredContent"]["harvest_status_line"] = json!(false);
            let mut rpc = FakeRpc {
                responses: VecDeque::from([initialize.clone(), response]),
                ..Default::default()
            };
            let mut ledger = TestLedger::new();
            let measurement = MeasurementContext {
                purpose: session_ledger::MeasurementPurpose::Diagnostic,
                session_started_at_ms: Some(1),
                ..Default::default()
            };
            execute_hook_with_measurement(
                &mut rpc,
                &user_payload(),
                "claude-code/claude",
                measurement,
                &mut ledger,
                &mut Vec::new(),
            )
            .unwrap();
            let measured = &ledger.events[0]["measurement"];
            assert_eq!(measured["purpose"], "diagnostic");
            assert_eq!(measured["session_started_at_ms"], 1);
            assert_eq!(measured["arm"], expected_arm);
            assert_eq!(
                measured["final_output"],
                json!({"status_line_present": false, "cadence_line_present": false, "status_omitted_for_budget": false})
            );
            assert!(
                !serde_json::to_string(&ledger.events)
                    .unwrap()
                    .contains("PRIVATE_")
            );
            assert_eq!(rpc.calls.len(), 2);
        }
    }

    /// 2026-09-06: フィルターはinitializeだけで計測し、検索失敗時に古い設定を流用しない。
    #[test]
    fn filtered_and_failed_measurements_keep_only_available_metadata() {
        for filtered in [true, false] {
            let mut initialize = initialized(Some(true));
            initialize["result"]["capabilities"]["experimental"]["kbApp"]["observation_measurement"] =
                json!({"arm": "gui_off"});
            let mut rpc = FakeRpc {
                responses: VecDeque::from([initialize]),
                ..Default::default()
            };
            let mut payload = user_payload();
            // Hook入力由来の開始時刻・計測情報は採用しない。
            payload["session_started_at_ms"] = json!(1);
            payload["observation_measurement"] = json!({"purpose": "diagnostic", "arm": "gui_on"});
            if filtered {
                payload["prompt"] = json!("ok");
            }
            let mut ledger = TestLedger::new();
            execute_hook_with_measurement(
                &mut rpc,
                &payload,
                "claude-code/claude",
                MeasurementContext {
                    purpose: session_ledger::MeasurementPurpose::Normal,
                    ..Default::default()
                },
                &mut ledger,
                &mut Vec::new(),
            )
            .unwrap();
            let measured = &ledger.events[0]["measurement"];
            assert_eq!(measured["purpose"], "normal");
            assert_eq!(
                measured["arm"],
                if filtered { "gui_off" } else { "unknown" }
            );
            assert!(measured["session_started_at_ms"].is_null());
            assert!(measured["final_output"].is_null());
            assert!(ledger.events[0]["workspace_id"].is_null());
            assert_eq!(rpc.calls.len(), if filtered { 1 } else { 2 });
        }
    }

    /// 2026-09-06: 状態行の採否は本文の文字列や設定値でなく、最終rendererの結果を残す。
    #[test]
    fn measurement_distinguishes_rendered_omitted_and_absent_status() {
        for (enabled, status, cadence, omitted) in [
            (true, "KB記録: fixture。\n".to_owned(), false, false),
            (
                true,
                "KB記録: fixture。\nKB手入れ: fixture。\n".to_owned(),
                true,
                false,
            ),
            (true, "KB記録: fixture。\n".repeat(4000), false, true),
            (
                false,
                "KB記録: fixture。\nKB手入れ: fixture。\n".to_owned(),
                false,
                false,
            ),
        ] {
            for client in ["codex-cli/gpt", "claude-code/claude"] {
                let mut response = search_response();
                response["result"]["structuredContent"]["harvest_status_line"] = json!(enabled);
                response["result"]["structuredContent"]["observation_measurement"] =
                    json!({"arm": if enabled { "gui_on" } else { "gui_off" }});
                response["result"]["structuredContent"]["documents"][0]["text"] = json!(
                    "KB記録: 本文中の例\nKB手入れ: 本文中の例\n省略: KB状態行（出力予算）。\n"
                );
                let mut rpc = FakeRpc {
                    responses: VecDeque::from([initialized(Some(true)), response]),
                    ..Default::default()
                };
                let mut ledger = TestLedger::new();
                ledger.harvest_text = status.clone();
                let mut output = Vec::new();
                execute_hook_with_measurement(
                    &mut rpc,
                    &user_payload(),
                    client,
                    MeasurementContext::default(),
                    &mut ledger,
                    &mut output,
                )
                .unwrap();
                assert_eq!(
                    ledger.events[0]["measurement"]["final_output"],
                    json!({"status_line_present": enabled && !omitted, "cadence_line_present": cadence, "status_omitted_for_budget": omitted})
                );
                assert_eq!(
                    observation(&ledger.events[0])["stats"]["emitted_bytes"],
                    output.len()
                );
                let text = String::from_utf8(output).unwrap();
                let budget = ClientSurface::from_hint(client).hook_output_budget();
                assert!(budget.measure(&text) <= budget.limit);
                assert_eq!(ledger.finalized, [HookEmissionOutcome::Emitted]);
            }
        }
    }
}
