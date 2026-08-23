//! MCP サーバー(stdio、newline-delimited JSON-RPC 2.0)。
//! 公開ツールは search / get / recent / inspect_markdown_conflict /
//! resolve_markdown_conflict / plan_distillation / plan_targeted_distillation / audit_distillation /
//! distillation_cadence_status / run_distillation_cadence / apply_distillation /
//! rollback_distillation / plan_initiative_closure / apply_initiative_closure /
//! rollback_initiative_closure / plan_legacy_artifact_promotions /
//! apply_legacy_artifact_promotion / rollback_legacy_artifact_promotion /
//! propose / update / prepare_remove / commit_remove / attach。
//! 人間のノートは変更できない(所有ガード)。
//!
//! v0.1 は手組みの最小実装(依存最小・同期 I/O)。リモート化(Streamable HTTP)の
//! 段階で公式 Rust SDK(rmcp)への載せ替えを再評価する(ADR-0001 スタック表)。

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use base64::Engine as _;
use rand::RngCore as _;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::client_surface::ClientSurface;
use crate::index::{open_db, open_db_read_only, open_db_recovery, sync_with_degradations};
use crate::search::{recent, search};
use crate::vault::{NoteProposal, NoteUpdate, Vault};

const PROTOCOL_FALLBACK: &str = "2025-06-18";
const KB_DISABLED_CODE: &str = "kb_disabled";
const REMOVAL_TOKEN_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug)]
struct PendingRemoval {
    note_id: String,
    title: String,
    reason: String,
    fingerprint: [u8; 32],
    expires_at: Instant,
}

#[derive(Debug, Default)]
struct RemovalPlans {
    pending: HashMap<String, PendingRemoval>,
}

impl RemovalPlans {
    fn prepare(
        &mut self,
        note_id: &str,
        title: &str,
        reason: &str,
        note_text: &str,
    ) -> Result<String> {
        let reason = reason.trim();
        if reason.is_empty() || reason.chars().count() > 500 || reason.contains(['\n', '\r']) {
            anyhow::bail!("削除理由は1〜500文字の一行で指定する");
        }
        self.pending
            .retain(|_, pending| pending.expires_at > Instant::now());
        loop {
            let mut bytes = [0_u8; 32];
            rand::rng().fill_bytes(&mut bytes);
            let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            if self.pending.contains_key(&token) {
                continue;
            }
            self.pending.insert(
                token.clone(),
                PendingRemoval {
                    note_id: note_id.to_string(),
                    title: title.to_string(),
                    reason: reason.to_string(),
                    fingerprint: Sha256::digest(note_text.as_bytes()).into(),
                    expires_at: Instant::now() + REMOVAL_TOKEN_TTL,
                },
            );
            return Ok(token);
        }
    }

    fn consume(&mut self, token: &str, note_id: &str) -> Result<PendingRemoval> {
        let pending = self.pending.remove(token).ok_or_else(|| {
            anyhow::anyhow!("削除tokenが無効または使用済み。prepare_removeからやり直す")
        })?;
        if pending.expires_at <= Instant::now() {
            anyhow::bail!("削除tokenの期限が切れた。prepare_removeからやり直す");
        }
        if pending.note_id != note_id {
            anyhow::bail!("削除対象がprepare_remove時と一致しない。prepare_removeからやり直す");
        }
        Ok(pending)
    }
}

impl PendingRemoval {
    fn require_unchanged(&self, note_text: &str) -> Result<()> {
        let fingerprint: [u8; 32] = Sha256::digest(note_text.as_bytes()).into();
        if self.fingerprint != fingerprint {
            anyhow::bail!("削除対象がprepare_remove後に変更された。内容を確認してからやり直す");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServeOptions {
    /// tool callの前にGitHub pullを試す。短命な自動retrieval processではfalseにし、
    /// Keychain認証やremote I/Oを発話回数へ結び付けない。
    pub remote_sync: bool,
    /// 公開するツール群。ホストが遅延ロードを誤判定しても、read面を小さく常時公開し、
    /// 書込・保守ツールは別MCP登録へ分離できるようにする。
    pub tool_surface: ToolSurface,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ToolSurface {
    /// CLI・評価fixture向けの後方互換面。
    #[default]
    All,
    Read,
    Write,
    Maintenance,
}

impl ToolSurface {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "all" => Ok(Self::All),
            "read" => Ok(Self::Read),
            "write" => Ok(Self::Write),
            "maintenance" => Ok(Self::Maintenance),
            _ => anyhow::bail!(
                "unknown MCP tool surface: {value} (expected all, read, write, or maintenance)"
            ),
        }
    }

    fn server_name(self) -> &'static str {
        match self {
            Self::All => "kb-app",
            Self::Read => "kb-app-read",
            Self::Write => "kb-app-write",
            Self::Maintenance => "kb-app-maintenance",
        }
    }

    fn allows(self, tool: &str) -> bool {
        match self {
            Self::All => true,
            Self::Read => matches!(tool, "search" | "get" | "recent"),
            Self::Write => matches!(
                tool,
                "propose" | "update" | "attach" | "prepare_remove" | "commit_remove"
            ),
            Self::Maintenance => matches!(
                tool,
                "inspect_markdown_conflict"
                    | "resolve_markdown_conflict"
                    | "plan_distillation"
                    | "plan_targeted_distillation"
                    | "audit_distillation"
                    | "distillation_cadence_status"
                    | "run_distillation_cadence"
                    | "apply_distillation"
                    | "rollback_distillation"
                    | "plan_initiative_closure"
                    | "apply_initiative_closure"
                    | "rollback_initiative_closure"
                    | "plan_legacy_artifact_promotions"
                    | "apply_legacy_artifact_promotion"
                    | "rollback_legacy_artifact_promotion"
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ToolCallOptions {
    remote_sync: bool,
    update_embeddings: bool,
    tool_surface: ToolSurface,
}

#[derive(Clone, Debug)]
pub struct EvaluationServeOptions {
    pub instructions: String,
    pub event_rules: Vec<crate::rule_delivery_eval::PreparedEventRule>,
    pub injected_degradations: Vec<String>,
    pub trace_path: Option<PathBuf>,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            remote_sync: true,
            tool_surface: ToolSurface::All,
        }
    }
}

/// server instructions(FR-C5)。「まず引く・終わりに起票を提案」の規律を配る。
/// 裁量が残る「引く・育てる」の規律。能力差はClientSurface別schemaへ、必須表示は
/// structured conversation eventへ分離し、instructionsだけを保証点にしない。
const INSTRUCTIONS_BASE: &str = "\
ユーザーの個人ナレッジベース(kb-app)への取次口。会話で「引く・育てる」を回す。\n\
【契約(機構で強制。正本はアプリの docs/contract.md)】ノートはタグ1〜4個必須/\
形式はアプリが管理(frontmatter を自分で書かない)。\n\
【引く】ユーザー個人に関する話題(嗜好・決定・進行中の作業・過去に調べたこと・固有名詞)は、\
推測で答える前に search(自然文可・意味で当たる。外したら語を変えて再検索)。ヒットは get で\
全文を読んでから答える。`[kb-app 自動retrieval — MCP]` が届いていれば、その検索・リンク展開・本文取得は\
実行済みなので、文脈が不足するときだけ追加検索する。本文中の /path.md リンクは必要なら辿る。\
該当なしは正常(その旨を添える)。degraded があれば回答に添える。\n\
【育てる】残す価値のある知見・決定が生まれたら、会話の終わりに propose を提案(承諾を得て\
から)。既存ノートの手入れは update で直接行う。蒸留・メンテナンス中の削除もAIの領分で、\
prepare_remove で対象と理由を固定し、追加の人間承認なしに commit_remove へ同じnoteと\
短命tokenを渡す。対象と理由は会話へ報告する。\
本文は未来の読者向けに自己完結で(経緯・出典・関連ノートへの /path.md リンク)。\n\
【ファイル】会話で作成・受領した画像や文書を既存ノートの持ち物にするときは attach を使う。\
ローカルpathではなく内容をBase64で渡す。保存先・持ち出し区分・来歴はkb-appが固定する。\n\
【関連】ノート間の関連づけはあなたの領分。get の「近いノート」を全文確認し、本当に関連するなら\
update のrelationsへ対象note_uidを含むtyped relationを設定する。relationsは全置換なので既存edgeを保持して送る。\
本文の /path.md リンクは人間可読な参照が必要な場合に併用する(ユーザーに可否を尋ねる形にはしない)。\n\
【authority】proposeでは共通6namespace、canonical/record/proposal role、authority status、\
同じ主題・適用範囲を示すscopeを必ず指定する。同じnamespace+scopeのactive canonicalを複数作らない。\
typed relationはnote_uidを端点にし、根拠・更新・矛盾・後継をpath変更から独立して結ぶ。\n\
【蒸留】まずdistillation_cadence_statusで追加直後／日次／週次／月次の期限を確認し、dueがあれば\
run_distillation_cadenceで最後に受入成功したcheckpointから増分監査する。cadence runは端末ローカルの\
checkpointだけを更新し、ノートの意味変更は行わない。個別baselineを比較するときはaudit_distillationを使う。\
audit内のplanは同一DB snapshotへ固定したread-only監査記録で、承認待ちqueueではない。候補ノートはgetで\
全文確認する。baselineなしの機械planはplan_distillation、全文監査で見つけたkeep対象のsemantic変更は\
plan_targeted_distillationで対象・operation・理由を先に固定する。planのnormalize /\
revise / extractを複数ノートへ反映するときはapply_distillationを使い、plan schema・profile・\
snapshot・input hashをそのまま渡す。executorは全対象を1 transactionで更新し、古いplan・二重実行・\
record本文改変を拒否する。失敗したwaveは対象が変わる前にrollback_distillationで一括復元する。\
完了したactive canonical initiativeのstatusだけをhistoricalへ変える場合はplan_initiative_closureの\
全出力をapply_initiative_closureへ渡し、失敗時はrollback_initiative_closureで一括復元する。\
create・delete・merge・supersede・splitはexecutor v1へ混ぜず、削除は既存の二段階removeを使う。\n\
【旧Artifact移行】物理再配置はplan_legacy_artifact_promotionsで1件単位のread-only planを取り、\
plan全体をapply_legacy_artifact_promotionへそのまま渡す。uploadとhash確認前にはlocatorを切り替えず、\
各件の直後にplanとgetを取り直す。失敗時はLegacyGitのまま停止し、apply直後から戻す必要がある場合だけ\
result全体をrollback_legacy_artifact_promotionへ渡す。VaultやCLIを代替経路にしない。\n\
【タグ】体系は会話でユーザーと合意して育てる(暫定・要確認といった扱いもタグで表す — \
アプリに下書き状態は無い)。合意済み(「タグ運用」ノート。無ければ起票を\
提案)は勝手に変えない。MCPの書込では既存語彙だけを使う。新語が本当に必要なら、\
通常の書込で追加せず、trusted UI / CLIの別承認が必要だと伝える。";

fn instructions_for(client: &str) -> String {
    let current_note = if ClientSurface::from_hint(client)
        .capabilities()
        .current_note_argument_optional
    {
        "【現在ノート】ユーザーが「このノート」と言った場合だけ、note引数なしのgetを使える。\n"
    } else {
        "【現在ノート】getのnote引数は必須。search結果などのノートIDを必ず指定する。\n"
    };
    format!("{INSTRUCTIONS_BASE}\n{current_note}")
}

/// OFF 時は KB の内容や保存先を渡さず、迂回禁止だけを制御プレーンとして配る。
/// ツールを非公開にするだけでは、汎用 shell を持つ AI が Vault を直読みできるため。
const DISABLED_INSTRUCTIONS: &str = "\
kb-app はこの AI クライアントで無効です [kb_disabled]。\
各ツールがこのコードを返す状態は権威ある終端結果であり、再試行できません。\
KB のデータを shell・ファイル操作・保存先の探索など別経路で参照・推測・更新しないでください。\
過去の会話に残る KB 内容も代替経路として使わず、必要な場合は「現在は KB を参照できない」と伝えてください。";

pub fn serve(client_hint: &str, open_vault: impl FnMut() -> Result<Vault>) -> Result<()> {
    serve_with_options(client_hint, ServeOptions::default(), open_vault)
}

pub fn serve_with_options(
    client_hint: &str,
    options: ServeOptions,
    open_vault: impl FnMut() -> Result<Vault>,
) -> Result<()> {
    serve_loop(client_hint, options, None, open_vault)
}

/// 固定fixture専用のMCP server。端末設定とAI guardを評価対象へ混ぜず、明示された
/// 一時Vaultだけを開く。production appからは呼ばない。
pub fn serve_evaluation(
    client_hint: &str,
    evaluation: EvaluationServeOptions,
    open_vault: impl FnMut() -> Result<Vault>,
) -> Result<()> {
    serve_loop(
        client_hint,
        ServeOptions {
            remote_sync: false,
            tool_surface: ToolSurface::All,
        },
        Some(evaluation),
        open_vault,
    )
}

fn serve_loop(
    client_hint: &str,
    options: ServeOptions,
    evaluation: Option<EvaluationServeOptions>,
    mut open_vault: impl FnMut() -> Result<Vault>,
) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut trace = evaluation
        .as_ref()
        .and_then(|options| options.trace_path.as_ref())
        .map(|path| {
            OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(path)
                .with_context(|| format!("評価traceを開けない: {}", path.display()))
        })
        .transpose()?;
    let mut vault = None;
    let mut removal_plans = RemovalPlans::default();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("kb mcp: parse error: {e}");
                continue;
            }
        };
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        // 通知(id なし)は応答しない
        let Some(id) = id else { continue };
        // GUIとは別プロセスなので、各要求で端末設定を読み直す。既に動いている
        // MCPもOFF後の次の要求からVaultへ触れなくなる。設定破損時はfail-closed。
        let enabled = if evaluation.is_some() {
            true
        } else {
            match crate::settings::load() {
                Ok(settings) => {
                    settings.ai_kb_enabled_for(client_hint)
                        && crate::ai_guard::client_connection_is_allowed(client_hint)
                }
                Err(error) => {
                    eprintln!("kb mcp: settings unavailable: {}", error.detail());
                    false
                }
            }
        };
        // initialize / list / OFF は Vault の場所すら開かない。ON の tool call が来た
        // 最初の1回だけ開き、以後は同じ process 内で再利用する。
        let needs_vault =
            method_needs_vault(enabled, method, options.tool_surface, msg.get("params"));
        if needs_vault && vault.is_none() {
            vault = Some(open_vault()?);
        }
        let handled = handle_with_search_options(
            vault.as_ref(),
            client_hint,
            enabled,
            ToolCallOptions {
                remote_sync: options.remote_sync,
                update_embeddings: evaluation.is_none(),
                tool_surface: options.tool_surface,
            },
            &mut removal_plans,
            method,
            msg.get("params"),
        );
        let response = match handled {
            Ok(Some(mut result)) => {
                if let Some(evaluation) = evaluation.as_ref() {
                    apply_evaluation_transform(evaluation, method, msg.get("params"), &mut result);
                }
                json!({"jsonrpc": "2.0", "id": id, "result": result})
            }
            Ok(None) => json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": format!("unknown method: {method}")}}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32603, "message": e.to_string()}}),
        };
        if let Some(trace) = trace.as_mut() {
            writeln!(
                trace,
                "{}",
                json!({
                    "at": crate::frontmatter::now_iso(),
                    "request": msg,
                    "response": response,
                })
            )?;
            trace.flush()?;
        }
        writeln!(out, "{response}")?;
        out.flush()?;
    }
    Ok(())
}

fn apply_evaluation_transform(
    evaluation: &EvaluationServeOptions,
    method: &str,
    params: Option<&Value>,
    result: &mut Value,
) {
    if method == "initialize" {
        result["instructions"] = json!(evaluation.instructions);
        return;
    }
    if method != "tools/call" {
        return;
    }
    let tool = params
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if tool == "search" && !evaluation.injected_degradations.is_empty() {
        if let Some(text) = result
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            let suffix = evaluation
                .injected_degradations
                .iter()
                .map(|code| format!("⚠ 劣化: [{code}] 評価fixtureによる注入"))
                .collect::<Vec<_>>()
                .join("\n");
            *result.pointer_mut("/content/0/text").unwrap() =
                json!(format!("{}\n{}\n", text.trim_end(), suffix));
        }
        result["structuredContent"]["eval_injected_degradations"] =
            json!(evaluation.injected_degradations);
    }
    let event_rules = evaluation
        .event_rules
        .iter()
        .filter(|rule| rule.after_tools.iter().any(|after| after == tool))
        .map(|rule| {
            json!({
                "rule_id": rule.rule_id,
                "instruction": rule.instruction,
            })
        })
        .collect::<Vec<_>>();
    if !event_rules.is_empty() {
        result["structuredContent"]["event_rules"] = json!(event_rules);
    }
}

