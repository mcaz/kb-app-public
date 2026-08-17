//! MCP サーバー(stdio、newline-delimited JSON-RPC 2.0)。
//! 公開ツールは search / get / recent / propose / update / remove。
//! 人間のノートは変更できない(所有ガード)。
//!
//! v0.1 は手組みの最小実装(依存最小・同期 I/O)。リモート化(Streamable HTTP)の
//! 段階で公式 Rust SDK(rmcp)への載せ替えを再評価する(ADR-0001 スタック表)。

use std::io::{BufRead, Write};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::index::{open_db, sync_with_degradations};
use crate::search::{recent, search};
use crate::vault::{NoteProposal, NoteUpdate, Vault};

const PROTOCOL_FALLBACK: &str = "2025-06-18";

/// server instructions(FR-C5)。「まず引く・終わりに起票を提案」の規律を配る。
/// 旧 KB の M1/M3 実測で「instructions だけでフック無し環境でも規律が成立」を確認済み。
/// FR-C6(プロバイダ別出し分け)は保留中のため、現接続先の Claude に直接最適化した文面
/// (2026-08-10 本人決定: Claude 最適化を先行し、後続開発はその観察に合わせる)。
const INSTRUCTIONS: &str = "\
ユーザーの個人ナレッジベース(kb-app)への取次口。会話で「引く・育てる」を回す。\n\
【契約(機構で強制。正本はアプリの docs/contract.md)】ノートはタグ1〜4個必須/\
形式はアプリが管理(frontmatter を自分で書かない)。\n\
【引く】ユーザー個人に関する話題(嗜好・決定・進行中の作業・過去に調べたこと・固有名詞)は、\
推測で答える前に search(自然文可・意味で当たる。外したら語を変えて再検索)。ヒットは get で\
全文を読んでから答える。`[kb-app 自動retrieval — MCP]` が届いていれば、その search → get は\
実行済みなので、文脈が不足するときだけ追加検索する。本文中の /path.md リンクは必要なら辿る。\
「このノート」=引数なしの get。該当なしは正常(その旨を添える)。degraded があれば回答に添える。\n\
【育てる】残す価値のある知見・決定が生まれたら、会話の終わりに propose を提案(承諾を得て\
から)。既存ノートの手入れは update / remove で直接(大きな変更は一言添える)。\
本文は未来の読者向けに自己完結で(経緯・出典・関連ノートへの /path.md リンク)。\n\
【関連】ノート間の関連づけはあなたの領分。get の「近いノート」を見て、本当に関連するなら\
update で本文に /path.md リンクを足す(ユーザーに可否を尋ねる形にはしない)。\n\
【タグ】体系は会話でユーザーと合意して育てる(暫定・要確認といった扱いもタグで表す — \
アプリに下書き状態は無い)。合意済み(「タグ運用」ノート。無ければ起票を\
提案)は勝手に変えない。それ以外はあなたの裁量で付与・統合・整理してよい(まとめて整理したら\
一言報告)。新語を乱発せず既存語彙に揃える。";

/// OFF 時は KB の内容や保存先を渡さず、迂回禁止だけを制御プレーンとして配る。
/// ツールを非公開にするだけでは、汎用 shell を持つ AI が Vault を直読みできるため。
const DISABLED_INSTRUCTIONS: &str = "\
kb-app はこの AI クライアントで無効です。\
KB のデータを shell・ファイル操作・保存先の探索など別経路で参照・推測・更新しないでください。\
過去の会話に残る KB 内容も代替経路として使わず、必要な場合は「現在は KB を参照できない」と伝えてください。";

