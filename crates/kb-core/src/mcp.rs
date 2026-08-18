//! MCP サーバー(stdio、newline-delimited JSON-RPC 2.0)。
//! 公開ツールは search / get / recent / propose / update / remove / attach。
//! 人間のノートは変更できない(所有ガード)。
//!
//! v0.1 は手組みの最小実装(依存最小・同期 I/O)。リモート化(Streamable HTTP)の
//! 段階で公式 Rust SDK(rmcp)への載せ替えを再評価する(ADR-0001 スタック表)。

use std::io::{BufRead, Write};
use std::str::FromStr;

use anyhow::{Context, Result};
use base64::Engine as _;
use serde_json::{Value, json};

use crate::index::{open_db, sync_with_degradations};
use crate::search::{recent, search};
use crate::vault::{NoteProposal, NoteUpdate, Vault};

const PROTOCOL_FALLBACK: &str = "2025-06-18";
const KB_DISABLED_CODE: &str = "kb_disabled";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServeOptions {
    /// tool callの前にGitHub pullを試す。短命な自動retrieval processではfalseにし、
    /// Keychain認証やremote I/Oを発話回数へ結び付けない。
    pub remote_sync: bool,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self { remote_sync: true }
    }
}

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
全文を読んでから答える。`[kb-app 自動retrieval — MCP]` が届いていれば、その検索・リンク展開・本文取得は\
実行済みなので、文脈が不足するときだけ追加検索する。本文中の /path.md リンクは必要なら辿る。\
「このノート」=引数なしの get。該当なしは正常(その旨を添える)。degraded があれば回答に添える。\n\
【育てる】残す価値のある知見・決定が生まれたら、会話の終わりに propose を提案(承諾を得て\
から)。既存ノートの手入れは update / remove で直接(大きな変更は一言添える)。\
本文は未来の読者向けに自己完結で(経緯・出典・関連ノートへの /path.md リンク)。\n\
【ファイル】会話で作成・受領した画像や文書を既存ノートの持ち物にするときは attach を使う。\
ローカルpathではなく内容をBase64で渡す。保存先・持ち出し区分・来歴はkb-appが固定する。\n\
【関連】ノート間の関連づけはあなたの領分。get の「近いノート」を見て、本当に関連するなら\
update で本文に /path.md リンクを足す(ユーザーに可否を尋ねる形にはしない)。\n\
【タグ】体系は会話でユーザーと合意して育てる(暫定・要確認といった扱いもタグで表す — \
アプリに下書き状態は無い)。合意済み(「タグ運用」ノート。無ければ起票を\
提案)は勝手に変えない。それ以外はあなたの裁量で付与・統合・整理してよい(まとめて整理したら\
一言報告)。新語を乱発せず既存語彙に揃える。";

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
    mut open_vault: impl FnMut() -> Result<Vault>,
) -> Result<()> {
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
            options.remote_sync,
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
    remote_sync: bool,
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
                "capabilities": if enabled {
                    json!({"tools": {}, "prompts": {}})
                } else {
                    json!({"tools": {}})
                },
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
        "tools/list" => Ok(Some(json!({"tools": tool_definitions()}))),
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
            let args = params
                .and_then(|p| p.get("arguments"))
                .cloned()
                .unwrap_or(json!({}));
            let vault = vault.context("Vault is unavailable")?;
            let output = call_tool(vault, client, name, &args, remote_sync);
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
                "any": {"type": "boolean", "description": "語をOR結合する(発話全文の自動retrieval用)"},
                "include_documents": {"type": "boolean", "description": "検索seedとリンク近傍の本文を予算内で同じ検索応答に含める(自動retrieval用)"}
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
            "name": "attach",
            "description": "会話で作成・受領した小さなファイルを既存ノートへ添付。pathではなくBase64内容だけを渡し、保存先・区分・来歴はkb-appが固定する(上限16 MiB)。",
            "inputSchema": {"type": "object", "properties": {
                "note": {"type": "string", "description": "ひもづけ先の既存ノート ID"},
                "file_name": {"type": "string", "description": "表示ファイル名(区切り文字なし)"},
                "content_base64": {"type": "string", "description": "ファイル内容の標準Base64"},
                "ref_name": {"type": "string", "description": "本文から安定参照する任意の参照名(workspace内で一意)"}
            }, "required": ["note", "file_name", "content_base64"]}
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

fn call_tool(
    vault: &Vault,
    client: &str,
    name: &str,
    args: &Value,
    remote_sync: bool,
) -> Result<ToolOutput> {
    // メッセージのやり取りの際に pull(複数デバイス同期・FR-A6 改定)。
    // スロットリング付き・失敗は劣化情報(fail-open)。自動retrievalの短命processは
    // remote_sync=falseで、発話ごとのKeychainアクセスとremote I/Oを行わない。
    let mut degraded = remote_degradations(remote_sync, || crate::connect::pull_if_stale(vault));
    let conn = open_db(vault)?;
    // DBが実行時正本なので、全文取得は検索時のsnapshotをそのまま読む。索引の追い付きと
    // 埋め込み生成は検索時に一度だけ行い、上位候補ごとのgetでは繰り返さない。
    if name == "search" {
        match sync_with_degradations(vault, &conn) {
            Ok(report) => {
                degraded.extend(report.degraded);
                degraded.extend(crate::index::embed_step(&conn));
            }
            Err(error) => degraded.push(crate::degradation::Degradation::IndexSync {
                detail: error.to_string(),
            }),
        }
    }
    match name {
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
                match crate::retrieval::context_documents(&snapshot, &hit_ids, options) {
                    Ok(bundle) => Some(bundle),
                    Err(error) => {
                        // リンク表だけが壊れても検索seed本文は返す。正常な0リンクとは
                        // ContextRetrieval degradationで区別する。
                        out.degraded
                            .push(crate::degradation::Degradation::ContextRetrieval {
                                detail: error.to_string(),
                            });
                        Some(crate::retrieval::context_documents(
                            &snapshot,
                            &hit_ids,
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
            Ok(ToolOutput {
                text: format!(
                    "(note: {id})\n{legacy_line}{managed_line}{sim_line}{}{}",
                    degradation_text(&degraded),
                    note.to_file_string()?
                ),
                structured: Some(json!({
                    "note": id,
                    "artifacts": artifact_rows,
                    "legacy_attachments": legacy_names,
                    "degraded": degraded,
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
            let structured = json!({
                "note": note_id,
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
            });
            Ok(ToolOutput {
                text: with_degradations(
                    format!(
                        "添付した: {} → {} (artifact {})",
                        file_name, note_id, taken.manifest.id
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
            vault.agent_delete_note(&conn, id, client)?;
            Ok(ToolOutput::text(with_degradations(
                format!("削除した: {id}(履歴には残る)"),
                &degraded,
            )))
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
        assert_eq!(
            initialized["capabilities"],
            serde_json::json!({"tools": {}})
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
        assert_eq!(tools, serde_json::json!({"tools": tool_definitions()}));
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
        let initialized = handle(Some(&vault), "test/client", true, true, "initialize", None)
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
        let tools = tool_definitions();
        let definitions = tools.as_array().unwrap();
        assert_eq!(definitions.len(), 7);
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