fn method_needs_vault(
    enabled: bool,
    method: &str,
    tool_surface: ToolSurface,
    params: Option<&Value>,
) -> bool {
    enabled
        && method == "tools/call"
        && params
            .and_then(|params| params.get("name"))
            .and_then(Value::as_str)
            .is_some_and(|name| tool_surface.allows(name))
}

#[cfg(test)]
fn handle(
    vault: Option<&Vault>,
    client: &str,
    enabled: bool,
    remote_sync: bool,
    method: &str,
    params: Option<&Value>,
) -> Result<Option<Value>> {
    handle_with_search_options(
        vault,
        client,
        enabled,
        ToolCallOptions {
            remote_sync,
            update_embeddings: true,
            tool_surface: ToolSurface::All,
        },
        &mut RemovalPlans::default(),
        method,
        params,
    )
}

#[cfg(test)]
fn handle_on_surface(
    vault: Option<&Vault>,
    client: &str,
    enabled: bool,
    remote_sync: bool,
    tool_surface: ToolSurface,
    method: &str,
    params: Option<&Value>,
) -> Result<Option<Value>> {
    handle_with_search_options(
        vault,
        client,
        enabled,
        ToolCallOptions {
            remote_sync,
            update_embeddings: true,
            tool_surface,
        },
        &mut RemovalPlans::default(),
        method,
        params,
    )
}

fn handle_with_search_options(
    vault: Option<&Vault>,
    client: &str,
    enabled: bool,
    tool_options: ToolCallOptions,
    removal_plans: &mut RemovalPlans,
    method: &str,
    params: Option<&Value>,
) -> Result<Option<Value>> {
    match method {
        "initialize" => {
            let requested = params
                .and_then(|p| p.get("protocolVersion"))
                .and_then(|v| v.as_str())
                .unwrap_or(PROTOCOL_FALLBACK);
            let surface = ClientSurface::from_hint(client);
            let capabilities = surface.capabilities();
            let mut initialized = json!({
                "protocolVersion": requested,
                "capabilities": if enabled {
                    json!({
                        "tools": {},
                        "prompts": {},
                        "experimental": {"kbApp": capabilities},
                    })
                } else {
                    json!({
                        "tools": {},
                        "experimental": {"kbApp": capabilities},
                    })
                },
                "serverInfo": {"name": tool_options.tool_surface.server_name(), "version": env!("CARGO_PKG_VERSION")},
            });
            initialized["instructions"] = json!(if enabled {
                instructions_for(client)
            } else {
                DISABLED_INSTRUCTIONS.to_string()
            });
            Ok(Some(initialized))
        }
        "ping" => Ok(Some(json!({}))),
        "tools/list" => Ok(Some(
            json!({"tools": tool_definitions_for_surface(client, tool_options.tool_surface)}),
        )),
        // MCP prompts — 定型操作の入口(常駐コンテキストを増やさず、正しい挙動を
        // ワンタップで起動させる。Desktop のプロンプトピッカーに現れる)
        "prompts/list" => Ok(Some(if enabled {
            json!({"prompts": prompt_definitions()})
        } else {
            json!({"prompts": []})
        })),
        "prompts/get" => {
            if !enabled {
                anyhow::bail!(KB_DISABLED_CODE);
            }
            let name = params
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let text =
                prompt_text(name).ok_or_else(|| anyhow::anyhow!("unknown prompt: {name}"))?;
            Ok(Some(json!({
                "messages": [{"role": "user", "content": {"type": "text", "text": text}}]
            })))
        }
        "tools/call" => {
            if !enabled {
                return Ok(Some(disabled_tool_result()));
            }
            let name = params
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !tool_options.tool_surface.allows(name) {
                return Ok(Some(json!({
                    "content": [{"type": "text", "text": format!(
                        "エラー: tool '{name}' is not available on the {} MCP surface",
                        tool_options.tool_surface.server_name()
                    )}],
                    "structuredContent": {
                        "code": "tool_surface_mismatch",
                        "authoritative": true,
                        "retryable": false,
                        "surface": tool_options.tool_surface.server_name(),
                        "tool": name,
                    },
                    "isError": true,
                })));
            }
            let args = params
                .and_then(|p| p.get("arguments"))
                .cloned()
                .unwrap_or(json!({}));
            let vault = vault.context("Vault is unavailable")?;
            let output = call_tool_with_search_options(
                vault,
                client,
                name,
                &args,
                tool_options.remote_sync,
                tool_options.update_embeddings,
                removal_plans,
            );
            match output {
                Ok(output) => {
                    let mut result = json!({
                        "content": [{"type": "text", "text": output.text}]
                    });
                    if let Some(structured) = output.structured {
                        result["structuredContent"] = structured;
                    }
                    Ok(Some(result))
                }
                // ツール内エラーはプロトコルエラーにせず isError で返す(fail-open)
                Err(e) => Ok(Some(json!({
                    "content": [{"type": "text", "text": format!("エラー: {e}")}],
                    "structuredContent": {
                        "conversation_events": [{
                            "type": "error",
                            "required": true,
                            "message": e.to_string(),
                        }],
                    },
                    "isError": true,
                }))),
            }
        }
        _ => Ok(None),
    }
}

fn disabled_tool_result() -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": "KBは設定で無効です。別経路を探索しないでください [kb_disabled]"
        }],
        "structuredContent": {
            "code": KB_DISABLED_CODE,
            "authoritative": true,
            "retryable": false,
            "data": [],
            "conversation_events": [{
                "type": "kb_disabled",
                "required": true,
                "code": KB_DISABLED_CODE,
                "message": "KBは設定で無効です。別経路を探索しないでください",
            }],
        },
        "isError": true,
    })
}

fn prompt_definitions() -> Value {
    json!([
        {"name": "タグの整理", "description": "タグ体系を見直し、AI 裁量の範囲で統合・整理する"},
        {"name": "会話を起票", "description": "この会話の知見・決定を下書きノートとして起票する"},
        {"name": "最近のノートを点検", "description": "最近のノートを一緒に点検する(内容・タグ・つながり)"}
    ])
}

fn prompt_text(name: &str) -> Option<&'static str> {
    match name {
        "タグの整理" => Some(
            "kb-app の全タグの現状を把握して(recent と search を使う)、タグ体系を見直して。\
「タグ運用」ノートに合意があればそれに従い、合意のないタグはあなたの裁量で統合・改名・整理\
してよい(update で実行)。MCPでは既存語彙だけを使い、新語が必要なら別承認が必要だと伝えて。\
ユーザーの合意が要ると感じた変更は提案に留めて。\
終わったら、実行した整理と提案を一覧で報告して。",
        ),
        "会話を起票" => Some(
            "ここまでの会話から、恒久的に残す価値のある知見・決定・事実を洗い出して。\
それぞれについて一言で要約を見せて、私が選んだものを propose で起票して\
(タグ1〜4個・本文は未来の読者向けに自己完結・関連ノートへリンク)。",
        ),
        "最近のノートを点検" => Some(
            "kb-app の最近のノート(recent)を一つずつ、「要約・気になる点・タグの妥当性」の\
形で見せて。手を入れた方がよいものは update で直してから見せて(大きな変更は一言添える)。",
        ),
        _ => None,
    }
}

fn semantic_tool_definitions() -> [Value; 2] {
    let relation = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "type": {"type": "string", "enum": ["derived_from", "supports", "updates", "contradicts", "supersedes", "mentions"]},
            "target": {"type": "string", "pattern": "^[0-9A-HJKMNP-TV-Z]{26}$", "description": "参照先note_uid(ULID)"}
        },
        "required": ["type", "target"]
    });
    let target = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "title": {"type": ["string", "null"]},
            "body": {"type": "string"},
            "description": {"type": ["string", "null"]},
            "tags": {"type": "array", "minItems": 1, "maxItems": 4, "uniqueItems": true, "items": {"type": "string", "minLength": 1, "maxLength": 20, "pattern": "^[a-z0-9]+(?:-[a-z0-9]+)*$"}},
            "relations": {"type": "array", "uniqueItems": true, "items": relation}
        },
        "required": ["title", "body", "description", "tags", "relations"]
    });
    let change = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "note": {"type": "string", "description": "plan entryのnote ID"},
            "input_hash": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"},
            "operation": {"type": "string", "enum": ["normalize", "revise", "extract"]},
            "reason": {"type": "string", "minLength": 1, "maxLength": 500, "pattern": "^[^\\r\\n]+$", "description": "semantic変更の根拠(一行)"},
            "target": target
        },
        "required": ["note", "input_hash", "operation", "reason", "target"]
    });
    let apply = json!({
        "name": "apply_distillation",
        "description": "mechanical-v1またはtargeted-v1 planの同一snapshotを再照合し、既存AIノートのnormalize / revise / extractを1 transactionで反映する。create・delete・merge・supersede・split・authority変更は受け付けない。",
        "annotations": {
            "title": "蒸留waveを実行",
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": false,
            "openWorldHint": false
        },
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "schema": {"type": "string", "const": "kb-app.distillation-execution-request/v1"},
                "plan_schema": {"type": "string", "const": "kb-app.distillation-plan/v1"},
                "planner_profile": {"type": "string", "enum": ["mechanical-v1", "targeted-v1"]},
                "plan_id": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"},
                "snapshot_digest": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"},
                "snapshot_note_count": {"type": "integer", "minimum": 0},
                "changes": {"type": "array", "minItems": 1, "maxItems": 100, "items": change}
            },
            "required": ["schema", "plan_schema", "planner_profile", "plan_id", "snapshot_digest", "snapshot_note_count", "changes"]
        }
    });
    let rollback = json!({
        "name": "rollback_distillation",
        "description": "適用済みsemantic executionの全対象が直後snapshotのままなら、同じtransactionで実行前documentへ復元する。二重rollbackと後続変更後のrollbackは拒否する。",
        "annotations": {
            "title": "蒸留waveをrollback",
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": false,
            "openWorldHint": false
        },
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "execution_id": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"}
            },
            "required": ["execution_id"]
        }
    });
    [apply, rollback]
}

fn targeted_distillation_tool_definition() -> Value {
    json!({
        "name": "plan_targeted_distillation",
        "description": "全文監査で見つけた既存AIノートのnormalize / revise / extractを、対象・operation・理由・input hash・DB snapshotへ固定するread-only plan。apply_distillationとrollback_distillationで実行・復元する。",
        "annotations": {
            "title": "対象指定の蒸留waveを計画",
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false
        },
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "changes": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 100,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "note": {"type": "string", "minLength": 1, "description": "全文確認済みの既存note ID"},
                            "operation": {"type": "string", "enum": ["normalize", "revise", "extract"]},
                            "reason": {"type": "string", "minLength": 1, "maxLength": 500, "pattern": "^[^\\r\\n]+$", "description": "このoperationをplanへ載せる根拠(一行)"}
                        },
                        "required": ["note", "operation", "reason"]
                    }
                }
            },
            "required": ["changes"]
        }
    })
}

fn initiative_closure_tool_definitions() -> [Value; 3] {
    let plan_change = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "note": {"type": "string", "minLength": 1, "description": "完了内容を全文確認済みのactive canonical initiative note ID"},
            "reason": {"type": "string", "minLength": 1, "maxLength": 500, "pattern": "^[^\\r\\n]+$", "description": "initiativeを完了扱いにする根拠(一行)"}
        },
        "required": ["note", "reason"]
    });
    let execution_change = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "note": {"type": "string", "minLength": 1},
            "note_uid": {"type": "string", "pattern": "^[0-9A-HJKMNP-TV-Z]{26}$"},
            "input_hash": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"},
            "from_status": {"type": "string", "const": "active"},
            "to_status": {"type": "string", "const": "historical"},
            "reason": {"type": "string", "minLength": 1, "maxLength": 500, "pattern": "^[^\\r\\n]+$"}
        },
        "required": ["note", "note_uid", "input_hash", "from_status", "to_status", "reason"]
    });
    let plan = json!({
        "name": "plan_initiative_closure",
        "description": "全文確認済みのAI管理active canonical initiativeをhistoricalへ閉じるwaveを、対象・note_uid・input hash・DB snapshot・理由へ固定するread-only plan。本文やidentityは変更しない。",
        "annotations": {
            "title": "initiative完了waveを計画",
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false
        },
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "changes": {"type": "array", "minItems": 1, "maxItems": 100, "items": plan_change}
            },
            "required": ["changes"]
        }
    });
    let apply = json!({
        "name": "apply_initiative_closure",
        "description": "initiative-close-v1 planの全対象を同一snapshotへ再照合し、authority statusのactive→historicalだけを1 transactionで反映する。",
        "annotations": {
            "title": "initiative完了waveを実行",
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": false,
            "openWorldHint": false
        },
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "schema": {"type": "string", "const": "kb-app.initiative-closure-execution-request/v1"},
                "plan_schema": {"type": "string", "const": "kb-app.initiative-closure-plan/v1"},
                "planner_profile": {"type": "string", "const": "initiative-close-v1"},
                "plan_id": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"},
                "snapshot_digest": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"},
                "snapshot_note_count": {"type": "integer", "minimum": 0},
                "changes": {"type": "array", "minItems": 1, "maxItems": 100, "items": execution_change}
            },
            "required": ["schema", "plan_schema", "planner_profile", "plan_id", "snapshot_digest", "snapshot_note_count", "changes"]
        }
    });
    let rollback = json!({
        "name": "rollback_initiative_closure",
        "description": "適用済みinitiative完了waveの全対象が直後snapshotのままなら、同じtransactionで実行前のactive状態へ復元する。",
        "annotations": {
            "title": "initiative完了waveをrollback",
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": false,
            "openWorldHint": false
        },
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "execution_id": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"}
            },
            "required": ["execution_id"]
        }
    });
    [plan, apply, rollback]
}

fn distillation_audit_tool_definition() -> Value {
    let checkpoint_entry = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "note": {"type": "string", "minLength": 1},
            "note_uid": {
                "type": ["string", "null"],
                "pattern": "^[0-9A-HJKMNP-TV-Z]{26}$"
            },
            "input_hash": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"}
        },
        "required": ["note", "note_uid", "input_hash"]
    });
    let checkpoint = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schema": {"type": "string", "const": "kb-app.distillation-checkpoint/v1"},
            "checkpoint_id": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"},
            "plan_schema": {"type": "string", "const": "kb-app.distillation-plan/v1"},
            "planner_profile": {"type": "string", "const": "mechanical-v1"},
            "plan_id": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"},
            "snapshot_digest": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"},
            "snapshot_note_count": {"type": "integer", "minimum": 0},
            "entries": {"type": "array", "items": checkpoint_entry}
        },
        "required": ["schema", "checkpoint_id", "plan_schema", "planner_profile", "plan_id", "snapshot_digest", "snapshot_note_count", "entries"]
    });
    json!({
        "name": "audit_distillation",
        "description": "現在の決定的planを前回checkpointと比較し、追加・変更・移動・削除と依存閉包workset、Storage Contract・Markdown outbox・local Git backupを含む受入gateを返す。pull・network I/O・KB更新は行わない。",
        "annotations": {
            "title": "継続蒸留を増分監査",
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false
        },
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "baseline": checkpoint
            }
        }
    })
}

fn distillation_cadence_tool_definitions() -> [Value; 2] {
    let status = json!({
        "name": "distillation_cadence_status",
        "description": "最後に受入成功したcheckpointと現在planを比較し、追加直後／日次／週次／月次のどの蒸留監査がdueかを返す。KB・checkpointとも変更しない。",
        "annotations": {
            "title": "継続蒸留cadenceを確認",
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false
        },
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {}
        }
    });
    let run = json!({
        "name": "run_distillation_cadence",
        "description": "dueまたは指定laneの増分auditを実行する。gate PASS時だけ端末ローカルcheckpointを進め、失敗時は旧checkpointと失敗理由を保持する。ノートは変更しない。",
        "annotations": {
            "title": "継続蒸留cadenceを実行",
            "readOnlyHint": false,
            "destructiveHint": false,
            "idempotentHint": false,
            "openWorldHint": false
        },
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "lane": {
                    "type": "string",
                    "enum": ["after_write", "daily", "weekly", "monthly"],
                    "description": "省略時はdue laneをまとめて実行。指定時はその深度を強制実行"
                }
            }
        }
    });
    [status, run]
}

