//! MCP サーバー(stdio、newline-delimited JSON-RPC 2.0)。
//! 公開ツールは search / get / recent / propose のみ — confirm は公開しない
//! (確定は人の操作。FR-C5。統治は接続面で差を付ける)。
//!
//! v0.1 は手組みの最小実装(依存最小・同期 I/O)。リモート化(Streamable HTTP)の
//! 段階で公式 Rust SDK(rmcp)への載せ替えを再評価する(ADR-0001 スタック表)。

use std::io::{BufRead, Write};

use anyhow::Result;
use serde_json::{Value, json};

use crate::index::{open_db, sync};
use crate::search::{recent, search};
use crate::vault::Vault;

const PROTOCOL_FALLBACK: &str = "2025-06-18";

/// server instructions(FR-C5)。「まず引く・終わりに起票を提案」の規律を配る。
/// 旧 KB の M1/M3 実測で「instructions だけでフック無し環境でも規律が成立」を確認済み。
const INSTRUCTIONS: &str = "このサーバーはユーザーの個人ナレッジベース(vault)への取次口。\
ユーザー個人に関する話題(嗜好・過去の決定・進行中の作業・過去に調べたこと)に触れる前に、\
必ず search で当たりを付け、ヒットしたノートは get で全文を読んでから答えること。\
該当が無ければその旨を添えて普通に答えてよい。\
会話の中で恒久的に残す価値のある知見・決定・事実が新しく生まれたら、会話の終わりに \
propose での下書き起票を提案すること(起票は下書きまで。確定はユーザーがアプリ側で行う)。";

pub fn serve(vault: &Vault, client_hint: &str) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
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
        let response = match handle(vault, client_hint, method, msg.get("params")) {
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

fn handle(vault: &Vault, client: &str, method: &str, params: Option<&Value>) -> Result<Option<Value>> {
    match method {
        "initialize" => {
            let requested = params
                .and_then(|p| p.get("protocolVersion"))
                .and_then(|v| v.as_str())
                .unwrap_or(PROTOCOL_FALLBACK);
            Ok(Some(json!({
                "protocolVersion": requested,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "kb-app", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            })))
        }
        "ping" => Ok(Some(json!({}))),
        "tools/list" => Ok(Some(json!({"tools": tool_definitions()}))),
        "tools/call" => {
            let name = params
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args = params
                .and_then(|p| p.get("arguments"))
                .cloned()
                .unwrap_or(json!({}));
            let text = call_tool(vault, client, name, &args);
            match text {
                Ok(t) => Ok(Some(json!({"content": [{"type": "text", "text": t}]}))),
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

fn tool_definitions() -> Value {
    json!([
        {
            "name": "search",
            "description": "ナレッジベースを検索する(全文+リンク近傍)。ユーザー個人に関する話題ではまずこれを引く。結果に劣化情報(degraded)があれば検索品質が落ちている — ユーザーへの回答にその旨を添える。",
            "inputSchema": {"type": "object", "properties": {
                "query": {"type": "string", "description": "検索語(空白区切りのキーワード列挙可)"},
                "limit": {"type": "integer", "description": "最大件数(既定 8)"}
            }, "required": ["query"]}
        },
        {
            "name": "get",
            "description": "ノート ID を指定して全文(frontmatter + 本文)を取得する。search でヒットしたノートは必ずこれで全文を読んでから答える。",
            "inputSchema": {"type": "object", "properties": {
                "note": {"type": "string", "description": "ノート ID(例: notes/foo)"}
            }, "required": ["note"]}
        },
        {
            "name": "recent",
            "description": "最近作成・更新されたノートの一覧。",
            "inputSchema": {"type": "object", "properties": {
                "limit": {"type": "integer", "description": "最大件数(既定 10)"}
            }}
        },
        {
            "name": "propose",
            "description": "会話から得た恒久的な知見・決定を下書きノートとして起票する。起票は下書き(draft)までで、確定はユーザーがアプリ側で行う。本文は自己完結の Markdown で、出典となる会話の文脈を要約して含める。",
            "inputSchema": {"type": "object", "properties": {
                "title": {"type": "string", "description": "ノートのタイトル(内容が一意に分かる具体的なもの)"},
                "body": {"type": "string", "description": "本文(Markdown)"},
                "description": {"type": "string", "description": "一文要約"},
                "tags": {"type": "array", "items": {"type": "string"}, "description": "分類タグ(任意)"}
            }, "required": ["title", "body"]}
        }
    ])
}

fn call_tool(vault: &Vault, client: &str, name: &str, args: &Value) -> Result<String> {
    // メッセージのやり取りの際に pull(複数デバイス同期・FR-A6 改定)。
    // スロットリング付き・失敗は劣化情報(fail-open)
    let pull_note = crate::connect::pull_if_stale(vault);
    let conn = open_db(vault)?;
    // 増分 sync(書いてすぐ引ける保証)。失敗しても検索は劣化情報つきで続行(fail-open)
    let sync_note = match sync(vault, &conn) {
        Ok(_) => pull_note,
        Err(e) => Some(format!("索引の更新に失敗(結果が古い可能性): {e}")),
    };
    match name {
        "search" => {
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(8) as usize;
            let mut out = search(&conn, query, limit);
            if let Some(s) = sync_note {
                out.degraded.push(s);
            }
            let mut text = String::new();
            if out.hits.is_empty() {
                text.push_str("該当なし。\n");
            }
            for h in &out.hits {
                text.push_str(&format!(
                    "- {} [{}]{}: {}\n",
                    h.id,
                    h.title.as_deref().unwrap_or("無題"),
                    if h.status == "draft" { "(draft)" } else { "" },
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
            for d in &out.degraded {
                text.push_str(&format!("⚠ 劣化: {d}\n"));
            }
            Ok(text)
        }
        "get" => {
            let id = args
                .get("note")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("note が必要"))?;
            let note = vault.read_note(id)?;
            Ok(note.to_file_string()?)
        }
        "recent" => {
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let hits = recent(&conn, limit)?;
            Ok(hits
                .iter()
                .map(|h| {
                    format!(
                        "- {} [{}]{}: {}",
                        h.id,
                        h.title.as_deref().unwrap_or("無題"),
                        if h.status == "draft" { "(draft)" } else { "" },
                        h.snippet
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"))
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
                .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let id = vault.propose(title, body, description, &tags, client)?;
            Ok(format!(
                "下書きを起票した: {id}(status: draft)。確定はユーザーがアプリ側で行う。"
            ))
        }
        other => anyhow::bail!("unknown tool: {other}"),
    }
}