pub fn serve(client_hint: &str, mut open_vault: impl FnMut() -> Result<Vault>) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut vault = None;
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
        let enabled = match crate::settings::load() {
            Ok(settings) => {
                settings.ai_kb_enabled_for(client_hint)
                    && crate::ai_guard::client_is_enforced(client_hint)
            }
            Err(error) => {
                eprintln!("kb mcp: settings unavailable: {}", error.detail());
                false
            }
        };
        // initialize / list / OFF は Vault の場所すら開かない。ON の tool call が来た
        // 最初の1回だけ開き、以後は同じ process 内で再利用する。
        let needs_vault = method_needs_vault(enabled, method);
        if needs_vault && vault.is_none() {
            vault = Some(open_vault()?);
        }
        let response = match handle(
            vault.as_ref(),
            client_hint,
            enabled,
            method,
            msg.get("params"),
        ) {
            Ok(Some(result)) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Ok(None) => json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": format!("unknown method: {method}")}}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32603, "message": e.to_string()}}),
        };
        writeln!(out, "{response}")?;
        out.flush()?;
    }
    Ok(())
}

fn method_needs_vault(enabled: bool, method: &str) -> bool {
    enabled && method == "tools/call"
}

fn handle(
    vault: Option<&Vault>,
    client: &str,
    enabled: bool,
    method: &str,
    params: Option<&Value>,
) -> Result<Option<Value>> {
    match method {
        "initialize" => {
            let requested = params
                .and_then(|p| p.get("protocolVersion"))
                .and_then(|v| v.as_str())
                .unwrap_or(PROTOCOL_FALLBACK);
            let mut initialized = json!({
                "protocolVersion": requested,
                "capabilities": if enabled { json!({"tools": {}, "prompts": {}}) } else { json!({}) },
                "serverInfo": {"name": "kb-app", "version": env!("CARGO_PKG_VERSION")},
            });
            initialized["instructions"] = json!(if enabled {
                INSTRUCTIONS
            } else {
                DISABLED_INSTRUCTIONS
            });
            Ok(Some(initialized))
        }
        "ping" => Ok(Some(json!({}))),
        "tools/list" => Ok(Some(if enabled {
            json!({"tools": tool_definitions()})
        } else {
            json!({"tools": []})
        })),
        // MCP prompts — 定型操作の入口(常駐コンテキストを増やさず、正しい挙動を
        // ワンタップで起動させる。Desktop のプロンプトピッカーに現れる)
        "prompts/list" => Ok(Some(if enabled {
            json!({"prompts": prompt_definitions()})
        } else {
            json!({"prompts": []})
        })),
        "prompts/get" => {
            if !enabled {
                anyhow::bail!("kb_disabled");
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
                return Ok(Some(json!({
                    "content": [{"type": "text", "text": "KBは設定で無効です [kb_disabled]"}],
                    "isError": true,
                })));
            }
            let name = params
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args = params
                .and_then(|p| p.get("arguments"))
                .cloned()
                .unwrap_or(json!({}));
            let vault = vault.context("Vault is unavailable")?;
            let output = call_tool(vault, client, name, &args);
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
                    "isError": true,
                }))),
            }
        }
        _ => Ok(None),
    }
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
してよい(update で実行)。ユーザーの合意が要ると感じた変更は提案に留めて。\
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