fn legacy_promotion_plan_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schema": {"type": "string", "const": "kb-app.legacy-artifact-promotion-plan/v1"},
            "plan_id": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"},
            "artifact_id": {"type": "string", "pattern": "^[0-9A-HJKMNP-TV-Z]{26}$"},
            "manifest_version": {"type": "integer", "minimum": 1},
            "note_id": {"type": "string", "minLength": 1},
            "file_name": {"type": "string", "minLength": 1},
            "legacy_path": {"type": "string", "minLength": 1},
            "hash": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
            "size": {"type": "integer", "minimum": 0},
            "destination": {"type": "string", "minLength": 1},
            "reference": {
                "type": ["object", "null"],
                "additionalProperties": false,
                "properties": {
                    "name": {"type": "string", "minLength": 1, "maxLength": 64},
                    "revision": {"type": "integer", "minimum": 1}
                },
                "required": ["name", "revision"]
            },
            "aliases": {
                "type": "array",
                "items": {
                    "type": "array",
                    "prefixItems": [{"type": "string"}, {"type": "string"}],
                    "minItems": 2,
                    "maxItems": 2
                }
            }
        },
        "required": ["schema", "plan_id", "artifact_id", "manifest_version", "note_id", "file_name", "legacy_path", "hash", "size", "destination", "reference", "aliases"]
    })
}

fn legacy_promotion_result_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schema": {"type": "string", "const": "kb-app.legacy-artifact-promotion-result/v1"},
            "result_id": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"},
            "plan_id": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"},
            "artifact_id": {"type": "string", "pattern": "^[0-9A-HJKMNP-TV-Z]{26}$"},
            "before_version": {"type": "integer", "minimum": 1},
            "after_version": {"type": "integer", "minimum": 2},
            "note_id": {"type": "string", "minLength": 1},
            "file_name": {"type": "string", "minLength": 1},
            "hash": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
            "old_path_retained": {"type": "boolean", "const": true},
            "already_applied": {"type": "boolean"}
        },
        "required": ["schema", "result_id", "plan_id", "artifact_id", "before_version", "after_version", "note_id", "file_name", "hash", "old_path_retained", "already_applied"]
    })
}

fn legacy_promotion_tool_definitions() -> [Value; 3] {
    let plan = json!({
        "name": "plan_legacy_artifact_promotions",
        "description": "LegacyGit Artifactを1件ずつManagedへ昇格する決定的planを読み取り専用で返す。bytes hash・manifest version・ref revision・aliasを固定し、network I/OもKB更新も行わない。",
        "annotations": {
            "title": "旧Artifactの昇格を計画",
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false
        },
        "inputSchema": {"type": "object", "additionalProperties": false, "properties": {}}
    });
    let apply = json!({
        "name": "apply_legacy_artifact_promotion",
        "description": "planを現在状態と再照合し、LFS objectのhashとorigin upload成功後にだけ1 Artifactのprimary locatorをManagedへ昇格する。旧パスは削除しない。",
        "annotations": {
            "title": "旧Artifactを昇格",
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": true,
            "openWorldHint": true
        },
        "inputSchema": legacy_promotion_plan_schema()
    });
    let rollback = json!({
        "name": "rollback_legacy_artifact_promotion",
        "description": "apply直後の対象固定resultを再照合し、primary locatorをLegacyGitへ補償復元する。旧実体・LFS object・pointerは削除しない。",
        "annotations": {
            "title": "旧Artifactの昇格をrollback",
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": false,
            "openWorldHint": true
        },
        "inputSchema": legacy_promotion_result_schema()
    });
    [plan, apply, rollback]
}

#[cfg(test)]
fn tool_definitions(client: &str) -> Value {
    tool_definitions_for_surface(client, ToolSurface::All)
}

fn tool_definitions_for_surface(client: &str, tool_surface: ToolSurface) -> Value {
    let capabilities = ClientSurface::from_hint(client).capabilities();
    let mut definitions = json!([
        {
            "name": "search",
            "description": "KB 検索(全文+意味+リンク近傍)。個人の話題ではまず引く。自然文可。degraded は回答に添える。",
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {
                "query": {"type": "string", "description": "検索語(自然文可)"},
                "limit": {"type": "integer", "description": "最大件数(既定8)"},
                "any": {"type": "boolean", "description": "語をOR結合する(発話全文の自動retrieval用)"},
                "include_documents": {"type": "boolean", "description": "検索seedとリンク近傍の本文を予算内で同じ検索応答に含める(自動retrieval用)"}
            }, "required": ["query"]}
        },
        {
            "name": "get",
            "description": "ノート全文の取得(応答に添付と「近いノート」が付く)。search のヒットは必ず全文を読む。note 省略=いま開いているノート。",
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {
                "note": {"type": "string", "description": "ノート ID。省略=いま開いているノート"}
            }}
        },
        {
            "name": "attach",
            "description": "会話で作成・受領した小さなファイルを既存ノートへ添付。pathではなくBase64内容だけを渡し、保存先・区分・来歴はkb-appが固定する(上限16 MiB)。",
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {
                "note": {"type": "string", "description": "ひもづけ先の既存ノート ID"},
                "file_name": {"type": "string", "description": "表示ファイル名(区切り文字なし)"},
                "content_base64": {"type": "string", "description": "ファイル内容の標準Base64"},
                "ref_name": {"type": "string", "description": "本文から安定参照する任意の参照名(workspace内で一意)"}
            }, "required": ["note", "file_name", "content_base64"]}
        },
        {
            "name": "recent",
            "description": "最近のノート一覧。",
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {
                "limit": {"type": "integer", "description": "最大件数(既定10)"}
            }}
        },
        {
            "name": "inspect_markdown_conflict",
            "description": "DB確定documentのMarkdown出力が外部編集で停止した対象を読み取り専用で検査し、外部Markdownと保留documentの全文・SHA-256を返す。解消前に必ず実行する。",
            "annotations": {
                "title": "Markdown出力競合を検査",
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false
            },
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {
                "note": {"type": "string", "description": "競合しているノート ID"}
            }, "required": ["note"]}
        },
        {
            "name": "resolve_markdown_conflict",
            "description": "inspect_markdown_conflictで確認した外部Markdownを、検査時の両SHA-256へ固定してDB確定documentで置換し、保留outboxを再開する。strategy v1はkeep_dbのみ。",
            "annotations": {
                "title": "Markdown出力競合を解消",
                "readOnlyHint": false,
                "destructiveHint": true,
                "idempotentHint": false,
                "openWorldHint": false
            },
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {
                "note": {"type": "string", "description": "inspectで確認したノート ID"},
                "strategy": {"type": "string", "const": "keep_db"},
                "markdown_hash": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"},
                "pending_document_hash": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"}
            }, "required": ["note", "strategy", "markdown_hash", "pending_document_hash"]}
        },
        {
            "name": "plan_distillation",
            "description": "DBの同一read snapshotから、input hash・snapshot digest・決定的plan ID付きの蒸留候補を列挙する。KB、remote、索引、careを変更しない。",
            "annotations": {
                "title": "継続蒸留を計画",
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false
            },
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {}}
        },
        {
            "name": "propose",
            "description": "知見をauthority付きノートとして起票。本文は自己完結の Markdown で、経緯・出典と関連ノートへの /path.md リンクを含める。",
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {
                "title": {"type": "string", "description": "内容が一意に分かるタイトル"},
                "body": {"type": "string", "description": "本文(自己完結)"},
                "description": {"type": "string", "description": "一文要約"},
                "tags": {"type": "array", "items": {"type": "string"}, "description": "タグ1〜4個(必須・既存語彙のみ。英小文字ケバブ)"},
                "authority": {"type": "object", "additionalProperties": false, "properties": {
                    "namespace": {"type": "string", "enum": ["entities", "initiatives", "decisions", "procedures", "records", "knowledge"]},
                    "role": {"type": "string", "enum": ["canonical", "record", "proposal"]},
                    "status": {"type": "string", "enum": ["active", "historical", "superseded"]},
                    "scope": {"type": "string", "description": "同じ主題・適用範囲のcanonicalを一意にする安定key"}
                }, "required": ["namespace", "role", "status", "scope"]},
                "relations": {"type": "array", "description": "初期typed relation一覧。targetは既存ノートのnote_uid", "items": {"type": "object", "additionalProperties": false, "properties": {
                    "type": {"type": "string", "enum": ["derived_from", "supports", "updates", "contradicts", "supersedes", "mentions"]},
                    "target": {"type": "string", "description": "参照先note_uid(ULID)"}
                }, "required": ["type", "target"]}}
            }, "required": ["title", "body", "tags", "authority"]}
        },
        {
            "name": "update",
            "description": "AI ノートの直接更新(指定フィールドのみ置換)。大きな書き換えは一言添えてから。",
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {
                "note": {"type": "string", "description": "ノート ID"},
                "title": {"type": "string"},
                "body": {"type": "string", "description": "本文全体の置換"},
                "description": {"type": "string"},
                "tags": {"type": "array", "items": {"type": "string"}, "description": "既存語彙のみ(全消し・5個以上は不可)"},
                "authority": {"type": "object", "additionalProperties": false, "properties": {
                    "namespace": {"type": "string", "enum": ["entities", "initiatives", "decisions", "procedures", "records", "knowledge"]},
                    "role": {"type": "string", "enum": ["canonical", "record", "proposal"]},
                    "status": {"type": "string", "enum": ["active", "historical", "superseded"]},
                    "scope": {"type": "string"}
                }, "required": ["namespace", "role", "status", "scope"]},
                "relations": {"type": "array", "description": "typed relation一覧を全置換。getで既存edgeを確認してから送る", "items": {"type": "object", "additionalProperties": false, "properties": {
                    "type": {"type": "string", "enum": ["derived_from", "supports", "updates", "contradicts", "supersedes", "mentions"]},
                    "target": {"type": "string", "description": "参照先note_uid(ULID)"}
                }, "required": ["type", "target"]}}
            }, "required": ["note"]}
        },
        {
            "name": "prepare_remove",
            "description": "AIノートの自律削除を準備。対象と現在内容を固定した5分間有効のtoken、理由、必須eventを返す。削除方針の範囲内なら追加の人間承認なしにcommit_removeへ進む。",
            "annotations": {
                "title": "削除対象を固定",
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": false,
                "openWorldHint": false
            },
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {
                "note": {"type": "string", "description": "削除候補のノート ID"},
                "reason": {"type": "string", "minLength": 1, "maxLength": 500, "description": "AIが判断した削除理由(一行)"}
            }, "required": ["note", "reason"]}
        },
        {
            "name": "commit_remove",
            "description": "prepare_removeで固定したAI管理ノートを自律削除。noteと短命tokenを照合し、理由と履歴を残して削除する。個別の人間承認は要求しない。",
            "annotations": {
                "title": "ノートを削除",
                "readOnlyHint": false,
                "destructiveHint": true,
                "idempotentHint": false,
                "openWorldHint": false
            },
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {
                "note": {"type": "string", "description": "prepare_removeで固定したノート ID"},
                "removal_token": {"type": "string", "description": "prepare_removeが返した5分間有効の対象固定token"}
            }, "required": ["note", "removal_token"]}
        }
    ]);
    let tools = definitions
        .as_array_mut()
        .expect("tool definitions are array");
    let insert_at = tools
        .iter()
        .position(|tool| tool["name"] == "plan_distillation")
        .expect("plan_distillation tool definition exists")
        + 1;
    let mut distillation_tools = vec![
        targeted_distillation_tool_definition(),
        distillation_audit_tool_definition(),
    ];
    distillation_tools.extend(distillation_cadence_tool_definitions());
    distillation_tools.extend(semantic_tool_definitions());
    distillation_tools.extend(initiative_closure_tool_definitions());
    distillation_tools.extend(legacy_promotion_tool_definitions());
    tools.splice(insert_at..insert_at, distillation_tools);
    if !capabilities.current_note_argument_optional {
        let get = definitions
            .as_array_mut()
            .and_then(|items| items.iter_mut().find(|item| item["name"] == "get"))
            .expect("get tool definition exists");
        get["description"] = json!(
            "ノート全文の取得(応答に添付と「近いノート」が付く)。search のヒットは必ず全文を読み、note IDを指定する。"
        );
        get["inputSchema"]["properties"]["note"]["description"] = json!("ノート ID(必須)");
        get["inputSchema"]["required"] = json!(["note"]);
    }
    definitions
        .as_array_mut()
        .expect("tool definitions are array")
        .retain(|definition| {
            definition["name"]
                .as_str()
                .is_some_and(|name| tool_surface.allows(name))
        });
    definitions
}

#[derive(Debug)]
struct ToolOutput {
    text: String,
    structured: Option<Value>,
}

/// 会話UIへそのまま渡せるノート同一性。本文用の`/notes/...`リンクを
/// 呼び出し側に組み立てさせると、作業directory基準の壊れたリンクになり得る。
fn note_conversation_identity(vault: &Vault, id: &str, title: &str) -> Result<Value> {
    let path = vault.note_path(id)?;
    Ok(json!({
        "note_id": id,
        "title": title,
        "conversation_link": path.to_string_lossy(),
    }))
}

fn authority_argument(args: &Value, required: bool) -> Result<Option<crate::authority::Authority>> {
    let Some(value) = args.get("authority") else {
        if required {
            anyhow::bail!("authority が必要");
        }
        return Ok(None);
    };
    serde_json::from_value(value.clone())
        .context("authority の形式が不正")
        .map(Some)
}

fn relations_argument(args: &Value) -> Result<Option<Vec<crate::authority::NoteRelation>>> {
    args.get("relations")
        .map(|value| serde_json::from_value(value.clone()).context("relations の形式が不正"))
        .transpose()
}

fn note_conversation_event(identity: &Value, event: &str) -> Value {
    json!({
        "type": "note_link",
        "event": event,
        "required": true,
        "note_id": identity["note_id"],
        "title": identity["title"],
        "conversation_link": identity["conversation_link"],
    })
}

fn conversation_events(
    note_event: Option<Value>,
    degraded: &[crate::degradation::Degradation],
) -> Value {
    let mut events = note_event.into_iter().collect::<Vec<_>>();
    events.extend(degraded.iter().map(|item| {
        json!({
            "type": "degradation",
            "required": true,
            "code": item.code(),
            "message": item.to_string(),
        })
    }));
    json!(events)
}

fn note_markdown_link(identity: &Value) -> String {
    let title = identity["title"]
        .as_str()
        .unwrap_or("無題")
        .replace('[', "\\[")
        .replace(']', "\\]");
    let link = identity["conversation_link"].as_str().unwrap_or_default();
    format!("[{title}](<{link}>)")
}

fn reject_unavailable_mcp_capabilities(client: &str, name: &str, args: &Value) -> Result<()> {
    let capabilities = ClientSurface::from_hint(client).capabilities();
    if name == "get"
        && !capabilities.current_note_argument_optional
        && args.get("note").and_then(Value::as_str).is_none()
    {
        anyhow::bail!("このclient surfaceではgetのnote引数が必要");
    }
    if matches!(name, "propose" | "update") && args.get("allow_new_tags").is_some() {
        anyhow::bail!(
            "allow_new_tags はAI用MCPでは利用できない。既存語彙を使うか、trusted UI / CLIの別承認を案内する"
        );
    }
    if matches!(
        name,
        "plan_distillation" | "distillation_cadence_status" | "plan_legacy_artifact_promotions"
    ) && args.as_object().is_none_or(|args| !args.is_empty())
    {
        anyhow::bail!("{name} は引数を受け取らない");
    }
    Ok(())
}

fn remote_degradations(
    enabled: bool,
    pull: impl FnOnce() -> Option<crate::degradation::Degradation>,
) -> Vec<crate::degradation::Degradation> {
    if enabled {
        pull().into_iter().collect()
    } else {
        Vec::new()
    }
}

