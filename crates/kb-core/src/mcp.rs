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
/// FR-C6(プロバイダ別出し分け)は保留中のため、現接続先の Claude に直接最適化した文面
/// (2026-08-10 本人決定: Claude 最適化を先行し、後続開発はその観察に合わせる)。
const INSTRUCTIONS: &str = "\
このサーバーはユーザーの個人ナレッジベース(kb-app)への取次口。ここはユーザーの外部記憶で、\
会話を通じて育つ。あなたの仕事は「引く・育てる」の両輪を回すこと。\n\
\n\
■ 引く(会話の前半で)\n\
- ユーザー個人に関する話題(嗜好・判断基準・過去の決定・進行中の作業・過去に調べたこと・\
固有名詞)に触れる前に、推測や一般論で答えず必ず search を引く\n\
- クエリはキーワード列挙でも文まるごとでもよい(意味検索が効くので、言い回しが違っても\
見つかる)。1回で当たらなければ語を変えてもう1回\n\
- ヒットしたノートは get で全文を読んでから答える。要約だけで判断しない。本文中のリンク\
(/path.md)は関連が深そうなら get で辿る\n\
- ユーザーが「このノート」と言ったら、note 引数なしの get でアプリでいま開いているノートが\
取れる\n\
- 該当なしは正常。その旨を一言添えて普通に答える\n\
- 結果に degraded(劣化情報)があれば、検索品質が落ちている — 回答にその旨を添える\n\
\n\
■ 所有(2026-08-10 改定: ノートは「生まれ」で領分が決まる)\n\
- origin: human のノート(ユーザーのメモ)= ユーザーの領分。あなたは読む・つなげる・\
気づきを知らせるまで。本文の変更・削除はできない(提案したいことがあれば会話で伝える)\n\
- origin: agent のノート(AI 由来)= あなたの領分。update / remove で直接手入れしてよい\
(内容の更新・統合・古くなったノートの削除)。ユーザー側からは読むだけになっている\n\
\n\
■ 育てる(会話の終わりに)\n\
- 恒久的に残す価値のある知見・決定・事実が新しく生まれたら、会話の終わりに propose での\
下書き起票を提案する(勝手に起票せず、一言添えて承諾を得るのが基本。ユーザーが起票を\
指示したら即実行してよい)\n\
- 新規は下書き(draft)から。確定・却下はユーザーがアプリの受信箱で行う — あなたは確定を\
促さなくてよい\n\
- 既存の AI ノートの手入れは update / remove で直接。大きな書き換えは一言添えてから\n\
\n\
■ タグ(体系は会話で育てる — 2026-08-10 本人方針)\n\
- タグの種類・役割はアプリが決めない。ユーザーとの会話で合意して育てる\n\
- ユーザーが決めたタグとその役割は「タグ運用」ノートに記録し(無ければ起票を提案)、\
以後それに従う — ユーザー決定のタグを勝手に変更・削除しない\n\
- それ以外のタグはあなたの裁量で付与・統合・改名・整理してよい(update で tags を\
書き換える。まとまった整理をしたときは会話で一言報告)\n\
- 新しいタグを乱発しない — まず既存の語彙(search で確認できる)に揃える\n\
\n\
■ 書き方(propose の質)\n\
- title: 内容が一意に分かる具体的なもの(「メモ」「まとめ」だけは不可)\n\
- description: 一文要約(検索スニペットと一覧に使われる)\n\
- body: この会話を読んでいない未来の読者に向けて自己完結で。結論だけでなく、経緯・根拠・\
出典(会話の文脈)を短く含める。関連する既存ノートがあれば markdown リンク(/path.md)で\
つなぐ\n\
- tags: 2〜4個。既存ノートに付いているタグに揃える";

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
            "description": "ナレッジベースを検索する(全文+意味+リンク近傍のハイブリッド)。ユーザー個人に関する話題ではまずこれを引く。キーワード列挙でも文まるごとでもよい(言い回しが違っても意味で当たる)。結果に劣化情報(degraded)があれば検索品質が落ちている — ユーザーへの回答にその旨を添える。",
            "inputSchema": {"type": "object", "properties": {
                "query": {"type": "string", "description": "検索語(キーワード列挙または自然文)"},
                "limit": {"type": "integer", "description": "最大件数(既定 8)"}
            }, "required": ["query"]}
        },
        {
            "name": "get",
            "description": "ノートの全文(frontmatter + 本文)を取得する。search でヒットしたノートは必ずこれで全文を読んでから答える。note を省略すると、ユーザーがアプリでいま開いているノート(「このノート」)を返す。",
            "inputSchema": {"type": "object", "properties": {
                "note": {"type": "string", "description": "ノート ID(例: notes/foo)。省略時はいま開いているノート"}
            }}
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
            "description": "会話から得た恒久的な知見・決定を下書きノートとして起票する。起票は下書き(draft)までで、確定・却下はユーザーがアプリの受信箱で行う。本文はこの会話を読んでいない未来の読者向けに自己完結の Markdown で書き、経緯・根拠・会話出典の要約を含め、関連する既存ノートは /path.md 形式のリンクでつなぐ。既存ノートの更新提案もこれで(差分・追記案を本文に)。",
            "inputSchema": {"type": "object", "properties": {
                "title": {"type": "string", "description": "内容が一意に分かる具体的なタイトル"},
                "body": {"type": "string", "description": "本文(Markdown・自己完結)"},
                "description": {"type": "string", "description": "一文要約(一覧・検索スニペットに使われる)"},
                "tags": {"type": "array", "items": {"type": "string"}, "description": "分類タグ 2〜4個(既存タグに揃える)"}
            }, "required": ["title", "body"]}
        },
        {
            "name": "update",
            "description": "AI 由来のノート(origin: agent)を直接更新する。指定したフィールドだけ置き換わる。ユーザーのメモ(origin: human)は更新できない(読むだけ)。大きな書き換えは会話で一言添えてから。",
            "inputSchema": {"type": "object", "properties": {
                "note": {"type": "string", "description": "ノート ID"},
                "title": {"type": "string"},
                "body": {"type": "string", "description": "本文全体の置き換え(Markdown・自己完結)"},
                "description": {"type": "string"},
                "tags": {"type": "array", "items": {"type": "string"}}
            }, "required": ["note"]}
        },
        {
            "name": "remove",
            "description": "AI 由来のノート(origin: agent)を削除する(git 履歴には残る)。重複・陳腐化したノートの整理に使う。ユーザーのメモ(origin: human)は削除できない。削除前に会話で一言添えるのが基本。",
            "inputSchema": {"type": "object", "properties": {
                "note": {"type": "string", "description": "ノート ID"}
            }, "required": ["note"]}
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
        Ok(_) => pull_note.or_else(|| crate::index::embed_step(&conn)),
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
            let id = match args.get("note").and_then(|v| v.as_str()) {
                Some(id) => id.to_string(),
                // 引数なし =「このノート」(アプリでいま開いているノート。FR-A5 の文脈受け渡し)
                None => crate::connect::current_note(vault).ok_or_else(|| {
                    anyhow::anyhow!("いま開いているノートが無い(note 引数で ID を指定)")
                })?,
            };
            let note = vault.read_note(&id)?;
            let attachments = vault.list_attachments(&id);
            let attach_line = if attachments.is_empty() {
                String::new()
            } else {
                format!(
                    "(添付: {} — 本文からは /{id}.files/<名前> で参照)\n",
                    attachments.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")
                )
            };
            Ok(format!("(note: {id})\n{attach_line}{}", note.to_file_string()?))
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
        "update" => {
            let id = args.get("note").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("note が必要"))?;
            let tags: Option<Vec<String>> = args.get("tags").and_then(|v| v.as_array()).map(|a| {
                a.iter().filter_map(|t| t.as_str().map(String::from)).collect()
            });
            vault.agent_update_note(
                id,
                args.get("title").and_then(|v| v.as_str()),
                args.get("body").and_then(|v| v.as_str()),
                args.get("description").and_then(|v| v.as_str()),
                tags.as_deref(),
                client,
            )?;
            Ok(format!("更新した: {id}"))
        }
        "remove" => {
            let id = args.get("note").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("note が必要"))?;
            vault.agent_delete_note(id, client)?;
            Ok(format!("削除した: {id}(履歴には残る)"))
        }
        other => anyhow::bail!("unknown tool: {other}"),
    }
}