fn tool_definitions() -> Value {
    json!([
        {
            "name": "search",
            "description": "KB 検索(全文+意味+リンク近傍)。個人の話題ではまず引く。自然文可。degraded は回答に添える。",
            "inputSchema": {"type": "object", "properties": {
                "query": {"type": "string", "description": "検索語(自然文可)"},
                "limit": {"type": "integer", "description": "最大件数(既定8)"},
                "any": {"type": "boolean", "description": "語をOR結合する(発話全文の自動retrieval用)"}
            }, "required": ["query"]}
        },
        {
            "name": "get",
            "description": "ノート全文の取得(応答に添付と「近いノート」が付く)。search のヒットは必ず全文を読む。note 省略=いま開いているノート。",
            "inputSchema": {"type": "object", "properties": {
                "note": {"type": "string", "description": "ノート ID。省略=いま開いているノート"}
            }}
        },
        {
            "name": "recent",
            "description": "最近のノート一覧。",
            "inputSchema": {"type": "object", "properties": {
                "limit": {"type": "integer", "description": "最大件数(既定10)"}
            }}
        },
        {
            "name": "propose",
            "description": "知見をノートとして起票。本文は自己完結の Markdown で、経緯・出典と関連ノートへの /path.md リンクを含める。",
            "inputSchema": {"type": "object", "properties": {
                "title": {"type": "string", "description": "内容が一意に分かるタイトル"},
                "body": {"type": "string", "description": "本文(自己完結)"},
                "description": {"type": "string", "description": "一文要約"},
                "tags": {"type": "array", "items": {"type": "string"}, "description": "タグ1〜4個(必須・既存語彙のみ。英小文字ケバブ)"},
                "allow_new_tags": {"type": "boolean", "description": "語彙にない新語を許す(2本目のノートが見えたときだけ)"}
            }, "required": ["title", "body", "tags"]}
        },
        {
            "name": "update",
            "description": "AI ノートの直接更新(指定フィールドのみ置換)。大きな書き換えは一言添えてから。",
            "inputSchema": {"type": "object", "properties": {
                "note": {"type": "string", "description": "ノート ID"},
                "title": {"type": "string"},
                "body": {"type": "string", "description": "本文全体の置換"},
                "description": {"type": "string"},
                "tags": {"type": "array", "items": {"type": "string"}, "description": "既存語彙のみ(全消し・5個以上は不可)"},
                "allow_new_tags": {"type": "boolean", "description": "語彙にない新語を許す"}
            }, "required": ["note"]}
        },
        {
            "name": "remove",
            "description": "AI ノートの削除(履歴には残る)。重複・陳腐化の整理に。削除前に一言添える。",
            "inputSchema": {"type": "object", "properties": {
                "note": {"type": "string", "description": "ノート ID"}
            }, "required": ["note"]}
        }
    ])
}

struct ToolOutput {
    text: String,
    structured: Option<Value>,
}

impl ToolOutput {
    fn text(text: String) -> Self {
        Self {
            text,
            structured: None,
        }
    }
}