#[cfg(test)]
fn call_tool(
    vault: &Vault,
    client: &str,
    name: &str,
    args: &Value,
    remote_sync: bool,
) -> Result<ToolOutput> {
    call_tool_with_search_options(
        vault,
        client,
        name,
        args,
        remote_sync,
        true,
        &mut RemovalPlans::default(),
    )
}

fn call_tool_with_search_options(
    vault: &Vault,
    client: &str,
    name: &str,
    args: &Value,
    remote_sync: bool,
    update_embeddings: bool,
    removal_plans: &mut RemovalPlans,
) -> Result<ToolOutput> {
    reject_unavailable_mcp_capabilities(client, name, args)?;
    let distillation_closed_world = matches!(
        name,
        "plan_distillation"
            | "plan_targeted_distillation"
            | "plan_initiative_closure"
            | "audit_distillation"
            | "distillation_cadence_status"
            | "run_distillation_cadence"
            | "plan_legacy_artifact_promotions"
    );
    let conflict_operation = matches!(
        name,
        "inspect_markdown_conflict" | "resolve_markdown_conflict"
    );
    // メッセージのやり取りの際に pull(複数デバイス同期・FR-A6 改定)。
    // スロットリング付き・失敗は劣化情報(fail-open)。自動retrievalの短命processは
    // remote_sync=falseで、発話ごとのKeychainアクセスとremote I/Oを行わない。
    let mut degraded = remote_degradations(
        remote_sync && !distillation_closed_world && !conflict_operation,
        || crate::connect::pull_if_stale(vault),
    );
    let conn = if conflict_operation {
        open_db_recovery(vault)?
    } else if distillation_closed_world {
        open_db_read_only(vault)?
    } else {
        open_db(vault)?
    };
    // DBが実行時正本なので、全文取得は検索時のsnapshotをそのまま読む。索引の追い付きと
    // 埋め込み生成は検索時に一度だけ行い、上位候補ごとのgetでは繰り返さない。
    if name == "search" {
        match sync_with_degradations(vault, &conn) {
            Ok(report) => {
                degraded.extend(report.degraded);
                if update_embeddings {
                    degraded.extend(crate::index::embed_step(&conn));
                }
            }
            Err(error) => degraded.push(crate::degradation::Degradation::IndexSync {
                detail: error.to_string(),
            }),
        }
    }
    match name {
        "plan_legacy_artifact_promotions" => {
            let workspace_id = crate::workspace::workspace_id(vault)?;
            let ledger = crate::ledger::Ledger::open(vault, &workspace_id)?;
            let plans = crate::migrate::plan_promotions(vault, &ledger)?;
            Ok(ToolOutput {
                text: format!(
                    "Legacy Artifact promotion plan(read-only): {}件。1件ずつapplyし、各件の直後に再監査する",
                    plans.len()
                ),
                structured: Some(json!({
                    "schema": "kb-app.legacy-artifact-promotion-plans/v1",
                    "read_only": true,
                    "plans": plans,
                })),
            })
        }
        "apply_legacy_artifact_promotion" => {
            let plan: crate::migrate::PromotionPlan = serde_json::from_value(args.clone())
                .context("Legacy Artifact promotion planを解釈できない")?;
            let workspace_id = crate::workspace::workspace_id(vault)?;
            let ledger = crate::ledger::Ledger::open(vault, &workspace_id)?;
            let result = crate::migrate::apply_promotion(
                vault,
                &ledger,
                &plan,
                &crate::frontmatter::now_iso(),
            )?;
            let mut structured = serde_json::to_value(&result)?;
            structured["degraded"] = serde_json::to_value(&degraded)?;
            structured["conversation_events"] = conversation_events(
                Some(json!({
                    "type": "legacy_artifact_promoted",
                    "event": "legacy_artifact_promoted",
                    "required": true,
                    "artifact_id": &result.artifact_id,
                    "plan_id": &result.plan_id,
                    "result_id": &result.result_id,
                    "old_path_retained": result.old_path_retained,
                    "already_applied": result.already_applied,
                })),
                &degraded,
            );
            Ok(ToolOutput {
                text: with_degradations(
                    format!(
                        "Legacy ArtifactをManagedへ昇格した: {} / version {} / 旧パス保持 {}",
                        result.artifact_id, result.after_version, result.old_path_retained
                    ),
                    &degraded,
                ),
                structured: Some(structured),
            })
        }
        "rollback_legacy_artifact_promotion" => {
            let applied: crate::migrate::PromotionResult = serde_json::from_value(args.clone())
                .context("Legacy Artifact promotion resultを解釈できない")?;
            let workspace_id = crate::workspace::workspace_id(vault)?;
            let ledger = crate::ledger::Ledger::open(vault, &workspace_id)?;
            let result = crate::migrate::rollback_promotion(
                vault,
                &ledger,
                &applied,
                &crate::frontmatter::now_iso(),
            )?;
            let mut structured = serde_json::to_value(&result)?;
            structured["degraded"] = serde_json::to_value(&degraded)?;
            structured["conversation_events"] = conversation_events(
                Some(json!({
                    "type": "legacy_artifact_promotion_rolled_back",
                    "event": "legacy_artifact_promotion_rolled_back",
                    "required": true,
                    "artifact_id": &result.artifact_id,
                    "plan_id": &result.plan_id,
                    "result_id": &result.result_id,
                    "old_path_retained": result.old_path_retained,
                })),
                &degraded,
            );
            Ok(ToolOutput {
                text: with_degradations(
                    format!(
                        "Legacy Artifact promotionをrollbackした: {} / version {} / 実体は削除していない",
                        result.artifact_id, result.after_version
                    ),
                    &degraded,
                ),
                structured: Some(structured),
            })
        }
        "inspect_markdown_conflict" => {
            let note = args
                .get("note")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("note が必要"))?;
            let conflict = vault.inspect_markdown_export_conflict(&conn, note)?;
            Ok(ToolOutput {
                text: format!(
                    "Markdown出力競合を検査した: {} / external {} / pending {}。内容を比較し、DB確定状態を採用する場合だけresolve_markdown_conflictを実行する",
                    conflict.note, conflict.markdown_hash, conflict.pending_document_hash
                ),
                structured: Some(serde_json::to_value(conflict)?),
            })
        }
        "resolve_markdown_conflict" => {
            let note = args
                .get("note")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("note が必要"))?;
            let strategy = args
                .get("strategy")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("strategy が必要"))?;
            if strategy != "keep_db" {
                anyhow::bail!("strategy v1はkeep_dbのみ");
            }
            let markdown_hash = args
                .get("markdown_hash")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("markdown_hash が必要"))?;
            let pending_document_hash =
                args.get("pending_document_hash")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("pending_document_hash が必要"))?;
            let report = vault.resolve_markdown_export_keep_db(
                &conn,
                note,
                markdown_hash,
                pending_document_hash,
            )?;
            let mut structured = serde_json::to_value(&report)?;
            structured["conversation_events"] = json!([{
                "type": "markdown_conflict_resolved",
                "event": "markdown_conflict_resolved",
                "required": true,
                "note_id": &report.note,
                "strategy": report.strategy,
                "pending_exports": report.pending_exports,
            }]);
            Ok(ToolOutput {
                text: format!(
                    "Markdown出力競合を解消した: {} / strategy keep_db / pending exports {}",
                    report.note, report.pending_exports
                ),
                structured: Some(structured),
            })
        }
        "plan_distillation" => {
            let plan = crate::distillation::plan(&conn)?;
            let counts = &plan.summary.operations;
            let text = format!(
                "蒸留plan(read-only): {} notes / {} actionable\nplan: {}\nsnapshot: {}\nkeep {} / normalize {} / revise {} / extract {} / split候補 {} / merge候補 {} / supersede候補 {} / unresolved {}\n",
                plan.snapshot.note_count,
                plan.summary.actionable,
                plan.plan_id,
                plan.snapshot.digest,
                counts.keep,
                counts.normalize,
                counts.revise,
                counts.extract,
                counts.split_canonical,
                counts.merge_candidate,
                counts.supersede_candidate,
                counts.unresolved,
            );
            Ok(ToolOutput {
                text,
                structured: Some(serde_json::to_value(&plan)?),
            })
        }
        "plan_targeted_distillation" => {
            let arguments: crate::distillation::TargetedDistillationArguments =
                serde_json::from_value(args.clone())
                    .context("plan_targeted_distillation引数を解釈できない")?;
            let plan = crate::distillation::plan_targeted(&conn, arguments)?;
            let text = format!(
                "対象指定蒸留plan(read-only): {} changes / plan {} / snapshot {}",
                plan.entries.len(),
                plan.plan_id,
                plan.snapshot.digest,
            );
            Ok(ToolOutput {
                text,
                structured: Some(serde_json::to_value(&plan)?),
            })
        }
        "plan_initiative_closure" => {
            let arguments: crate::initiative_lifecycle::InitiativeClosureArguments =
                serde_json::from_value(args.clone())
                    .context("plan_initiative_closure引数を解釈できない")?;
            let plan = crate::initiative_lifecycle::plan(&conn, arguments)?;
            let text = format!(
                "initiative完了plan(read-only): {} changes / plan {} / snapshot {}",
                plan.changes.len(),
                plan.plan_id,
                plan.snapshot.digest,
            );
            Ok(ToolOutput {
                text,
                structured: Some(serde_json::to_value(&plan)?),
            })
        }
        "audit_distillation" => {
            let arguments: crate::distillation_audit::DistillationAuditArguments =
                serde_json::from_value(args.clone())
                    .context("audit_distillation引数を解釈できない")?;
            let report =
                crate::distillation_audit::audit(vault, &conn, arguments.baseline.as_ref())?;
            let text = format!(
                "蒸留audit(read-only): gate {} / workset {} / added {} / changed {} / moved {} / removed {}\naudit: {}\nplan: {}\n",
                if report.gate.passed {
                    "PASS"
                } else {
                    "ATTENTION"
                },
                report.delta.workset.len(),
                report.delta.added.len(),
                report.delta.changed.len(),
                report.delta.moved.len(),
                report.delta.removed.len(),
                report.audit_id,
                report.plan.plan_id,
            );
            Ok(ToolOutput {
                text,
                structured: Some(serde_json::to_value(&report)?),
            })
        }
        "distillation_cadence_status" => {
            let status = crate::distillation_cadence::status(vault, &conn)?;
            let due = status
                .due_lanes()
                .iter()
                .map(|lane| lane.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Ok(ToolOutput {
                text: format!(
                    "蒸留cadence: due {} / current {} / accepted {}",
                    if due.is_empty() { "none" } else { &due },
                    status.current_checkpoint_id,
                    status.accepted_checkpoint_id.as_deref().unwrap_or("none"),
                ),
                structured: Some(serde_json::to_value(&status)?),
            })
        }
        "run_distillation_cadence" => {
            let arguments: crate::distillation_cadence::DistillationCadenceRunArguments =
                serde_json::from_value(args.clone())
                    .context("run_distillation_cadence引数を解釈できない")?;
            let report = crate::distillation_cadence::run(vault, &conn, arguments)?;
            let selected = report
                .selected_lanes
                .iter()
                .map(|lane| lane.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Ok(ToolOutput {
                text: format!(
                    "蒸留cadence: executed {} / accepted {} / lanes {} / review scope {}",
                    report.executed,
                    report.accepted,
                    if selected.is_empty() {
                        "none"
                    } else {
                        &selected
                    },
                    report.review_scope.len(),
                ),
                structured: Some(serde_json::to_value(&report)?),
            })
        }
        "apply_distillation" => {
            let request: crate::distillation_executor::DistillationExecutionRequest =
                serde_json::from_value(args.clone())
                    .context("apply_distillation引数を解釈できない")?;
            let report = crate::distillation_executor::execute(vault, &conn, request, client)?;
            if let Some(detail) = &report.markdown_export_error {
                degraded.push(crate::degradation::Degradation::MarkdownExport {
                    detail: detail.clone(),
                });
            }
            let mut structured = serde_json::to_value(&report)?;
            structured["degraded"] = serde_json::to_value(&degraded)?;
            structured["conversation_events"] = conversation_events(
                Some(json!({
                    "type": "distillation_applied",
                    "event": "distillation_applied",
                    "required": true,
                    "execution_id": &report.execution_id,
                    "plan_id": &report.plan_id,
                    "changed_notes": report.changes.len(),
                })),
                &degraded,
            );
            Ok(ToolOutput {
                text: with_degradations(
                    format!(
                        "蒸留waveを適用した: {}件 / execution {} / after snapshot {}",
                        report.changes.len(),
                        report.execution_id,
                        report.after_snapshot_digest
                    ),
                    &degraded,
                ),
                structured: Some(structured),
            })
        }
        "rollback_distillation" => {
            let execution_id = args
                .get("execution_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("execution_id が必要"))?;
            let report =
                crate::distillation_executor::rollback(vault, &conn, execution_id, client)?;
            if let Some(detail) = &report.markdown_export_error {
                degraded.push(crate::degradation::Degradation::MarkdownExport {
                    detail: detail.clone(),
                });
            }
            let mut structured = serde_json::to_value(&report)?;
            structured["degraded"] = serde_json::to_value(&degraded)?;
            structured["conversation_events"] = conversation_events(
                Some(json!({
                    "type": "distillation_rolled_back",
                    "event": "distillation_rolled_back",
                    "required": true,
                    "execution_id": &report.execution_id,
                    "rollback_id": &report.rollback_id,
                    "restored_notes": report.restored_notes.len(),
                })),
                &degraded,
            );
            Ok(ToolOutput {
                text: with_degradations(
                    format!(
                        "蒸留waveをrollbackした: {}件 / execution {} / snapshot {}",
                        report.restored_notes.len(),
                        report.execution_id,
                        report.after_snapshot_digest
                    ),
                    &degraded,
                ),
                structured: Some(structured),
            })
        }
        "apply_initiative_closure" => {
            let request: crate::initiative_lifecycle::InitiativeClosureExecutionRequest =
                serde_json::from_value(args.clone())
                    .context("apply_initiative_closure引数を解釈できない")?;
            let report = crate::initiative_lifecycle::execute(vault, &conn, request, client)?;
            if let Some(detail) = &report.markdown_export_error {
                degraded.push(crate::degradation::Degradation::MarkdownExport {
                    detail: detail.clone(),
                });
            }
            let mut structured = serde_json::to_value(&report)?;
            structured["degraded"] = serde_json::to_value(&degraded)?;
            structured["conversation_events"] = conversation_events(
                Some(json!({
                    "type": "initiative_closure_applied",
                    "event": "initiative_closure_applied",
                    "required": true,
                    "execution_id": &report.execution_id,
                    "plan_id": &report.plan_id,
                    "changed_notes": report.changes.len(),
                })),
                &degraded,
            );
            Ok(ToolOutput {
                text: with_degradations(
                    format!(
                        "initiative完了waveを適用した: {}件 / execution {} / after snapshot {}",
                        report.changes.len(),
                        report.execution_id,
                        report.after_snapshot_digest,
                    ),
                    &degraded,
                ),
                structured: Some(structured),
            })
        }
        "rollback_initiative_closure" => {
            let execution_id = args
                .get("execution_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("execution_id が必要"))?;
            let report = crate::initiative_lifecycle::rollback(vault, &conn, execution_id, client)?;
            if let Some(detail) = &report.markdown_export_error {
                degraded.push(crate::degradation::Degradation::MarkdownExport {
                    detail: detail.clone(),
                });
            }
            let mut structured = serde_json::to_value(&report)?;
            structured["degraded"] = serde_json::to_value(&degraded)?;
            structured["conversation_events"] = conversation_events(
                Some(json!({
                    "type": "initiative_closure_rolled_back",
                    "event": "initiative_closure_rolled_back",
                    "required": true,
                    "execution_id": &report.execution_id,
                    "rollback_id": &report.rollback_id,
                    "restored_notes": report.restored_notes.len(),
                })),
                &degraded,
            );
            Ok(ToolOutput {
                text: with_degradations(
                    format!(
                        "initiative完了waveをrollbackした: {}件 / execution {} / snapshot {}",
                        report.restored_notes.len(),
                        report.execution_id,
                        report.after_snapshot_digest,
                    ),
                    &degraded,
                ),
                structured: Some(structured),
            })
        }
        "search" => {
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(8) as usize;
            let any = args.get("any").and_then(|v| v.as_bool()).unwrap_or(false);
            let include_documents = args
                .get("include_documents")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            // Ranking・リンク展開・本文選択の間で別processの更新を挟まない。
            // include_documentsはこのread transactionの同じSQLite snapshotから組み立てる。
            let snapshot = conn.unchecked_transaction()?;
            let mut out = if any {
                crate::search::search_mode(&snapshot, query, limit, true)
            } else {
                search(&snapshot, query, limit)
            };
            out.degraded.extend(degraded);
            let retrieval = if include_documents {
                let hit_ids = out
                    .hits
                    .iter()
                    .map(|hit| hit.id.clone())
                    .collect::<Vec<_>>();
                let options = crate::retrieval::RetrievalOptions::default();
                match crate::retrieval::context_documents_for_query(
                    &snapshot, &hit_ids, query, options,
                ) {
                    Ok(bundle) => Some(bundle),
                    Err(error) => {
                        // リンク表だけが壊れても検索seed本文は返す。正常な0リンクとは
                        // ContextRetrieval degradationで区別する。
                        out.degraded
                            .push(crate::degradation::Degradation::ContextRetrieval {
                                detail: error.to_string(),
                            });
                        Some(crate::retrieval::context_documents_for_query(
                            &snapshot,
                            &hit_ids,
                            query,
                            crate::retrieval::RetrievalOptions {
                                max_depth: 0,
                                include_incoming: false,
                                ..options
                            },
                        )?)
                    }
                }
            } else {
                None
            };
            let mut text = String::new();
            if out.hits.is_empty() {
                text.push_str("該当なし。\n");
            }
            for h in &out.hits {
                text.push_str(&format!(
                    "- {} [{}]: {}\n",
                    h.id,
                    h.title.as_deref().unwrap_or("無題"),
                    h.snippet
                ));
            }
            if !out.related.is_empty() {
                text.push_str("関連(リンク1ホップ): ");
                let rel: Vec<String> = out
                    .related
                    .iter()
                    .map(|(id, t)| format!("{id} [{}]", t.as_deref().unwrap_or("無題")))
                    .collect();
                text.push_str(&rel.join(", "));
                text.push('\n');
            }
            text.push_str(&degradation_text(&out.degraded));
            let mut structured = serde_json::to_value(&out)?;
            if let Some(retrieval) = retrieval {
                structured["documents"] = serde_json::to_value(&retrieval.documents)?;
                structured["retrieval_candidates"] = serde_json::to_value(&retrieval.candidates)?;
                structured["retrieval"] = serde_json::to_value(&retrieval.stats)?;
            }
            structured["conversation_events"] = conversation_events(None, &out.degraded);
            snapshot.commit()?;
            Ok(ToolOutput {
                text,
                structured: Some(structured),
            })
        }
        "get" => {
            let id = match args.get("note").and_then(|v| v.as_str()) {
                Some(id) => id.to_string(),
                // 引数なし =「このノート」(アプリでいま開いているノート。FR-A5 の文脈受け渡し)
                None => crate::connect::current_note(vault).ok_or_else(|| {
                    anyhow::anyhow!("いま開いているノートが無い(note 引数で ID を指定)")
                })?,
            };
            let note = vault.read_note_from_db(&conn, &id)?;
            let title = note.front.title.as_deref().unwrap_or("無題");
            let identity = note_conversation_identity(vault, &id, title)?;
            let attachments = vault.list_attachments(&id)?;
            let workspace_id = crate::workspace::workspace_id(vault)?;
            let stores = crate::store::Stores::open(&workspace_id)?;
            let ledger = crate::ledger::Ledger::open(vault, &workspace_id)?;
            let managed = ledger.list_for_note(&id);
            let legacy_line = if attachments.is_empty() {
                String::new()
            } else {
                format!(
                    "(旧添付: {} — 本文からは /{id}.files/<名前> で参照)\n",
                    attachments
                        .iter()
                        .map(|(n, _)| n.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            let managed_line = if managed.is_empty() {
                String::new()
            } else {
                let rows = managed
                    .iter()
                    .map(|manifest| {
                        let reference = ledger
                            .ref_for(&manifest.id)
                            .map(|r| format!(", ref={}", r.name))
                            .unwrap_or_default();
                        let availability = crate::store::availability(vault, &stores, manifest);
                        format!(
                            "{} [artifact={}, v={}, {}, {} bytes, role={:?}, {:?}/{:?}, {:?}{}]",
                            manifest.display_name,
                            manifest.id,
                            manifest.version,
                            manifest.created.media_type,
                            manifest.created.size,
                            manifest.role,
                            manifest.policy.sensitivity,
                            manifest.policy.sync,
                            availability,
                            reference
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("(ファイル: {rows})\n")
            };
            // 関連を決めるのは AI(2026-08-11 方針)。判断材料として近いノートを添える
            let similar = match crate::search::similar_notes(&conn, &id, 5) {
                Ok(similar) => similar,
                Err(error) => {
                    degraded.push(crate::degradation::Degradation::SimilarNotes {
                        detail: error.to_string(),
                    });
                    Vec::new()
                }
            };
            let sim_line = if similar.is_empty() {
                String::new()
            } else {
                format!(
                    "(近いノート — まだリンクされていない: {})\n",
                    similar
                        .iter()
                        .map(|(sid, t, d)| format!(
                            "{sid}[{}] {d:.2}",
                            t.as_deref().unwrap_or("無題")
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            let artifact_rows = managed
                .iter()
                .map(|manifest| {
                    let reference = ledger.ref_for(&manifest.id).map(|r| r.name.to_string());
                    json!({
                        "artifact_id": manifest.id,
                        "version": manifest.version,
                        "display_name": manifest.display_name,
                        "media_type": manifest.created.media_type,
                        "size": manifest.created.size,
                        "role": manifest.role,
                        "policy": manifest.policy,
                        "ref_name": reference,
                        "availability": crate::store::availability(vault, &stores, manifest),
                    })
                })
                .collect::<Vec<_>>();
            let legacy_names = attachments
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>();
            let note_text = note.to_file_string()?;
            let note_description = note.front.description.clone();
            let note_tags = note.front.tags.clone();
            let note_status = note.front.effective_status().to_string();
            let note_origin = note.front.origin.clone();
            let note_uid = note.front.note_uid.clone();
            let note_authority = note.front.authority.clone();
            let note_relations = note.front.relations.clone();
            let note_body = note.body.clone();
            let link_text = note_markdown_link(&identity);
            Ok(ToolOutput {
                text: format!(
                    "(note: {id})\nリンク: {link_text}\n{legacy_line}{managed_line}{sim_line}{}{}",
                    degradation_text(&degraded),
                    note_text
                ),
                structured: Some(json!({
                    "note": &id,
                    "note_id": identity["note_id"],
                    "title": identity["title"],
                    "description": note_description,
                    "tags": note_tags,
                    "status": note_status,
                    "origin": note_origin,
                    "note_uid": note_uid,
                    "authority": note_authority,
                    "relations": note_relations,
                    "body": note_body,
                    "conversation_link": identity["conversation_link"],
                    "artifacts": artifact_rows,
                    "legacy_attachments": legacy_names,
                    "degraded": degraded,
                    "conversation_events": conversation_events(
                        Some(note_conversation_event(&identity, "note_read")),
                        &degraded,
                    ),
                })),
            })
        }
        "attach" => {
            let note_id = args
                .get("note")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("note が必要"))?;
            let file_name = args
                .get("file_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("file_name が必要"))?;
            let content = decode_attachment_content(args)?;
            let ref_name = args
                .get("ref_name")
                .and_then(|v| v.as_str())
                .map(crate::artifact::RefName::from_str)
                .transpose()?;

            let workspace_id = crate::workspace::workspace_id(vault)?;
            let stores = crate::store::Stores::open(&workspace_id)?;
            let ledger = crate::ledger::Ledger::open(vault, &workspace_id)?;
            let taken = crate::intake::take_content(
                vault,
                &stores,
                &ledger,
                &workspace_id,
                crate::intake::ContentRequest {
                    note_id,
                    display_name: file_name,
                    content: &content,
                    ref_name,
                    client,
                },
            )?;
            let reference = taken
                .artifact_ref
                .as_ref()
                .map(|artifact_ref| artifact_ref.name.as_str());
            let availability = crate::store::availability(vault, &stores, &taken.manifest);
            let note = vault.read_note_from_db(&conn, note_id)?;
            let identity = note_conversation_identity(
                vault,
                note_id,
                note.front.title.as_deref().unwrap_or("無題"),
            )?;
            let structured = json!({
                "note": note_id,
                "note_id": identity["note_id"],
                "title": identity["title"],
                "conversation_link": identity["conversation_link"],
                "file_name": taken.manifest.display_name,
                "artifact_id": taken.manifest.id,
                "version": taken.manifest.version,
                "media_type": taken.manifest.created.media_type,
                "size": taken.manifest.created.size,
                "role": taken.manifest.role,
                "policy": taken.manifest.policy,
                "ref_name": reference,
                "locator": "managed",
                "availability": availability,
                "delivery": taken.delivery,
                "warn_over_bytes": taken.warn_over,
                "degraded": degraded,
                "conversation_events": conversation_events(
                    Some(note_conversation_event(&identity, "artifact_attached")),
                    &degraded,
                ),
            });
            Ok(ToolOutput {
                text: with_degradations(
                    format!(
                        "添付した: {} → {} (artifact {})",
                        file_name,
                        note_markdown_link(&identity),
                        taken.manifest.id
                    ),
                    &degraded,
                ),
                structured: Some(structured),
            })
        }
        "recent" => {
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let hits = recent(&conn, limit)?;
            let text = hits
                .iter()
                .map(|h| {
                    format!(
                        "- {} [{}]: {}",
                        h.id,
                        h.title.as_deref().unwrap_or("無題"),
                        h.snippet
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok(ToolOutput {
                text: with_degradations(text, &degraded),
                structured: Some(json!({
                    "hits": hits,
                    "degraded": degraded,
                    "conversation_events": conversation_events(None, &degraded),
                })),
            })
        }
        "propose" => {
            let title = args
                .get("title")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("title が必要"))?;
            let body = args
                .get("body")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("body が必要"))?;
            let description = args.get("description").and_then(|v| v.as_str());
            let tags: Vec<String> = args
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            // 語彙外タグは**書く前に**弾く(契約1の強制点)。以前は起票後に
            // 「新しいタグを導入した」と教えるだけだったため語彙が膨らみ続けた
            // (60ノートに112語・1回きり61%)。2026-08-12 に事前拒否へ引き上げ。
            let id = vault.propose(
                &conn,
                NoteProposal {
                    title,
                    body,
                    description,
                    tags: &tags,
                    authority: authority_argument(args, true)?.expect("required above"),
                    relations: relations_argument(args)?.unwrap_or_default(),
                    allow_new_tags: false,
                    client,
                },
            )?;
            let mut structured = note_conversation_identity(vault, &id, title)?;
            let created = vault.read_note_from_db(&conn, &id)?;
            structured["note_uid"] = serde_json::to_value(&created.front.note_uid)?;
            structured["authority"] = serde_json::to_value(&created.front.authority)?;
            structured["relations"] = serde_json::to_value(&created.front.relations)?;
            structured["event"] = json!("note_created");
            structured["degraded"] = serde_json::to_value(&degraded)?;
            structured["conversation_events"] = conversation_events(
                Some(note_conversation_event(&structured, "note_created")),
                &degraded,
            );
            Ok(ToolOutput {
                text: with_degradations(
                    format!("起票した: {}", note_markdown_link(&structured)),
                    &degraded,
                ),
                structured: Some(structured),
            })
        }
        "update" => {
            let id = args
                .get("note")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("note が必要"))?;
            let tags: Option<Vec<String>> = args.get("tags").and_then(|v| v.as_array()).map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str().map(String::from))
                    .collect()
            });
            vault.agent_update_note(
                &conn,
                NoteUpdate {
                    id,
                    title: args.get("title").and_then(|v| v.as_str()),
                    body: args.get("body").and_then(|v| v.as_str()),
                    description: args.get("description").and_then(|v| v.as_str()),
                    tags: tags.as_deref(),
                    authority: authority_argument(args, false)?,
                    relations: relations_argument(args)?,
                    allow_new_tags: false,
                    client,
                },
            )?;
            let note = vault.read_note_from_db(&conn, id)?;
            let mut structured = note_conversation_identity(
                vault,
                id,
                note.front.title.as_deref().unwrap_or("無題"),
            )?;
            structured["note_uid"] = serde_json::to_value(&note.front.note_uid)?;
            structured["authority"] = serde_json::to_value(&note.front.authority)?;
            structured["relations"] = serde_json::to_value(&note.front.relations)?;
            structured["event"] = json!("note_updated");
            structured["degraded"] = serde_json::to_value(&degraded)?;
            structured["conversation_events"] = conversation_events(
                Some(note_conversation_event(&structured, "note_updated")),
                &degraded,
            );
            Ok(ToolOutput {
                text: with_degradations(
                    format!("更新した: {}", note_markdown_link(&structured)),
                    &degraded,
                ),
                structured: Some(structured),
            })
        }
        "prepare_remove" => {
            let id = args
                .get("note")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("note が必要"))?;
            let note = vault.agent_removal_candidate(&conn, id)?;
            let title = note.front.title.as_deref().unwrap_or("無題");
            let reason = args
                .get("reason")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("reason が必要"))?;
            let token = removal_plans.prepare(id, title, reason, &note.to_file_string()?)?;
            Ok(ToolOutput {
                text: with_degradations(
                    format!(
                        "削除準備: {id} [{title}]。理由: {}。まだ削除していない。メンテナンス方針の範囲内なら5分以内にcommit_removeを実行できる",
                        reason.trim()
                    ),
                    &degraded,
                ),
                structured: Some(json!({
                    "note_id": id,
                    "title": title,
                    "reason": reason.trim(),
                    "removal_token": token,
                    "expires_in_seconds": REMOVAL_TOKEN_TTL.as_secs(),
                    "event": "removal_prepared",
                    "degraded": degraded,
                    "conversation_events": conversation_events(Some(json!({
                        "type": "removal_prepared",
                        "event": "removal_prepared",
                        "required": true,
                        "note_id": id,
                        "title": title,
                        "reason": reason.trim(),
                        "expires_in_seconds": REMOVAL_TOKEN_TTL.as_secs(),
                    })), &degraded),
                })),
            })
        }
        "commit_remove" => {
            let id = args
                .get("note")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("note が必要"))?;
            let token = args
                .get("removal_token")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("removal_token が必要"))?;
            let pending = removal_plans.consume(token, id)?;
            let note = vault.agent_removal_candidate(&conn, id)?;
            pending.require_unchanged(&note.to_file_string()?)?;
            vault.agent_delete_note(&conn, id, &pending.reason, client)?;
            Ok(ToolOutput {
                text: with_degradations(
                    format!("削除した: {id} [{}] (履歴には残る)", pending.title),
                    &degraded,
                ),
                structured: Some(json!({
                    "note_id": id,
                    "title": pending.title,
                    "reason": pending.reason,
                    "event": "note_removed",
                    "degraded": degraded,
                    "conversation_events": conversation_events(Some(json!({
                        "type": "note_removed",
                        "event": "note_removed",
                        "required": true,
                        "note_id": id,
                        "title": pending.title,
                    })), &degraded),
                })),
            })
        }
        other => anyhow::bail!("unknown tool: {other}"),
    }
}

fn decode_attachment_content(args: &Value) -> Result<Vec<u8>> {
    let encoded = args
        .get("content_base64")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("content_base64 が必要"))?;
    let max_encoded = crate::intake::MCP_CONTENT_MAX_BYTES.div_ceil(3) * 4;
    if encoded.len() > max_encoded {
        return Err(crate::artifact::ArtifactError::TooLarge {
            // decode前なので厳密値は未確定。上限超過を示す最小値を返す。
            size: (crate::intake::MCP_CONTENT_MAX_BYTES + 1) as u64,
            limit: crate::intake::MCP_CONTENT_MAX_BYTES as u64,
        }
        .into());
    }
    let content = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("content_base64 の形式が不正")?;
    if content.len() > crate::intake::MCP_CONTENT_MAX_BYTES {
        return Err(crate::artifact::ArtifactError::TooLarge {
            size: content.len() as u64,
            limit: crate::intake::MCP_CONTENT_MAX_BYTES as u64,
        }
        .into());
    }
    Ok(content)
}

fn degradation_text(degraded: &[crate::degradation::Degradation]) -> String {
    degraded
        .iter()
        .map(|item| format!("⚠ 劣化 [{}]: {item}\n", item.code()))
        .collect()
}

fn with_degradations(mut text: String, degraded: &[crate::degradation::Degradation]) -> String {
    if degraded.is_empty() {
        return text;
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&degradation_text(degraded));
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_initialize_keeps_tools_visible_with_only_the_no_bypass_rule() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let initialized = handle(
            Some(&vault),
            "test/client",
            false,
            true,
            "initialize",
            Some(&serde_json::json!({"protocolVersion": "test"})),
        )
        .unwrap()
        .unwrap();
        assert_eq!(initialized["capabilities"]["tools"], serde_json::json!({}));
        assert_eq!(
            initialized["capabilities"]["experimental"]["kbApp"]["client_surface"],
            "unknown"
        );
        assert_eq!(initialized["instructions"], DISABLED_INSTRUCTIONS);
        assert!(
            initialized["instructions"]
                .as_str()
                .unwrap()
                .contains(KB_DISABLED_CODE)
        );
        assert!(
            !initialized["instructions"]
                .as_str()
                .unwrap()
                .contains("~/kb")
        );

        let tools = handle(Some(&vault), "test/client", false, true, "tools/list", None)
            .unwrap()
            .unwrap();
        let prompts = handle(
            Some(&vault),
            "test/client",
            false,
            true,
            "prompts/list",
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            tools,
            serde_json::json!({"tools": tool_definitions("test/client")})
        );
        assert_eq!(prompts, serde_json::json!({"prompts": []}));
    }

    #[test]
    fn disabled_tool_call_stops_before_index_or_vault_work() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let index = vault.index_db_path();
        assert!(!index.exists());

        let response = handle(
            Some(&vault),
            "test/client",
            false,
            true,
            "tools/call",
            Some(&serde_json::json!({
                "name": "search",
                "arguments": {"query": "anything"}
            })),
        )
        .unwrap()
        .unwrap();
        assert_eq!(response, disabled_tool_result());
        assert_eq!(response["structuredContent"]["code"], KB_DISABLED_CODE);
        assert_eq!(response["structuredContent"]["authoritative"], true);
        assert_eq!(response["structuredContent"]["retryable"], false);
        assert_eq!(response["structuredContent"]["data"], serde_json::json!([]));
        assert!(!index.exists());
    }

    #[test]
    fn disabled_tool_calls_do_not_reveal_tool_or_argument_existence() {
        let expected = disabled_tool_result();
        for params in [
            serde_json::json!({"name": "search", "arguments": {"query": "known"}}),
            serde_json::json!({"name": "get", "arguments": {"note": "notes/secret"}}),
            serde_json::json!({"name": "plan_legacy_artifact_promotions", "arguments": {}}),
            serde_json::json!({"name": "apply_legacy_artifact_promotion", "arguments": {"plan_id": "secret"}}),
            serde_json::json!({"name": "rollback_legacy_artifact_promotion", "arguments": {"result_id": "secret"}}),
            serde_json::json!({"name": "unknown", "arguments": {"anything": true}}),
            serde_json::json!({}),
        ] {
            let actual = handle(
                None,
                "test/client",
                false,
                true,
                "tools/call",
                Some(&params),
            )
            .unwrap()
            .unwrap();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn disabled_control_plane_never_opens_the_vault() {
        let search = serde_json::json!({"name": "search"});
        for method in ["initialize", "tools/list", "prompts/list", "tools/call"] {
            assert!(!method_needs_vault(
                false,
                method,
                ToolSurface::All,
                Some(&search)
            ));
        }
        assert!(!method_needs_vault(
            true,
            "initialize",
            ToolSurface::All,
            Some(&search)
        ));
        assert!(method_needs_vault(
            true,
            "tools/call",
            ToolSurface::All,
            Some(&search)
        ));
    }

    #[test]
    fn enabled_initialize_keeps_the_existing_tools_and_instructions() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let initialized = handle(Some(&vault), "test/client", true, true, "initialize", None)
            .unwrap()
            .unwrap();
        assert_eq!(initialized["capabilities"]["tools"], serde_json::json!({}));
        assert_eq!(
            initialized["capabilities"]["prompts"],
            serde_json::json!({})
        );
        assert_eq!(initialized["instructions"], instructions_for("test/client"));
    }

    #[test]
    fn evaluation_transform_injects_only_selected_context_and_event_rules() {
        let options = EvaluationServeOptions {
            instructions: "fixture instructions".into(),
            event_rules: vec![crate::rule_delivery_eval::PreparedEventRule {
                rule_id: "event.note-link".into(),
                after_tools: vec!["get".into()],
                instruction: "structured linkを回答する".into(),
            }],
            injected_degradations: vec!["index_sync:fixture".into()],
            trace_path: None,
        };
        let mut initialized = serde_json::json!({"instructions": "production"});
        apply_evaluation_transform(&options, "initialize", None, &mut initialized);
        assert_eq!(initialized["instructions"], "fixture instructions");

        let mut searched = serde_json::json!({
            "content": [{"type": "text", "text": "該当なし。"}],
            "structuredContent": {"hits": []}
        });
        apply_evaluation_transform(
            &options,
            "tools/call",
            Some(&serde_json::json!({"name": "search"})),
            &mut searched,
        );
        assert!(
            searched["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("劣化")
        );
        assert_eq!(
            searched["structuredContent"]["eval_injected_degradations"][0],
            "index_sync:fixture"
        );
        assert!(searched["structuredContent"].get("event_rules").is_none());

        let mut fetched = serde_json::json!({
            "content": [{"type": "text", "text": "note"}],
            "structuredContent": {"note_id": "notes/a"}
        });
        apply_evaluation_transform(
            &options,
            "tools/call",
            Some(&serde_json::json!({"name": "get"})),
            &mut fetched,
        );
        assert_eq!(
            fetched["structuredContent"]["event_rules"][0]["rule_id"],
            "event.note-link"
        );
    }

    #[test]
    fn evaluation_search_does_not_advance_or_report_the_embedding_queue() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        vault
            .propose_for_test(
                "評価ノート",
                "固定fixture本文",
                None,
                &["eval".into()],
                "rule-delivery-eval/fixture",
            )
            .unwrap();

        let output = call_tool_with_search_options(
            &vault,
            "rule-delivery-eval/test",
            "search",
            &serde_json::json!({"query": "固定fixture"}),
            false,
            false,
            &mut RemovalPlans::default(),
        )
        .unwrap();
        let structured = output.structured.unwrap();
        assert_eq!(structured["degraded"], serde_json::json!([]));
    }

    #[test]
    fn mcp_degradation_keeps_the_stable_code() {
        let text = degradation_text(&[crate::degradation::Degradation::SimilarNotes {
            detail: "db locked".into(),
        }]);
        assert!(text.contains("[similar_notes]"), "{text}");
        assert!(text.contains("db locked"), "{text}");
    }

    #[test]
    fn local_only_mode_does_not_invoke_remote_pull() {
        let called = std::cell::Cell::new(false);
        let degraded = remote_degradations(false, || {
            called.set(true);
            Some(crate::degradation::Degradation::RemoteSync {
                detail: "should not run".into(),
            })
        });

        assert!(!called.get());
        assert!(degraded.is_empty());
        assert!(ServeOptions::default().remote_sync);
        assert_eq!(ServeOptions::default().tool_surface, ToolSurface::All);
    }

    #[test]
    fn split_surfaces_publish_only_their_tools() {
        let cases = [
            (ToolSurface::Read, vec!["search", "get", "recent"]),
            (
                ToolSurface::Write,
                vec![
                    "attach",
                    "propose",
                    "update",
                    "prepare_remove",
                    "commit_remove",
                ],
            ),
            (
                ToolSurface::Maintenance,
                vec![
                    "inspect_markdown_conflict",
                    "resolve_markdown_conflict",
                    "plan_distillation",
                    "plan_targeted_distillation",
                    "audit_distillation",
                    "distillation_cadence_status",
                    "run_distillation_cadence",
                    "apply_distillation",
                    "rollback_distillation",
                    "plan_initiative_closure",
                    "apply_initiative_closure",
                    "rollback_initiative_closure",
                    "plan_legacy_artifact_promotions",
                    "apply_legacy_artifact_promotion",
                    "rollback_legacy_artifact_promotion",
                ],
            ),
        ];

        for (surface, expected) in cases {
            let listed = handle_on_surface(
                None,
                "test/client",
                true,
                false,
                surface,
                "tools/list",
                None,
            )
            .unwrap()
            .unwrap();
            let names = listed["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|tool| tool["name"].as_str().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(names, expected, "unexpected tools on {surface:?}");
        }
    }

    #[test]
    fn split_surface_rejects_hidden_tool_before_opening_vault() {
        let response = handle_on_surface(
            None,
            "test/client",
            true,
            false,
            ToolSurface::Read,
            "tools/call",
            Some(&serde_json::json!({
                "name": "propose",
                "arguments": {"title": "must not run"}
            })),
        )
        .unwrap()
        .unwrap();

        assert_eq!(response["isError"], true);
        assert_eq!(
            response["structuredContent"]["code"],
            "tool_surface_mismatch"
        );
        assert_eq!(response["structuredContent"]["authoritative"], true);
        assert_eq!(response["structuredContent"]["retryable"], false);
        assert_eq!(response["structuredContent"]["surface"], "kb-app-read");
        assert!(!method_needs_vault(
            true,
            "tools/call",
            ToolSurface::Read,
            Some(&serde_json::json!({"name": "propose"})),
        ));
        assert!(method_needs_vault(
            true,
            "tools/call",
            ToolSurface::Read,
            Some(&serde_json::json!({"name": "search"})),
        ));
    }

    #[test]
    fn split_surface_identifies_itself_during_initialize() {
        let initialized = handle_on_surface(
            None,
            "test/client",
            true,
            false,
            ToolSurface::Write,
            "initialize",
            Some(&serde_json::json!({"protocolVersion": "2025-06-18"})),
        )
        .unwrap()
        .unwrap();

        assert_eq!(initialized["serverInfo"]["name"], "kb-app-write");
    }

    #[test]
    fn mcp_get_update_and_two_phase_remove_cannot_escape_the_vault() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let secret = outside.join("secret.md");
        std::fs::write(&secret, "TOP SECRET").unwrap();

        for (tool, args) in [
            ("get", serde_json::json!({"note": "../outside/secret"})),
            (
                "update",
                serde_json::json!({"note": "../outside/secret", "body": "侵入"}),
            ),
            (
                "prepare_remove",
                serde_json::json!({"note": "../outside/secret", "reason": "境界テスト"}),
            ),
            (
                "commit_remove",
                serde_json::json!({
                    "note": "../outside/secret",
                    "removal_token": "invalid"
                }),
            ),
        ] {
            assert!(call_tool(&vault, "test/client", tool, &args, true).is_err());
            assert_eq!(std::fs::read_to_string(&secret).unwrap(), "TOP SECRET");
        }
    }

    #[test]
    fn mcp_does_not_implicitly_import_an_external_markdown_edit() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        std::fs::write(vault.root.join("notes/broken.md"), "frontmatterではない").unwrap();

        let output = call_tool(
            &vault,
            "test/client",
            "search",
            &serde_json::json!({"query": "anything"}),
            true,
        )
        .unwrap();
        assert!(!output.text.contains("notes/broken"), "{}", output.text);
        assert_eq!(crate::note_store::pending_count(&conn).unwrap(), 0);
        assert!(output.structured.is_some());
    }

    #[test]
    fn mcp_get_reads_the_db_document_even_when_markdown_differs() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let id = vault
            .propose_for_test(
                "DB本文",
                "AIへ返す本文",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        std::fs::write(vault.note_path(&id).unwrap(), "壊れたMarkdown").unwrap();

        let output = call_tool(
            &vault,
            "test/client",
            "get",
            &serde_json::json!({"note": id}),
            false,
        )
        .unwrap();

        assert!(output.text.contains("AIへ返す本文"), "{}", output.text);
        assert!(!output.text.contains("壊れたMarkdown"), "{}", output.text);
    }

    #[test]
    fn mcp_resolves_only_the_inspected_markdown_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let id = vault
            .propose_for_test("競合解消", "更新前", None, &["test".into()], "test/client")
            .unwrap();
        let path = vault.note_path(&id).unwrap();
        let mut external = std::fs::read_to_string(&path).unwrap();
        external.push_str("\n外部編集\n");
        std::fs::write(&path, external).unwrap();

        let failed = call_tool(
            &vault,
            "test/client",
            "update",
            &serde_json::json!({"note": id, "body": "DB確定本文"}),
            false,
        )
        .unwrap_err();
        assert!(failed.to_string().contains("外部編集"));

        // 旧DB migrationの途中状態でも、競合解消操作はMarkdown importより先に
        // outboxを検査できなければならない。
        let conn = open_db_recovery(&vault).unwrap();
        conn.execute("DELETE FROM meta WHERE key = 'runtime_store'", [])
            .unwrap();

        let inspected = call_tool(
            &vault,
            "test/client",
            "inspect_markdown_conflict",
            &serde_json::json!({"note": id}),
            true,
        )
        .unwrap()
        .structured
        .unwrap();
        assert!(
            inspected["markdown_document"]
                .as_str()
                .unwrap()
                .contains("外部編集")
        );
        assert!(
            inspected["pending_document"]
                .as_str()
                .unwrap()
                .contains("DB確定本文")
        );

        let stale = call_tool(
            &vault,
            "test/client",
            "resolve_markdown_conflict",
            &serde_json::json!({
                "note": id,
                "strategy": "keep_db",
                "markdown_hash": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "pending_document_hash": inspected["pending_document_hash"],
            }),
            true,
        )
        .unwrap_err();
        assert!(stale.to_string().contains("検査後"));

        let resolved = call_tool(
            &vault,
            "test/client",
            "resolve_markdown_conflict",
            &serde_json::json!({
                "note": id,
                "strategy": "keep_db",
                "markdown_hash": inspected["markdown_hash"],
                "pending_document_hash": inspected["pending_document_hash"],
            }),
            true,
        )
        .unwrap()
        .structured
        .unwrap();
        assert_eq!(resolved["strategy"], "keep_db");
        assert_eq!(resolved["pending_exports"], 0);
        let runtime_store: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'runtime_store'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(runtime_store, "db-v1");
        assert!(
            std::fs::read_to_string(path)
                .unwrap()
                .contains("DB確定本文")
        );
    }

    #[test]
    fn note_events_return_a_conversation_ready_identity() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        vault
            .propose_for_test(
                "語彙seed",
                "evalタグを既存語彙にする。",
                None,
                &["eval".into()],
                "test/client",
            )
            .unwrap();

        let proposed = call_tool(
            &vault,
            "test/client",
            "propose",
            &serde_json::json!({
                "title": "評価ノート",
                "body": "初期本文",
                "tags": ["eval"],
                "authority": {
                    "namespace": "knowledge",
                    "role": "canonical",
                    "status": "active",
                    "scope": "test/evaluation-note"
                }
            }),
            false,
        )
        .unwrap()
        .structured
        .unwrap();
        let note_id = proposed["note_id"].as_str().unwrap();
        let expected_link = vault.note_path(note_id).unwrap();

        assert_eq!(proposed["title"], "評価ノート");
        assert_eq!(proposed["event"], "note_created");
        assert!(proposed["note_uid"].as_str().is_some());
        assert_eq!(proposed["authority"]["namespace"], "knowledge");
        assert_eq!(proposed["conversation_events"][0]["type"], "note_link");
        assert_eq!(proposed["conversation_events"][0]["event"], "note_created");
        assert_eq!(proposed["conversation_events"][0]["required"], true);
        assert_eq!(
            proposed["conversation_link"].as_str(),
            expected_link.to_str()
        );
        assert!(expected_link.is_absolute());

        let fetched = call_tool(
            &vault,
            "test/client",
            "get",
            &serde_json::json!({"note": note_id}),
            false,
        )
        .unwrap()
        .structured
        .unwrap();
        assert_eq!(fetched["note_id"], note_id);
        assert_eq!(fetched["title"], "評価ノート");
        assert_eq!(fetched["body"], "初期本文\n");
        assert_eq!(fetched["tags"], serde_json::json!(["eval"]));
        assert_eq!(fetched["status"], "stable");
        assert_eq!(fetched["conversation_link"], proposed["conversation_link"]);
        assert_eq!(fetched["conversation_events"][0]["event"], "note_read");

        let updated = call_tool(
            &vault,
            "test/client",
            "update",
            &serde_json::json!({
                "note": note_id,
                "title": "更新済み評価ノート",
                "body": "更新本文"
            }),
            false,
        )
        .unwrap()
        .structured
        .unwrap();
        assert_eq!(updated["note_id"], note_id);
        assert_eq!(updated["title"], "更新済み評価ノート");
        assert_eq!(updated["event"], "note_updated");
        assert_eq!(updated["conversation_link"], proposed["conversation_link"]);
        assert_eq!(updated["conversation_events"][0]["event"], "note_updated");
    }

    #[test]
    fn search_exposes_structured_hits_for_mcp_retrieval_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        vault
            .propose_for_test(
                "認証の設計",
                "認証と監査の設計判断。",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();

        let result = handle(
            Some(&vault),
            "test/client",
            true,
            true,
            "tools/call",
            Some(&serde_json::json!({
                "name": "search",
                "arguments": {
                    "query": "認証 監査",
                    "limit": 3,
                    "any": true,
                    "include_documents": true
                }
            })),
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            result["structuredContent"]["hits"][0]["title"],
            "認証の設計"
        );
        assert_eq!(
            result["structuredContent"]["hits"][0]["id"],
            "notes/認証の設計"
        );
        assert!(
            result["structuredContent"]["documents"][0]["text"]
                .as_str()
                .unwrap()
                .contains("認証と監査の設計判断")
        );
        assert_eq!(result["structuredContent"]["retrieval"]["seed_count"], 1);
        assert_eq!(
            result["structuredContent"]["documents"][0]["source"],
            "search"
        );
    }

    #[test]
    fn search_documents_follow_outgoing_links_for_two_hops_in_one_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let deep = vault
            .propose_for_test(
                "三段目",
                "検索語を含まない深い補足。",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let direct = vault
            .propose_for_test(
                "二段目",
                &format!("検索語を含まない直接補足。[次](/{}.md)", deep),
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let root = vault
            .propose_for_test(
                "連鎖取得の起点",
                &format!("固有番兵ネビュラ。[関連](/{}.md)", direct),
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();

        let result = handle(
            Some(&vault),
            "test/client",
            true,
            true,
            "tools/call",
            Some(&serde_json::json!({
                "name": "search",
                "arguments": {
                    "query": "固有番兵ネビュラ",
                    "limit": 5,
                    "any": true,
                    "include_documents": true
                }
            })),
        )
        .unwrap()
        .unwrap();

        let documents = result["structuredContent"]["documents"].as_array().unwrap();
        assert_eq!(
            documents
                .iter()
                .map(|document| document["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [root.as_str(), direct.as_str(), deep.as_str()]
        );
        assert_eq!(documents[0]["depth"], 0);
        assert_eq!(documents[1]["source"], "outgoing_link");
        assert_eq!(documents[1]["depth"], 1);
        assert_eq!(documents[2]["depth"], 2);
        assert_eq!(
            result["structuredContent"]["retrieval"]["candidate_count"],
            3
        );
        assert_eq!(
            result["structuredContent"]["retrieval"]["selected_count"],
            3
        );
    }

    #[test]
    fn broken_link_graph_falls_back_to_search_documents_with_typed_degradation() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        vault
            .propose_for_test(
                "検索seed",
                "固有番兵フォールバック。",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let conn = open_db(&vault).unwrap();
        conn.execute_batch(
            "DROP TABLE links;
             CREATE TABLE links(dst TEXT);
             CREATE INDEX links_dst ON links(dst);",
        )
        .unwrap();

        let output = call_tool(
            &vault,
            "test/client",
            "search",
            &serde_json::json!({
                "query": "固有番兵フォールバック",
                "limit": 5,
                "any": true,
                "include_documents": true
            }),
            false,
        )
        .unwrap();
        let structured = output.structured.unwrap();
        assert_eq!(structured["documents"].as_array().unwrap().len(), 1);
        assert!(
            structured["degraded"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["code"] == "context_retrieval")
        );
    }

    #[test]
    fn attach_schema_accepts_content_but_never_a_client_path() {
        let tools = tool_definitions("test/client");
        let definitions = tools.as_array().unwrap();
        assert_eq!(definitions.len(), 23);
        let attach = definitions
            .iter()
            .find(|definition| definition["name"] == "attach")
            .unwrap();
        let properties = attach["inputSchema"]["properties"].as_object().unwrap();

        assert!(properties.contains_key("content_base64"));
        assert!(!properties.contains_key("path"));
        assert_eq!(
            attach["inputSchema"]["required"],
            serde_json::json!(["note", "file_name", "content_base64"])
        );
    }

    #[test]
    fn legacy_promotion_tools_expose_exact_plan_apply_and_rollback_contracts() {
        let tools = tool_definitions("test/client");
        let definitions = tools.as_array().unwrap();
        let plan_definition = definitions
            .iter()
            .find(|tool| tool["name"] == "plan_legacy_artifact_promotions")
            .unwrap();
        let apply_definition = definitions
            .iter()
            .find(|tool| tool["name"] == "apply_legacy_artifact_promotion")
            .unwrap();
        let rollback_definition = definitions
            .iter()
            .find(|tool| tool["name"] == "rollback_legacy_artifact_promotion")
            .unwrap();
        assert_eq!(plan_definition["annotations"]["readOnlyHint"], true);
        assert_eq!(plan_definition["annotations"]["idempotentHint"], true);
        assert_eq!(apply_definition["annotations"]["destructiveHint"], true);
        assert_eq!(apply_definition["annotations"]["idempotentHint"], true);
        assert_eq!(apply_definition["annotations"]["openWorldHint"], true);
        assert_eq!(rollback_definition["annotations"]["destructiveHint"], true);
        assert_eq!(rollback_definition["annotations"]["idempotentHint"], false);
        assert_eq!(
            apply_definition["inputSchema"]["properties"]["schema"]["const"],
            crate::migrate::PROMOTION_PLAN_SCHEMA
        );
        assert_eq!(
            rollback_definition["inputSchema"]["properties"]["schema"]["const"],
            crate::migrate::PROMOTION_RESULT_SCHEMA
        );

        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        drop(open_db(&vault).unwrap());
        let note_id = vault
            .propose_for_test(
                "Legacy promotion MCP",
                "本文",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let attachment_dir = vault.attach_dir(&note_id).unwrap();
        std::fs::create_dir_all(&attachment_dir).unwrap();
        std::fs::write(attachment_dir.join("legacy.bin"), b"legacy bytes").unwrap();
        let workspace_id = crate::workspace::workspace_id(&vault).unwrap();
        let ledger = crate::ledger::Ledger::open(&vault, &workspace_id).unwrap();
        crate::migrate::migrate(&vault, &ledger, &workspace_id, "2026-08-21T02:00:00Z").unwrap();
        let manifest_before = ledger.list().remove(0);

        let first = call_tool(
            &vault,
            "test/client",
            "plan_legacy_artifact_promotions",
            &serde_json::json!({}),
            true,
        )
        .unwrap()
        .structured
        .unwrap();
        let second = call_tool(
            &vault,
            "test/client",
            "plan_legacy_artifact_promotions",
            &serde_json::json!({}),
            true,
        )
        .unwrap()
        .structured
        .unwrap();
        assert_eq!(first, second, "連続planはbyte-equivalent");
        assert_eq!(first["read_only"], true);
        assert_eq!(first["plans"].as_array().unwrap().len(), 1);
        assert_eq!(
            first["plans"][0]["schema"],
            crate::migrate::PROMOTION_PLAN_SCHEMA
        );
        assert_eq!(
            ledger.list().remove(0),
            manifest_before,
            "planは台帳を変更しない"
        );

        let rejected = call_tool(
            &vault,
            "test/client",
            "plan_legacy_artifact_promotions",
            &serde_json::json!({"limit": 1}),
            false,
        )
        .unwrap_err();
        assert!(rejected.to_string().contains("引数を受け取らない"));
    }

    #[test]
    fn distillation_plan_is_exposed_as_an_idempotent_read_only_snapshot() {
        let tools = tool_definitions("test/client");
        let definition = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|definition| definition["name"] == "plan_distillation")
            .unwrap();
        assert_eq!(definition["annotations"]["readOnlyHint"], true);
        assert_eq!(definition["annotations"]["destructiveHint"], false);
        assert_eq!(definition["annotations"]["idempotentHint"], true);
        assert_eq!(definition["inputSchema"]["additionalProperties"], false);

        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        drop(open_db(&vault).unwrap());
        let before = std::fs::metadata(vault.index_db_path())
            .unwrap()
            .modified()
            .unwrap();
        let output = call_tool(
            &vault,
            "test/client",
            "plan_distillation",
            &serde_json::json!({}),
            true,
        )
        .unwrap()
        .structured
        .unwrap();

        assert_eq!(output["schema"], crate::distillation::PLAN_SCHEMA);
        assert_eq!(output["read_only"], true);
        assert_eq!(output["snapshot"]["note_count"], 0);
        assert!(output.get("degraded").is_none());
        assert!(output.get("conversation_events").is_none());
        assert_eq!(
            std::fs::metadata(vault.index_db_path())
                .unwrap()
                .modified()
                .unwrap(),
            before
        );

        let rejected = call_tool(
            &vault,
            "test/client",
            "plan_distillation",
            &serde_json::json!({"limit": 1}),
            false,
        )
        .unwrap_err();
        assert!(rejected.to_string().contains("引数を受け取らない"));
    }

    #[test]
    fn targeted_distillation_plan_exposes_only_requested_snapshot_bound_changes() {
        use crate::authority::{Authority, AuthorityRole, AuthorityStatus, NoteNamespace};

        let tools = tool_definitions("test/client");
        let definition = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|definition| definition["name"] == "plan_targeted_distillation")
            .unwrap();
        assert_eq!(definition["annotations"]["readOnlyHint"], true);
        assert_eq!(definition["annotations"]["destructiveHint"], false);
        assert_eq!(definition["annotations"]["idempotentHint"], true);

        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        let id = vault
            .propose(
                &conn,
                NoteProposal {
                    title: "targeted MCP target",
                    body: "欠落参照に依存する本文",
                    description: Some("機械planではkeep"),
                    tags: &["test".to_string()],
                    authority: Authority {
                        namespace: NoteNamespace::Knowledge,
                        role: AuthorityRole::Canonical,
                        status: AuthorityStatus::Active,
                        scope: "test/mcp-targeted".into(),
                    },
                    relations: Vec::new(),
                    allow_new_tags: true,
                    client: "test/client",
                },
            )
            .unwrap();
        let arguments = serde_json::json!({
            "changes": [{
                "note": id,
                "operation": "revise",
                "reason": "全文監査で欠落参照への意味依存を検出した"
            }]
        });
        let first = call_tool(
            &vault,
            "test/client",
            "plan_targeted_distillation",
            &arguments,
            true,
        )
        .unwrap()
        .structured
        .unwrap();
        let second = call_tool(
            &vault,
            "test/client",
            "plan_targeted_distillation",
            &arguments,
            true,
        )
        .unwrap()
        .structured
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(first["planner_profile"], "targeted-v1");
        assert_eq!(first["entries"].as_array().unwrap().len(), 1);
        assert_eq!(first["entries"][0]["operation"], "revise");
        assert!(first.get("degraded").is_none());
    }

    #[test]
    fn distillation_audit_is_incremental_read_only_and_suppresses_remote_sync() {
        let tools = tool_definitions("test/client");
        let definition = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|definition| definition["name"] == "audit_distillation")
            .unwrap();
        assert_eq!(definition["annotations"]["readOnlyHint"], true);
        assert_eq!(definition["annotations"]["destructiveHint"], false);
        assert_eq!(definition["annotations"]["idempotentHint"], true);
        assert_eq!(definition["inputSchema"]["additionalProperties"], false);
        assert!(definition["inputSchema"]["required"].is_null());

        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        drop(open_db(&vault).unwrap());
        let before = std::fs::metadata(vault.index_db_path())
            .unwrap()
            .modified()
            .unwrap();
        let first = call_tool(
            &vault,
            "test/client",
            "audit_distillation",
            &serde_json::json!({}),
            true,
        )
        .unwrap()
        .structured
        .unwrap();

        assert_eq!(first["schema"], crate::distillation_audit::AUDIT_SCHEMA);
        assert_eq!(first["read_only"], true);
        assert_eq!(first["delta"]["mode"], "full");
        assert_eq!(first["gate"]["passed"], true);
        assert_eq!(first["remote_backup"]["configured"], false);

        let second = call_tool(
            &vault,
            "test/client",
            "audit_distillation",
            &serde_json::json!({"baseline": first["checkpoint"].clone()}),
            true,
        )
        .unwrap()
        .structured
        .unwrap();
        assert_eq!(second["delta"]["mode"], "incremental");
        assert!(second["delta"]["workset"].as_array().unwrap().is_empty());
        assert_eq!(
            std::fs::metadata(vault.index_db_path())
                .unwrap()
                .modified()
                .unwrap(),
            before
        );

        let rejected = call_tool(
            &vault,
            "test/client",
            "audit_distillation",
            &serde_json::json!({"unknown": true}),
            false,
        )
        .unwrap_err();
        assert!(rejected.to_string().contains("引数を解釈できない"));
    }

    #[test]
    fn distillation_cadence_separates_read_only_status_from_local_state_progress() {
        let tools = tool_definitions("test/client");
        let definitions = tools.as_array().unwrap();
        let status_definition = definitions
            .iter()
            .find(|definition| definition["name"] == "distillation_cadence_status")
            .unwrap();
        let run_definition = definitions
            .iter()
            .find(|definition| definition["name"] == "run_distillation_cadence")
            .unwrap();
        assert_eq!(status_definition["annotations"]["readOnlyHint"], true);
        assert_eq!(status_definition["annotations"]["idempotentHint"], true);
        assert_eq!(run_definition["annotations"]["readOnlyHint"], false);
        assert_eq!(run_definition["annotations"]["destructiveHint"], false);
        assert_eq!(run_definition["annotations"]["idempotentHint"], false);

        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        drop(open_db(&vault).unwrap());
        let status = call_tool(
            &vault,
            "test/client",
            "distillation_cadence_status",
            &serde_json::json!({}),
            true,
        )
        .unwrap()
        .structured
        .unwrap();
        assert_eq!(
            status["schema"],
            crate::distillation_cadence::CADENCE_STATUS_SCHEMA
        );
        assert_eq!(status["state_exists"], false);
        assert_eq!(status["lanes"].as_array().unwrap().len(), 4);

        let run = call_tool(
            &vault,
            "test/client",
            "run_distillation_cadence",
            &serde_json::json!({}),
            true,
        )
        .unwrap()
        .structured
        .unwrap();
        assert_eq!(
            run["schema"],
            crate::distillation_cadence::CADENCE_RUN_SCHEMA
        );
        assert_eq!(run["executed"], true);
        assert_eq!(run["accepted"], true);
        assert_eq!(run["selected_lanes"].as_array().unwrap().len(), 4);

        let rejected = call_tool(
            &vault,
            "test/client",
            "run_distillation_cadence",
            &serde_json::json!({"lane": "yearly"}),
            false,
        )
        .unwrap_err();
        assert!(rejected.to_string().contains("引数を解釈できない"));
    }

    #[test]
    fn semantic_execution_tools_apply_and_rollback_the_exact_plan_snapshot() {
        use crate::authority::{Authority, AuthorityRole, AuthorityStatus, NoteNamespace};

        let tools = tool_definitions("test/client");
        let apply = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "apply_distillation")
            .unwrap();
        let rollback = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "rollback_distillation")
            .unwrap();
        for definition in [apply, rollback] {
            assert_eq!(definition["annotations"]["readOnlyHint"], false);
            assert_eq!(definition["annotations"]["destructiveHint"], true);
            assert_eq!(definition["annotations"]["idempotentHint"], false);
        }

        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        let tags = vec!["test".to_string()];
        let id = vault
            .propose(
                &conn,
                NoteProposal {
                    title: "MCP semantic target",
                    body: "本文",
                    description: None,
                    tags: &tags,
                    authority: Authority {
                        namespace: NoteNamespace::Knowledge,
                        role: AuthorityRole::Canonical,
                        status: AuthorityStatus::Active,
                        scope: "test/mcp-semantic".into(),
                    },
                    relations: Vec::new(),
                    allow_new_tags: true,
                    client: "test/client",
                },
            )
            .unwrap();
        let planned = call_tool(
            &vault,
            "test/client",
            "plan_distillation",
            &serde_json::json!({}),
            false,
        )
        .unwrap()
        .structured
        .unwrap();
        let entry = planned["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["note"] == id)
            .unwrap();
        assert_eq!(entry["operation"], "normalize");
        let note = vault.read_note_from_db(&conn, &id).unwrap();
        let applied = call_tool(
            &vault,
            "test/client",
            "apply_distillation",
            &serde_json::json!({
                "schema": "kb-app.distillation-execution-request/v1",
                "plan_schema": planned["schema"],
                "planner_profile": planned["planner_profile"],
                "plan_id": planned["plan_id"],
                "snapshot_digest": planned["snapshot"]["digest"],
                "snapshot_note_count": planned["snapshot"]["note_count"],
                "changes": [{
                    "note": id,
                    "input_hash": entry["input_hash"],
                    "operation": "normalize",
                    "reason": "検索結果に用途を表示する",
                    "target": {
                        "title": &note.front.title,
                        "body": &note.body,
                        "description": "MCP経由で追加した一文要約",
                        "tags": &note.front.tags,
                        "relations": &note.front.relations,
                    }
                }]
            }),
            false,
        )
        .unwrap()
        .structured
        .unwrap();
        assert_eq!(applied["status"], "applied");
        assert_eq!(applied["conversation_events"][0]["required"], true);
        assert_eq!(
            vault
                .read_note_from_db(&open_db(&vault).unwrap(), &id)
                .unwrap()
                .front
                .description
                .as_deref(),
            Some("MCP経由で追加した一文要約")
        );

        let rolled_back = call_tool(
            &vault,
            "test/client",
            "rollback_distillation",
            &serde_json::json!({"execution_id": applied["execution_id"]}),
            false,
        )
        .unwrap()
        .structured
        .unwrap();
        assert_eq!(rolled_back["status"], "rolled_back");
        assert!(
            vault
                .read_note_from_db(&open_db(&vault).unwrap(), &id)
                .unwrap()
                .front
                .description
                .is_none()
        );
    }

    #[test]
    fn initiative_closure_tools_plan_apply_and_rollback_exact_active_initiative() {
        use crate::authority::{Authority, AuthorityRole, AuthorityStatus, NoteNamespace};

        let tools = tool_definitions("test/client");
        let definitions = tools.as_array().unwrap();
        let plan_definition = definitions
            .iter()
            .find(|tool| tool["name"] == "plan_initiative_closure")
            .unwrap();
        assert_eq!(plan_definition["annotations"]["readOnlyHint"], true);
        assert_eq!(plan_definition["annotations"]["destructiveHint"], false);
        for name in ["apply_initiative_closure", "rollback_initiative_closure"] {
            let definition = definitions
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap();
            assert_eq!(definition["annotations"]["readOnlyHint"], false);
            assert_eq!(definition["annotations"]["destructiveHint"], true);
            assert_eq!(definition["annotations"]["idempotentHint"], false);
        }

        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        let id = vault
            .propose(
                &conn,
                NoteProposal {
                    title: "MCP initiative closure target",
                    body: "完了した作業",
                    description: Some("MCPで閉じるinitiative"),
                    tags: &["test".to_string()],
                    authority: Authority {
                        namespace: NoteNamespace::Initiatives,
                        role: AuthorityRole::Canonical,
                        status: AuthorityStatus::Active,
                        scope: "test/mcp-initiative-close".into(),
                    },
                    relations: Vec::new(),
                    allow_new_tags: true,
                    client: "test/client",
                },
            )
            .unwrap();
        let planned = call_tool(
            &vault,
            "test/client",
            "plan_initiative_closure",
            &serde_json::json!({
                "changes": [{
                    "note": id,
                    "reason": "完了条件と最終監査を満たした"
                }]
            }),
            true,
        )
        .unwrap()
        .structured
        .unwrap();
        assert_eq!(planned["read_only"], true);
        assert_eq!(planned["planner_profile"], "initiative-close-v1");
        assert_eq!(planned["changes"][0]["from_status"], "active");
        assert_eq!(planned["changes"][0]["to_status"], "historical");

        let applied = call_tool(
            &vault,
            "test/client",
            "apply_initiative_closure",
            &serde_json::json!({
                "schema": "kb-app.initiative-closure-execution-request/v1",
                "plan_schema": planned["schema"],
                "planner_profile": planned["planner_profile"],
                "plan_id": planned["plan_id"],
                "snapshot_digest": planned["snapshot"]["digest"],
                "snapshot_note_count": planned["snapshot"]["note_count"],
                "changes": planned["changes"],
            }),
            false,
        )
        .unwrap()
        .structured
        .unwrap();
        assert_eq!(applied["status"], "applied");
        assert_eq!(applied["conversation_events"][0]["required"], true);
        assert_eq!(
            vault
                .read_note_from_db(&open_db(&vault).unwrap(), &id)
                .unwrap()
                .front
                .authority
                .unwrap()
                .status,
            AuthorityStatus::Historical
        );

        let rolled_back = call_tool(
            &vault,
            "test/client",
            "rollback_initiative_closure",
            &serde_json::json!({"execution_id": applied["execution_id"]}),
            false,
        )
        .unwrap()
        .structured
        .unwrap();
        assert_eq!(rolled_back["status"], "rolled_back");
        assert_eq!(
            vault
                .read_note_from_db(&open_db(&vault).unwrap(), &id)
                .unwrap()
                .front
                .authority
                .unwrap()
                .status,
            AuthorityStatus::Active
        );
    }

    #[test]
    fn autonomous_remove_uses_a_short_lived_target_bound_plan() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let note_id = vault
            .propose_for_test(
                "削除対象",
                "削除前の本文",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let mut plans = RemovalPlans::default();

        let prepared = call_tool_with_search_options(
            &vault,
            "test/client",
            "prepare_remove",
            &serde_json::json!({"note": note_id, "reason": "重複した中間ノート"}),
            false,
            true,
            &mut plans,
        )
        .unwrap()
        .structured
        .unwrap();
        let token = prepared["removal_token"].as_str().unwrap();
        assert_eq!(token.len(), 64);
        assert_eq!(prepared["expires_in_seconds"], 300);
        assert_eq!(prepared["reason"], "重複した中間ノート");
        assert_eq!(prepared["conversation_events"][0]["required"], true);
        assert_eq!(
            prepared["conversation_events"][0]["event"],
            "removal_prepared"
        );
        assert!(
            vault
                .read_note_from_db(&open_db(&vault).unwrap(), &note_id)
                .is_ok()
        );

        let removed = call_tool_with_search_options(
            &vault,
            "test/client",
            "commit_remove",
            &serde_json::json!({
                "note": note_id,
                "removal_token": token,
            }),
            false,
            true,
            &mut plans,
        )
        .unwrap()
        .structured
        .unwrap();
        assert_eq!(removed["event"], "note_removed");
        assert!(
            vault
                .read_note_from_db(&open_db(&vault).unwrap(), &note_id)
                .is_err()
        );

        let reused = call_tool_with_search_options(
            &vault,
            "test/client",
            "commit_remove",
            &serde_json::json!({
                "note": note_id,
                "removal_token": token,
            }),
            false,
            true,
            &mut plans,
        )
        .unwrap_err();
        assert!(reused.to_string().contains("無効または使用済み"));
    }

    #[test]
    fn removal_plan_rejects_expiry_target_swap_and_note_changes() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let first = vault
            .propose_for_test("対象A", "本文A", None, &["test".into()], "test/client")
            .unwrap();
        let second = vault
            .propose_for_test("対象B", "本文B", None, &["test".into()], "test/client")
            .unwrap();
        let mut plans = RemovalPlans::default();

        let prepare = |note: &str, plans: &mut RemovalPlans| {
            call_tool_with_search_options(
                &vault,
                "test/client",
                "prepare_remove",
                &serde_json::json!({"note": note, "reason": "重複整理"}),
                false,
                true,
                plans,
            )
            .unwrap()
            .structured
            .unwrap()["removal_token"]
                .as_str()
                .unwrap()
                .to_string()
        };

        let swap_token = prepare(&first, &mut plans);
        let swapped = call_tool_with_search_options(
            &vault,
            "test/client",
            "commit_remove",
            &serde_json::json!({"note": second, "removal_token": swap_token}),
            false,
            true,
            &mut plans,
        )
        .unwrap_err();
        assert!(
            swapped
                .to_string()
                .contains("削除対象がprepare_remove時と一致しない")
        );

        let changed_token = prepare(&first, &mut plans);
        let conn = open_db(&vault).unwrap();
        vault
            .agent_update_note(
                &conn,
                NoteUpdate {
                    id: &first,
                    title: None,
                    body: Some("変更後"),
                    description: None,
                    tags: None,
                    authority: None,
                    relations: None,
                    allow_new_tags: false,
                    client: "test/client",
                },
            )
            .unwrap();
        let changed = call_tool_with_search_options(
            &vault,
            "test/client",
            "commit_remove",
            &serde_json::json!({"note": first, "removal_token": changed_token}),
            false,
            true,
            &mut plans,
        )
        .unwrap_err();
        assert!(changed.to_string().contains("prepare_remove後に変更された"));

        let expired_token = prepare(&second, &mut plans);
        plans.pending.get_mut(&expired_token).unwrap().expires_at =
            Instant::now() - Duration::from_secs(1);
        let expired = call_tool_with_search_options(
            &vault,
            "test/client",
            "commit_remove",
            &serde_json::json!({"note": second, "removal_token": expired_token}),
            false,
            true,
            &mut plans,
        )
        .unwrap_err();
        assert!(expired.to_string().contains("期限が切れた"));
    }

    #[test]
    fn destructive_annotations_cover_commit_remove_and_semantic_writes() {
        let tools = tool_definitions("chatgpt/openai");
        let definitions = tools.as_array().unwrap();
        assert!(definitions.iter().all(|tool| tool["name"] != "remove"));
        let prepare = definitions
            .iter()
            .find(|tool| tool["name"] == "prepare_remove")
            .unwrap();
        let commit = definitions
            .iter()
            .find(|tool| tool["name"] == "commit_remove")
            .unwrap();
        assert_eq!(prepare["annotations"]["destructiveHint"], false);
        assert_eq!(commit["annotations"]["destructiveHint"], true);
        for name in [
            "apply_distillation",
            "rollback_distillation",
            "apply_initiative_closure",
            "rollback_initiative_closure",
            "apply_legacy_artifact_promotion",
            "rollback_legacy_artifact_promotion",
        ] {
            let semantic = definitions
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap();
            assert_eq!(semantic["annotations"]["destructiveHint"], true);
        }
        assert_eq!(
            commit["inputSchema"]["required"],
            serde_json::json!(["note", "removal_token"])
        );
        assert_eq!(
            prepare["inputSchema"]["required"],
            serde_json::json!(["note", "reason"])
        );
    }

    #[test]
    fn surface_schema_requires_explicit_note_and_hides_new_tag_capability() {
        fn find<'a>(definitions: &'a Value, name: &str) -> &'a Value {
            definitions
                .as_array()
                .unwrap()
                .iter()
                .find(|definition| definition["name"] == name)
                .unwrap()
        }

        let codex = tool_definitions("codex-cli/gpt-5-codex");
        let desktop = tool_definitions("claude-desktop/claude");

        assert_eq!(
            find(&codex, "get")["inputSchema"]["required"],
            serde_json::json!(["note"])
        );
        assert!(
            find(&desktop, "get")["inputSchema"]
                .get("required")
                .is_none()
        );
        for definitions in [&codex, &desktop] {
            for name in ["propose", "update"] {
                let schema = &find(definitions, name)["inputSchema"];
                assert_eq!(schema["additionalProperties"], false);
                assert!(schema["properties"].get("allow_new_tags").is_none());
            }
            assert_eq!(
                find(definitions, "propose")["inputSchema"]["required"],
                serde_json::json!(["title", "body", "tags", "authority"])
            );
        }
    }

    #[test]
    fn mcp_rejects_hidden_new_tag_override_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let error = call_tool(
            &vault,
            "codex-cli/gpt-5-codex",
            "propose",
            &serde_json::json!({
                "title": "拒否されるノート",
                "body": "本文",
                "tags": ["unapproved"],
                "allow_new_tags": true
            }),
            false,
        )
        .unwrap_err();

        assert!(error.to_string().contains("AI用MCPでは利用できない"));
        let conn = open_db(&vault).unwrap();
        assert!(recent(&conn, 10).unwrap().is_empty());
    }

    #[test]
    fn core_enforces_surface_specific_current_note_capability() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let note_id = vault
            .propose_for_test("現在ノート", "本文", None, &["test".into()], "test/client")
            .unwrap();
        crate::connect::set_current_note(&vault, &note_id).unwrap();

        let error = call_tool(
            &vault,
            "codex-cli/gpt-5-codex",
            "get",
            &serde_json::json!({}),
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("note引数が必要"));

        let desktop = call_tool(
            &vault,
            "claude-desktop/claude",
            "get",
            &serde_json::json!({}),
            false,
        )
        .unwrap();
        assert_eq!(desktop.structured.unwrap()["note_id"], note_id);
    }

    #[test]
    fn attach_rejects_oversized_input_before_base64_decode() {
        let max_encoded = crate::intake::MCP_CONTENT_MAX_BYTES.div_ceil(3) * 4;
        let invalid_but_too_large = "!".repeat(max_encoded + 1);
        let error = decode_attachment_content(&serde_json::json!({
            "content_base64": invalid_but_too_large
        }))
        .unwrap_err();

        assert!(matches!(
            error.downcast_ref::<crate::artifact::ArtifactError>(),
            Some(crate::artifact::ArtifactError::TooLarge { .. })
        ));
    }

    #[test]
    fn mcp_attach_returns_structured_identity_and_get_lists_the_file() {
        if !crate::external_tools::git_lfs_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let note_id = vault
            .propose_for_test(
                "鬼キャラクター",
                "赤鬼ちゃんと青鬼ちゃん。",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"png bytes");

        let attached = handle(
            Some(&vault),
            "test/client",
            true,
            false,
            "tools/call",
            Some(&serde_json::json!({
                "name": "attach",
                "arguments": {
                    "note": note_id,
                    "file_name": "red-oni.png",
                    "content_base64": encoded,
                    "ref_name": "red-oni"
                }
            })),
        )
        .unwrap()
        .unwrap();

        assert!(attached.get("isError").is_none(), "{attached}");
        assert_eq!(attached["structuredContent"]["note"], note_id);
        assert_eq!(attached["structuredContent"]["note_id"], note_id);
        assert_eq!(attached["structuredContent"]["title"], "鬼キャラクター");
        assert_eq!(
            attached["structuredContent"]["conversation_link"].as_str(),
            vault.note_path(&note_id).unwrap().to_str()
        );
        assert_eq!(attached["structuredContent"]["file_name"], "red-oni.png");
        assert_eq!(attached["structuredContent"]["media_type"], "image/png");
        assert_eq!(attached["structuredContent"]["size"], 9);
        assert_eq!(attached["structuredContent"]["role"], "file");
        assert_eq!(attached["structuredContent"]["policy"]["sync"], "full");
        assert_eq!(attached["structuredContent"]["ref_name"], "red-oni");
        assert_eq!(attached["structuredContent"]["locator"], "managed");

        let fetched = call_tool(
            &vault,
            "test/client",
            "get",
            &serde_json::json!({"note": note_id}),
            false,
        )
        .unwrap();
        assert!(fetched.text.contains("red-oni.png"), "{}", fetched.text);
        assert!(fetched.text.contains("ref=red-oni"), "{}", fetched.text);
        let fetched_data = fetched.structured.unwrap();
        assert_eq!(fetched_data["artifacts"][0]["role"], "file");
        assert_eq!(fetched_data["artifacts"][0]["ref_name"], "red-oni");
        assert_eq!(fetched_data["artifacts"][0]["availability"], "local");
    }
}
