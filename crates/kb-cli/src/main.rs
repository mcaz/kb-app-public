//! kb — エンジニア向け CLI(FR-C4/C5 の口)。`kb mcp` で MCP サーバー起動。

use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use kb_core::index::{open_db, sync};
use kb_core::registry::Registry;
use kb_core::search::recent;
use kb_core::vault::Vault;
use kb_core::OWNER_ACTOR;

#[derive(Parser)]
#[command(name = "kb", version, about = "kb-app core CLI(そのままでも使えるナレッジベース)")]
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
    /// 人間のメモを作成(本文は --body か stdin)
    New {
        title: String,
        #[arg(long)]
        body: Option<String>,
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
    /// AI 由来の下書き起票(テスト用の口。通常は MCP 経由)
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
    },
    /// 下書きの確定(draft → stable+verified)
    Confirm { note: String },
    /// 退役(status: deprecated)
    Archive { note: String },
    /// 既存 Markdown KB からの移植(互換レイヤ)。`<dir>` か `<dir>=<接頭辞>` を複数指定可。
    /// リンク解決はソース横断
    Import {
        /// 例: ~/old-kb/vault ~/old-team/vault=team
        sources: Vec<String>,
    },
    /// 索引の増分 sync
    Sync,
    /// MCP サーバーを stdio で起動
    Mcp {
        /// generated.by に刻むクライアント actor(例: claude-desktop/claude-fable-5)
        #[arg(long, default_value = "mcp-client/unknown")]
        client: String,
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
            std::io::stdin().read_to_string(&mut buf).context("stdin 読み取り")?;
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
                    None => dirs::home_dir().context("home が特定できない")?.join("kb").join(&name),
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
                    let mark = if reg.default.as_deref() == Some(v.name.as_str()) { "*" } else { " " };
                    println!("{mark} {}\t{}", v.name, v.path.display());
                }
            }
        },
        Command::New { title, body } => {
            let vault = open_vault(cli.vault.as_deref())?;
            let body = body_or_stdin(body)?;
            let id = vault.new_human_note(&title, &body, OWNER_ACTOR)?;
            let conn = open_db(&vault)?;
            sync(&vault, &conn)?;
            println!("{id}");
        }
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
        Command::Propose { title, body, description, tags, client } => {
            let vault = open_vault(cli.vault.as_deref())?;
            let body = body_or_stdin(body)?;
            let id = vault.propose(&title, &body, description.as_deref(), &tags, &client)?;
            println!("{id}");
        }
        Command::Confirm { note } => {
            let vault = open_vault(cli.vault.as_deref())?;
            vault.confirm(&note, OWNER_ACTOR)?;
            println!("confirmed: {note}");
        }
        Command::Archive { note } => {
            let vault = open_vault(cli.vault.as_deref())?;
            vault.archive(&note)?;
            println!("archived: {note}");
        }
        Command::Import { sources } => {
            let vault = open_vault(cli.vault.as_deref())?;
            let parsed: Vec<kb_core::import::Source> = sources
                .iter()
                .map(|s| {
                    let (dir, prefix) = match s.split_once('=') {
                        Some((d, p)) => (d, p.to_string()),
                        None => (s.as_str(), String::new()),
                    };
                    let label = if prefix.is_empty() { dir.to_string() } else { prefix.clone() };
                    kb_core::import::Source { root: PathBuf::from(dir), label, prefix }
                })
                .collect();
            let report = kb_core::import::import(&vault, &parsed)?;
            println!("移植: {} 本", report.imported.len());
            for s in &report.skipped {
                println!("スキップ: {s}");
            }
            if !report.unresolved_links.is_empty() {
                println!("未解決リンク({}件・原文のまま): {}", report.unresolved_links.len(), report.unresolved_links.join(", "));
            }
            let conn = open_db(&vault)?;
            sync(&vault, &conn)?;
        }
        Command::Sync => {
            let vault = open_vault(cli.vault.as_deref())?;
            let conn = open_db(&vault)?;
            let n = sync(&vault, &conn)?;
            println!("synced: {n} note(s) updated");
        }
        Command::Mcp { client } => {
            let vault = open_vault(cli.vault.as_deref())?;
            kb_core::mcp::serve(&vault, &client)?;
        }
    }
    Ok(())
}
