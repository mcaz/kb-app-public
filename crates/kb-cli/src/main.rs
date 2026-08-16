//! kb — エンジニア向け CLI(FR-C4/C5 の口)。`kb mcp` で MCP サーバー起動。

use std::fs;
use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use kb_core::index::{open_db, sync};
use kb_core::registry::Registry;
use kb_core::search::recent;
use kb_core::vault::{NoteProposal, Vault};

#[derive(Parser)]
#[command(
    name = "kb",
    version,
    about = "kb-app core CLI(そのままでも使えるナレッジベース)"
)]
struct Cli {
    /// 対象 vault(レジストリ名)。省略時は KB_VAULT か既定 vault
    #[arg(long, global = true)]
    vault: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// vault の作成・一覧
    Vault {
        #[command(subcommand)]
        command: VaultCommand,
    },
    /// 検索(全文+リンク近傍)
    Search {
        query: Vec<String>,
        #[arg(long, default_value_t = 8)]
        limit: usize,
        /// 語を OR 結合(文まるごとの前出し用)
        #[arg(long)]
        any: bool,
    },
    /// ノート全文を表示
    Get { note: String },
    /// 最近のノート
    Recent {
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// AI 管理ノートを起票(本文は --body か stdin。通常は MCP 経由)
    Propose {
        #[arg(long)]
        title: String,
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
        #[arg(long, default_value = "cli/unknown")]
        client: String,
        /// 語彙にない新語を許す(既定は拒否 — 契約1の強制点)
        #[arg(long)]
        allow_new_tags: bool,
    },
    /// 既存 Markdown KB からの移植(互換レイヤ)。`<dir>` か `<dir>=<接頭辞>` を複数指定可。
    /// リンク解決はソース横断
    Import {
        /// 例: ~/old-kb/vault ~/old-team/vault=team
        sources: Vec<String>,
        /// 語彙にない新語を明示的に許す(個数・形の契約は常に強制)
        #[arg(long)]
        allow_new_tags: bool,
    },
    /// お手入れ(FR-C7)の一覧・承諾・却下(エンジニア向けの口)
    Care {
        #[command(subcommand)]
        command: CareCommand,
    },
    /// 旧添付(<ノート ID>.files/)を台帳に載せる(既定は棚卸しだけ)
    MigrateFiles {
        /// 実際に書き込む。付けなければ何も書かない
        #[arg(long)]
        apply: bool,
    },
    /// 索引の増分 sync
    Sync,
    /// 正本の再現性を検査・論理 export
    Storage {
        #[command(subcommand)]
        command: StorageCommand,
    },
    /// かしこい検索(段1: 埋め込み内蔵)の管理
    Embed {
        #[command(subcommand)]
        command: EmbedCommand,
    },
    /// MCP サーバーを stdio で起動
    Mcp {
        /// generated.by に刻むクライアント actor(例: claude-desktop/claude-fable-5)
        #[arg(long, default_value = "mcp-client/unknown")]
        client: String,
    },
}

#[derive(Subcommand)]
enum CareCommand {
    /// 検知を回して未処理の提案を一覧
    List,
    /// 「このまま」(同じ提案は出なくなる)
    Dismiss { key: String },
}

#[derive(Subcommand)]
enum EmbedCommand {
    /// モデル(bge-m3 int8、約560MB)を導入し、全ノートを埋め込む
    Enable,
    /// 導入状態とカバレッジ
    Status,
}

#[derive(Subcommand)]
enum StorageCommand {
    /// clone 後にも再現すべき論理状態を読み取り専用で検査
    Verify,
    /// backend 非依存の比較用 JSON を出力(stdout または --output)
    Export {
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum VaultCommand {
    /// 新しい vault を作成して登録
    Create {
        name: String,
        /// 保存先(省略時 ~/kb/<name>)
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// 登録済み vault の一覧
    List,
}

fn open_vault(name: Option<&str>) -> Result<Vault> {
    let reg = Registry::load()?;
    let path = reg.resolve(name)?;
    Vault::open(path)
}

fn body_or_stdin(body: Option<String>) -> Result<String> {
    match body {
        Some(b) => Ok(b),
        None => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("stdin 読み取り")?;
            Ok(buf)
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Vault { command } => match command {
            VaultCommand::Create { name, path } => {
                let path = match path {
                    Some(p) => p,
                    None => dirs::home_dir()
                        .context("home が特定できない")?
                        .join("kb")
                        .join(&name),
                };
                let vault = Vault::create(&path)?;
                let mut reg = Registry::load()?;
                reg.add(&name, vault.root.clone())?;
                reg.save()?;
                println!("vault {name} を作成: {}", vault.root.display());
            }
            VaultCommand::List => {
                let reg = Registry::load()?;
                for v in &reg.vaults {
                    let mark = if reg.default.as_deref() == Some(v.name.as_str()) {
                        "*"
                    } else {
                        " "
                    };
                    println!("{mark} {}\t{}", v.name, v.path.display());
                }
            }
        },
        Command::Search { query, limit, any } => {
            let vault = open_vault(cli.vault.as_deref())?;
            let conn = open_db(&vault)?;
            sync(&vault, &conn)?;
            let out = kb_core::search::search_mode(&conn, &query.join(" "), limit, any);
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        Command::Get { note } => {
            let vault = open_vault(cli.vault.as_deref())?;
            print!("{}", vault.read_note(&note)?.to_file_string()?);
        }
        Command::Recent { limit } => {
            let vault = open_vault(cli.vault.as_deref())?;
            let conn = open_db(&vault)?;
            sync(&vault, &conn)?;
            for h in recent(&conn, limit)? {
                println!("{}\t{}\t{}", h.id, h.status, h.title.unwrap_or_default());
            }
        }
        Command::Propose {
            title,
            body,
            description,
            tags,
            client,
            allow_new_tags,
        } => {
            let vault = open_vault(cli.vault.as_deref())?;
            let body = body_or_stdin(body)?;
            let conn = open_db(&vault)?;
            sync(&vault, &conn)?;
            let id = vault.propose(
                &conn,
                NoteProposal {
                    title: &title,
                    body: &body,
                    description: description.as_deref(),
                    tags: &tags,
                    allow_new_tags,
                    client: &client,
                },
            )?;
            println!("{id}");
        }
        Command::Import {
            sources,
            allow_new_tags,
        } => {
            let vault = open_vault(cli.vault.as_deref())?;
            let parsed: Vec<kb_core::import::Source> = sources
                .iter()
                .map(|s| {
                    let (dir, prefix) = match s.split_once('=') {
                        Some((d, p)) => (d, p.to_string()),
                        None => (s.as_str(), String::new()),
                    };
                    let label = if prefix.is_empty() {
                        dir.to_string()
                    } else {
                        prefix.clone()
                    };
                    kb_core::import::Source {
                        root: PathBuf::from(dir),
                        label,
                        prefix,
                    }
                })
                .collect();
            let report = kb_core::import::import(&vault, &parsed, allow_new_tags)?;
            println!("移植: {} 本", report.imported.len());
            for s in &report.skipped {
                println!("スキップ: {s}");
            }
            if !report.unresolved_links.is_empty() {
                println!(
                    "未解決リンク({}件・原文のまま): {}",
                    report.unresolved_links.len(),
                    report.unresolved_links.join(", ")
                );
            }
            let conn = open_db(&vault)?;
            sync(&vault, &conn)?;
        }
        Command::Care { command } => {
            let vault = open_vault(cli.vault.as_deref())?;
            let conn = open_db(&vault)?;
            sync(&vault, &conn)?;
            match command {
                CareCommand::List => {
                    kb_core::care::detect(&conn, &vault)?;
                    for p in kb_core::care::list_open(&conn)? {
                        println!("{}\t{}\t{}", p.key, p.kind, p.detail);
                    }
                }
                CareCommand::Dismiss { key } => {
                    kb_core::care::dismiss(&conn, &key)?;
                    println!("dismissed: {key}");
                }
            }
        }
        Command::MigrateFiles { apply } => {
            let vault = open_vault(cli.vault.as_deref())?;
            let workspace_id = kb_core::workspace::workspace_id(&vault)?;
            let ledger = kb_core::ledger::Ledger::open(&vault, &workspace_id)?;

            let pending = kb_core::migrate::survey(&vault, &ledger)?;
            if pending.is_empty() {
                println!("移行するものはない(すべて台帳に載っている)");
                return Ok(());
            }
            for p in &pending {
                println!("{}\t{} バイト\t{}", p.link, p.size, p.note_id);
            }
            if !apply {
                // 既定を棚卸しにするのは、実データに書く操作だから(--apply で実行)
                println!("\n{} 件。書き込むには --apply", pending.len());
                return Ok(());
            }
            let at = kb_core::frontmatter::now_iso();
            let done = kb_core::migrate::migrate(&vault, &ledger, &workspace_id, &at)?;
            for d in &done {
                println!("載せた: {} → {} (参照名 {})", d.file_name, d.id, d.ref_name);
            }
            println!(
                "\n{} 件。実体は動かしていない(旧ファイルはそのまま)",
                done.len()
            );
        }
        Command::Sync => {
            let vault = open_vault(cli.vault.as_deref())?;
            let conn = open_db(&vault)?;
            let n = sync(&vault, &conn)?;
            println!("synced: {n} note(s) updated");
        }
        Command::Storage { command } => {
            let vault = open_vault(cli.vault.as_deref())?;
            match command {
                StorageCommand::Verify => {
                    let report = kb_core::storage_contract::verify(&vault)?;
                    println!("{}", serde_json::to_string_pretty(&report)?);
                }
                StorageCommand::Export { output } => {
                    let export = kb_core::storage_contract::export(&vault)?;
                    let json = serde_json::to_string_pretty(&export)?;
                    if let Some(path) = output {
                        fs::write(&path, format!("{json}\n"))
                            .with_context(|| format!("export を書けない: {}", path.display()))?;
                        println!("{}", path.display());
                    } else {
                        println!("{json}");
                    }
                }
            }
        }
        Command::Embed { command } => {
            let vault = open_vault(cli.vault.as_deref())?;
            let conn = open_db(&vault)?;
            sync(&vault, &conn)?;
            match command {
                EmbedCommand::Enable => {
                    if !kb_core::embed::model_installed() {
                        println!("モデルをダウンロードします(約560MB)…");
                        kb_core::embed::install_model()?;
                    }
                    println!("埋め込みを開始…");
                    loop {
                        let rest = kb_core::embed::embed_pending(&conn, 10)?;
                        println!("  残り {rest} 件");
                        if rest == 0 {
                            break;
                        }
                    }
                    println!("かしこい検索が有効になりました");
                }
                EmbedCommand::Status => {
                    let s = kb_core::search::stats(&conn)?;
                    println!(
                        "モデル: {} / 埋め込み済み: {}/{}",
                        if s.embed_enabled {
                            "導入済み"
                        } else {
                            "未導入"
                        },
                        s.embedded,
                        s.total
                    );
                }
            }
        }
        Command::Mcp { client } => {
            let vault = open_vault(cli.vault.as_deref())?;
            kb_core::mcp::serve(&vault, &client)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::Cli;

    /// ノートの書き込みは AI 経路だけ。CLI に人間所有の抜け道を戻さない。
    #[test]
    fn human_note_write_commands_are_not_exposed() {
        let command = Cli::command();
        for removed in ["new", "archive", "delete"] {
            assert!(
                command.find_subcommand(removed).is_none(),
                "{removed} を CLI に公開してはいけない"
            );
        }
        assert!(command.find_subcommand("propose").is_some());
    }

    #[test]
    fn import_requires_an_explicit_flag_to_allow_new_vocabulary() {
        let command = Cli::command();
        let import = command.find_subcommand("import").unwrap();
        assert!(
            import
                .get_arguments()
                .any(|argument| argument.get_id() == "allow_new_tags")
        );
    }
}