fn call_tool(vault: &Vault, client: &str, name: &str, args: &Value) -> Result<ToolOutput> {
    // メッセージのやり取りの際に pull(複数デバイス同期・FR-A6 改定)。
    // スロットリング付き・失敗は劣化情報(fail-open)
    let mut degraded: Vec<crate::degradation::Degradation> =
        crate::connect::pull_if_stale(vault).into_iter().collect();
    let conn = open_db(vault)?;
    // 増分 sync(書いてすぐ引ける保証)。失敗しても検索は劣化情報つきで続行(fail-open)
    match sync_with_degradations(vault, &conn) {
        Ok(report) => {
            degraded.extend(report.degraded);
            degraded.extend(crate::index::embed_step(&conn));
        }
        Err(error) => degraded.push(crate::degradation::Degradation::IndexSync {
            detail: error.to_string(),
        }),
    }
    match name {
        "search" => {
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(8) as usize;
            let any = args.get("any").and_then(|v| v.as_bool()).unwrap_or(false);
            let mut out = if any {
                crate::search::search_mode(&conn, query, limit, true)
            } else {
                search(&conn, query, limit)
            };
            out.degraded.extend(degraded);
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
            Ok(ToolOutput {
                text,
                structured: Some(serde_json::to_value(&out)?),
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
            let note = vault.read_note(&id)?;
            let attachments = vault.list_attachments(&id)?;
            let attach_line = if attachments.is_empty() {
                String::new()
            } else {
                format!(
                    "(添付: {} — 本文からは /{id}.files/<名前> で参照)\n",
                    attachments
                        .iter()
                        .map(|(n, _)| n.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
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
            Ok(ToolOutput::text(format!(
                "(note: {id})\n{attach_line}{sim_line}{}{}",
                degradation_text(&degraded),
                note.to_file_string()?
            )))
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
            Ok(ToolOutput::text(with_degradations(text, &degraded)))
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
            let allow_new = args
                .get("allow_new_tags")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let id = vault.propose(
                &conn,
                NoteProposal {
                    title,
                    body,
                    description,
                    tags: &tags,
                    allow_new_tags: allow_new,
                    client,
                },
            )?;
            let added = if allow_new {
                "\n新語を追加した。語彙の合意は「タグ運用」ノートに反映すること。"
            } else {
                ""
            };
            Ok(ToolOutput::text(with_degradations(
                format!("起票した: {id}。{added}"),
                &degraded,
            )))
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
            let allow_new = args
                .get("allow_new_tags")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            vault.agent_update_note(
                &conn,
                NoteUpdate {
                    id,
                    title: args.get("title").and_then(|v| v.as_str()),
                    body: args.get("body").and_then(|v| v.as_str()),
                    description: args.get("description").and_then(|v| v.as_str()),
                    tags: tags.as_deref(),
                    allow_new_tags: allow_new,
                    client,
                },
            )?;
            Ok(ToolOutput::text(with_degradations(
                format!("更新した: {id}"),
                &degraded,
            )))
        }
        "remove" => {
            let id = args
                .get("note")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("note が必要"))?;
            vault.agent_delete_note(id, client)?;
            Ok(ToolOutput::text(with_degradations(
                format!("削除した: {id}(履歴には残る)"),
                &degraded,
            )))
        }
        other => anyhow::bail!("unknown tool: {other}"),
    }
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
    fn disabled_initialize_exposes_only_the_no_bypass_rule_and_empty_lists() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let initialized = handle(
            Some(&vault),
            "test/client",
            false,
            "initialize",
            Some(&serde_json::json!({"protocolVersion": "test"})),
        )
        .unwrap()
        .unwrap();
        assert_eq!(initialized["capabilities"], serde_json::json!({}));
        assert_eq!(initialized["instructions"], DISABLED_INSTRUCTIONS);
        assert!(
            !initialized["instructions"]
                .as_str()
                .unwrap()
                .contains("~/kb")
        );

        let tools = handle(Some(&vault), "test/client", false, "tools/list", None)
            .unwrap()
            .unwrap();
        let prompts = handle(Some(&vault), "test/client", false, "prompts/list", None)
            .unwrap()
            .unwrap();
        assert_eq!(tools, serde_json::json!({"tools": []}));
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
            "tools/call",
            Some(&serde_json::json!({
                "name": "search",
                "arguments": {"query": "anything"}
            })),
        )
        .unwrap()
        .unwrap();
        assert_eq!(response["isError"], true);
        assert!(response.to_string().contains("kb_disabled"));
        assert!(!index.exists());
    }

    #[test]
    fn disabled_control_plane_never_opens_the_vault() {
        for method in ["initialize", "tools/list", "prompts/list", "tools/call"] {
            assert!(!method_needs_vault(false, method));
        }
        assert!(!method_needs_vault(true, "initialize"));
        assert!(method_needs_vault(true, "tools/call"));
    }

    #[test]
    fn enabled_initialize_keeps_the_existing_tools_and_instructions() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let initialized = handle(Some(&vault), "test/client", true, "initialize", None)
            .unwrap()
            .unwrap();
        assert_eq!(
            initialized["capabilities"],
            serde_json::json!({"tools": {}, "prompts": {}})
        );
        assert_eq!(initialized["instructions"], INSTRUCTIONS);
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
    fn mcp_get_update_and_remove_cannot_escape_the_vault() {
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
            ("remove", serde_json::json!({"note": "../outside/secret"})),
        ] {
            assert!(call_tool(&vault, "test/client", tool, &args).is_err());
            assert_eq!(std::fs::read_to_string(&secret).unwrap(), "TOP SECRET");
        }
    }

    #[test]
    fn mcp_surfaces_partial_index_failures_with_the_stable_code() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        std::fs::write(vault.root.join("notes/broken.md"), "frontmatterではない").unwrap();

        let output = call_tool(
            &vault,
            "test/client",
            "search",
            &serde_json::json!({"query": "anything"}),
        )
        .unwrap();
        assert!(output.text.contains("[index_parse]"), "{}", output.text);
        assert!(output.text.contains("notes/broken"), "{}", output.text);
        assert!(output.structured.is_some());
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
            "tools/call",
            Some(&serde_json::json!({
                "name": "search",
                "arguments": {"query": "認証 監査", "limit": 3, "any": true}
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
    }
}
