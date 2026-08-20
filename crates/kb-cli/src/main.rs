//! kb — エンジニア向け CLI(FR-C4/C5 の口)。`kb mcp` で MCP サーバー起動。

use std::fs;
use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use kb_core::authority::{Authority, AuthorityRole, AuthorityStatus, NoteNamespace};
use kb_core::index::{open_db, open_db_read_only, sync};
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
        #[arg(long, value_enum)]
        namespace: NamespaceArg,
        #[arg(long, value_enum)]
        role: AuthorityRoleArg,
        #[arg(long, value_enum, default_value = "active")]
        authority_status: AuthorityStatusArg,
        /// 同じ主題・適用範囲のcanonicalを一意にする安定key
        #[arg(long)]
        scope: String,
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
    /// snapshot固定・read-onlyの継続蒸留plan
    Distill {
        #[command(subcommand)]
        command: DistillCommand,
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
    /// 再現可能な品質評価
    Eval {
        #[command(subcommand)]
        command: EvalCommand,
    },
    /// MCP サーバーを stdio で起動
    Mcp {
        /// generated.by に刻むクライアント actor(例: claude-desktop/claude-fable-5)
        #[arg(long, default_value = "mcp-client/unknown")]
        client: String,
    },
    /// AI 連携向けの端末設定を読み取る
    Settings {
        #[command(subcommand)]
        command: SettingsCommand,
    },
}

#[derive(Subcommand)]
enum SettingsCommand {
    /// 全体とクライアント別の設定から KB 利用可否を JSON で返す
    AiEnabled {
        #[arg(long)]
        client: String,
    },
    /// Codex / Claude Code の OS レベル保護状態を JSON で返す
    AiGuardStatus,
    /// macOS の管理者認証を経て OS レベル保護を導入・更新する
    InstallAiGuard,
}

#[derive(Subcommand)]
enum CareCommand {
    /// 検知を回して未処理の提案を一覧
    List,
    /// 「このまま」(同じ提案は出なくなる)
    Dismiss { key: String },
}

#[derive(Subcommand)]
enum DistillCommand {
    /// DBの同一snapshotから決定的な候補planを作る(書き込み・pull・syncなし)
    Plan {
        #[arg(long, value_enum, default_value_t = ReportFormat::Json)]
        format: ReportFormat,
        /// 省略時はstdoutへ出力
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// 前回checkpointとの差分worksetと現在の受入gateをread-onlyで監査
    Audit {
        /// 前回audit reportまたはkb-app.distillation-checkpoint/v1 JSON
        #[arg(long)]
        baseline: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = ReportFormat::Json)]
        format: ReportFormat,
        /// 省略時はstdoutへ出力
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// 最後に受入成功したcheckpointを使う継続cadence
    Cadence {
        #[command(subcommand)]
        command: DistillationCadenceCommand,
    },
    /// planと全input hashを再照合し、既存ノートのsemantic waveをatomicに適用
    Apply {
        /// kb-app.distillation-execution-request/v1 JSON
        #[arg(long)]
        input: PathBuf,
        #[arg(long, default_value = "cli/unknown")]
        client: String,
    },
    /// execution直後から対象が変わっていないwaveを一括復元
    Rollback {
        #[arg(long)]
        execution_id: String,
        #[arg(long, default_value = "cli/unknown")]
        client: String,
    },
}

#[derive(Subcommand)]
enum DistillationCadenceCommand {
    /// 現在planとの差分と時間間隔からdue laneを読み取り専用で確認
    Status {
        #[arg(long, value_enum, default_value_t = ReportFormat::Json)]
        format: ReportFormat,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// due lane、または明示したlaneの監査を実行して成功時だけcheckpointを進める
    Run {
        #[arg(long, value_enum)]
        lane: Option<DistillationCadenceLaneArg>,
        #[arg(long, value_enum, default_value_t = ReportFormat::Json)]
        format: ReportFormat,
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum EmbedCommand {
    /// モデル(bge-m3 int8、約560MB)を導入し、全ノートを埋め込む
    Enable,
    /// 導入状態とカバレッジ
    Status,
}

#[derive(Subcommand)]
enum EvalCommand {
    /// Golden Queryで旧top3とリンク連鎖retrievalを比較する
    Retrieval {
        /// schemas/retrieval-eval.schema.jsonに従うGolden Query JSON
        #[arg(long)]
        cases: PathBuf,
        #[arg(long, value_enum, default_value_t = ReportFormat::Markdown)]
        format: ReportFormat,
        /// 省略時はstdoutへ出力
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// 固定20ケースについて4方式のRule配信contextを生成する
    RuleDeliveryPlan {
        /// schemas/rule-delivery-eval.schema.jsonに従うsuite
        #[arg(long)]
        suite: PathBuf,
        #[arg(long, value_enum)]
        mode: RuleDeliveryModeArg,
        #[arg(long, value_enum, default_value_t = ReportFormat::Json)]
        format: ReportFormat,
        /// 省略時はstdoutへ出力
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// 固定suiteから実KBと分離した使い捨てVaultを作る
    RuleDeliveryFixture {
        #[arg(long)]
        suite: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// client実行traceを機械判定する
    RuleDeliveryScore {
        #[arg(long)]
        suite: PathBuf,
        #[arg(long)]
        traces: PathBuf,
        #[arg(long, value_enum, default_value_t = ReportFormat::Markdown)]
        format: ReportFormat,
        /// 省略時はstdoutへ出力
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ReportFormat {
    Json,
    Markdown,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum DistillationCadenceLaneArg {
    AfterWrite,
    Daily,
    Weekly,
    Monthly,
}

impl From<DistillationCadenceLaneArg> for kb_core::distillation_cadence::DistillationCadenceLane {
    fn from(value: DistillationCadenceLaneArg) -> Self {
        match value {
            DistillationCadenceLaneArg::AfterWrite => Self::AfterWrite,
            DistillationCadenceLaneArg::Daily => Self::Daily,
            DistillationCadenceLaneArg::Weekly => Self::Weekly,
            DistillationCadenceLaneArg::Monthly => Self::Monthly,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum RuleDeliveryModeArg {
    SemanticOnly,
    RulesTopK,
    AlwaysTopic,
    AlwaysTopicEvent,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum NamespaceArg {
    Entities,
    Initiatives,
    Decisions,
    Procedures,
    Records,
    Knowledge,
}

impl From<NamespaceArg> for NoteNamespace {
    fn from(value: NamespaceArg) -> Self {
        match value {
            NamespaceArg::Entities => Self::Entities,
            NamespaceArg::Initiatives => Self::Initiatives,
            NamespaceArg::Decisions => Self::Decisions,
            NamespaceArg::Procedures => Self::Procedures,
            NamespaceArg::Records => Self::Records,
            NamespaceArg::Knowledge => Self::Knowledge,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum AuthorityRoleArg {
    Canonical,
    Record,
    Proposal,
}

impl From<AuthorityRoleArg> for AuthorityRole {
    fn from(value: AuthorityRoleArg) -> Self {
        match value {
            AuthorityRoleArg::Canonical => Self::Canonical,
            AuthorityRoleArg::Record => Self::Record,
            AuthorityRoleArg::Proposal => Self::Proposal,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum AuthorityStatusArg {
    Active,
    Historical,
    Superseded,
}

impl From<AuthorityStatusArg> for AuthorityStatus {
    fn from(value: AuthorityStatusArg) -> Self {
        match value {
            AuthorityStatusArg::Active => Self::Active,
            AuthorityStatusArg::Historical => Self::Historical,
            AuthorityStatusArg::Superseded => Self::Superseded,
        }
    }
}

impl From<RuleDeliveryModeArg> for kb_core::rule_delivery_eval::DeliveryMode {
    fn from(value: RuleDeliveryModeArg) -> Self {
        match value {
            RuleDeliveryModeArg::SemanticOnly => Self::SemanticOnly,
            RuleDeliveryModeArg::RulesTopK => Self::RulesTopK,
            RuleDeliveryModeArg::AlwaysTopic => Self::AlwaysTopic,
            RuleDeliveryModeArg::AlwaysTopicEvent => Self::AlwaysTopicEvent,
        }
    }
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
            let conn = open_db(&vault)?;
            sync(&vault, &conn)?;
            print!(
                "{}",
                vault.read_note_from_db(&conn, &note)?.to_file_string()?
            );
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
            namespace,
            role,
            authority_status,
            scope,
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
                    authority: Authority {
                        namespace: namespace.into(),
                        role: role.into(),
                        status: authority_status.into(),
                        scope,
                    },
                    relations: Vec::new(),
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
        Command::Distill { command } => {
            let vault = open_vault(cli.vault.as_deref())?;
            match command {
                DistillCommand::Plan { format, output } => {
                    let conn = open_db_read_only(&vault)?;
                    let plan = kb_core::distillation::plan(&conn)?;
                    let rendered = match format {
                        ReportFormat::Json => serde_json::to_string_pretty(&plan)?,
                        ReportFormat::Markdown => kb_core::distillation::render_markdown(&plan),
                    };
                    write_eval_output(output.as_ref(), &rendered, "Distillation plan")?;
                }
                DistillCommand::Audit {
                    baseline,
                    format,
                    output,
                } => {
                    let baseline = baseline
                        .as_ref()
                        .map(read_distillation_checkpoint)
                        .transpose()?;
                    let conn = open_db_read_only(&vault)?;
                    let report =
                        kb_core::distillation_audit::audit(&vault, &conn, baseline.as_ref())?;
                    let rendered = match format {
                        ReportFormat::Json => serde_json::to_string_pretty(&report)?,
                        ReportFormat::Markdown => {
                            kb_core::distillation_audit::render_markdown(&report)
                        }
                    };
                    write_eval_output(output.as_ref(), &rendered, "Distillation audit")?;
                }
                DistillCommand::Cadence { command } => match command {
                    DistillationCadenceCommand::Status { format, output } => {
                        let conn = open_db_read_only(&vault)?;
                        let status = kb_core::distillation_cadence::status(&vault, &conn)?;
                        let rendered = match format {
                            ReportFormat::Json => serde_json::to_string_pretty(&status)?,
                            ReportFormat::Markdown => {
                                let due = status
                                    .due_lanes()
                                    .iter()
                                    .map(|lane| lane.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                format!(
                                    "# Distillation cadence status\n\n- checked: {}\n- due: {}\n- current: `{}`\n- accepted: `{}`",
                                    status.checked_at,
                                    if due.is_empty() { "none" } else { &due },
                                    status.current_checkpoint_id,
                                    status.accepted_checkpoint_id.as_deref().unwrap_or("none"),
                                )
                            }
                        };
                        write_eval_output(
                            output.as_ref(),
                            &rendered,
                            "Distillation cadence status",
                        )?;
                    }
                    DistillationCadenceCommand::Run {
                        lane,
                        format,
                        output,
                    } => {
                        let conn = open_db_read_only(&vault)?;
                        let report = kb_core::distillation_cadence::run(
                            &vault,
                            &conn,
                            kb_core::distillation_cadence::DistillationCadenceRunArguments {
                                lane: lane.map(Into::into),
                            },
                        )?;
                        let rendered = match format {
                            ReportFormat::Json => serde_json::to_string_pretty(&report)?,
                            ReportFormat::Markdown => {
                                kb_core::distillation_cadence::render_markdown(&report)
                            }
                        };
                        write_eval_output(output.as_ref(), &rendered, "Distillation cadence run")?;
                    }
                },
                DistillCommand::Apply { input, client } => {
                    let request: kb_core::distillation_executor::DistillationExecutionRequest =
                        serde_json::from_str(&fs::read_to_string(&input).with_context(|| {
                            format!("Distillation executionを読めない: {}", input.display())
                        })?)
                        .context("Distillation execution JSONを解釈できない")?;
                    let conn = open_db(&vault)?;
                    let report =
                        kb_core::distillation_executor::execute(&vault, &conn, request, &client)?;
                    println!("{}", serde_json::to_string_pretty(&report)?);
                }
                DistillCommand::Rollback {
                    execution_id,
                    client,
                } => {
                    let conn = open_db(&vault)?;
                    let report = kb_core::distillation_executor::rollback(
                        &vault,
                        &conn,
                        &execution_id,
                        &client,
                    )?;
                    println!("{}", serde_json::to_string_pretty(&report)?);
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
        Command::Eval { command } => match command {
            EvalCommand::Retrieval {
                cases,
                format,
                output,
            } => {
                let vault = open_vault(cli.vault.as_deref())?;
                let conn = open_db(&vault)?;
                sync(&vault, &conn)?;
                let input = fs::read_to_string(&cases)
                    .with_context(|| format!("Golden Queryを読めない: {}", cases.display()))?;
                let suite: kb_core::retrieval_eval::GoldenSuite = serde_json::from_str(&input)
                    .with_context(|| format!("Golden Query JSONが不正: {}", cases.display()))?;
                let report = kb_core::retrieval_eval::evaluate(&conn, &suite)?;
                let rendered = match format {
                    ReportFormat::Json => serde_json::to_string_pretty(&report)?,
                    ReportFormat::Markdown => kb_core::retrieval_eval::render_markdown(&report),
                };
                if let Some(path) = output {
                    fs::write(&path, format!("{}\n", rendered.trim_end())).with_context(|| {
                        format!("retrieval評価レポートを書けない: {}", path.display())
                    })?;
                    println!("{}", path.display());
                } else {
                    println!("{}", rendered.trim_end());
                }
            }
            EvalCommand::RuleDeliveryPlan {
                suite,
                mode,
                format,
                output,
            } => {
                let input = fs::read_to_string(&suite).with_context(|| {
                    format!("Rule Delivery suiteを読めない: {}", suite.display())
                })?;
                let suite: kb_core::rule_delivery_eval::RuleDeliverySuite =
                    serde_json::from_str(&input)
                        .with_context(|| "Rule Delivery suite JSONが不正")?;
                kb_core::rule_delivery_eval::validate_official_suite(&suite)?;
                let plan = kb_core::rule_delivery_eval::prepare(&suite, mode.into())?;
                let rendered = match format {
                    ReportFormat::Json => serde_json::to_string_pretty(&plan)?,
                    ReportFormat::Markdown => {
                        anyhow::bail!("Rule Delivery planは--format jsonのみ対応")
                    }
                };
                write_eval_output(output.as_ref(), &rendered, "Rule Delivery plan")?;
            }
            EvalCommand::RuleDeliveryFixture { suite, output } => {
                let input = fs::read_to_string(&suite).with_context(|| {
                    format!("Rule Delivery suiteを読めない: {}", suite.display())
                })?;
                let suite: kb_core::rule_delivery_eval::RuleDeliverySuite =
                    serde_json::from_str(&input)
                        .with_context(|| "Rule Delivery suite JSONが不正")?;
                let vault = kb_core::rule_delivery_eval::create_fixture(&suite, &output)?;
                println!("{}", vault.root.display());
            }
            EvalCommand::RuleDeliveryScore {
                suite,
                traces,
                format,
                output,
            } => {
                let suite_input = fs::read_to_string(&suite).with_context(|| {
                    format!("Rule Delivery suiteを読めない: {}", suite.display())
                })?;
                let suite: kb_core::rule_delivery_eval::RuleDeliverySuite =
                    serde_json::from_str(&suite_input)
                        .with_context(|| "Rule Delivery suite JSONが不正")?;
                kb_core::rule_delivery_eval::validate_official_suite(&suite)?;
                let trace_input = fs::read_to_string(&traces).with_context(|| {
                    format!("Rule Delivery traceを読めない: {}", traces.display())
                })?;
                let traces: kb_core::rule_delivery_eval::TraceSuite =
                    serde_json::from_str(&trace_input)
                        .with_context(|| "Rule Delivery trace JSONが不正")?;
                let report = kb_core::rule_delivery_eval::score(&suite, &traces)?;
                let rendered = match format {
                    ReportFormat::Json => serde_json::to_string_pretty(&report)?,
                    ReportFormat::Markdown => {
                        kb_core::rule_delivery_eval::render_report_markdown(&report)
                    }
                };
                write_eval_output(output.as_ref(), &rendered, "Rule Delivery report")?;
            }
        },
        Command::Mcp { client } => {
            kb_core::mcp::serve(&client, || open_vault(cli.vault.as_deref()))?;
        }
        Command::Settings { command } => match command {
            SettingsCommand::AiEnabled { client } => {
                let enabled = kb_core::settings::load()?.ai_kb_enabled_for(&client)
                    && kb_core::ai_guard::client_connection_is_allowed(&client);
                println!("{}", serde_json::json!({"enabled": enabled}));
            }
            SettingsCommand::AiGuardStatus => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&kb_core::ai_guard::status()?)?
                );
            }
            SettingsCommand::InstallAiGuard => {
                let status = kb_core::ai_guard::install().map_err(anyhow::Error::new)?;
                println!("{}", serde_json::to_string_pretty(&status)?);
            }
        },
    }
    Ok(())
}

fn write_eval_output(output: Option<&PathBuf>, rendered: &str, label: &str) -> Result<()> {
    if let Some(path) = output {
        fs::write(path, format!("{}\n", rendered.trim_end()))
            .with_context(|| format!("{label}を書けない: {}", path.display()))?;
        println!("{}", path.display());
    } else {
        println!("{}", rendered.trim_end());
    }
    Ok(())
}

fn read_distillation_checkpoint(
    path: &PathBuf,
) -> Result<kb_core::distillation_audit::DistillationCheckpoint> {
    let input = fs::read_to_string(path)
        .with_context(|| format!("Distillation baselineを読めない: {}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&input)
        .with_context(|| format!("Distillation baseline JSONが不正: {}", path.display()))?;
    let checkpoint = value.get("checkpoint").cloned().unwrap_or(value);
    serde_json::from_value(checkpoint)
        .with_context(|| format!("Distillation checkpointが不正: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use clap::{CommandFactory, Parser};

    use super::{
        Cli, Command, DistillCommand, DistillationCadenceCommand, DistillationCadenceLaneArg,
        EvalCommand, ReportFormat, RuleDeliveryModeArg,
    };

    fn command_paths(command: &clap::Command, prefix: Option<&str>, paths: &mut BTreeSet<String>) {
        for subcommand in command.get_subcommands() {
            // clapが自動生成するhelpは公開APIの追加ではない。
            if subcommand.get_name() == "help" {
                continue;
            }
            let path = match prefix {
                Some(prefix) => format!("{prefix} {}", subcommand.get_name()),
                None => subcommand.get_name().to_string(),
            };
            paths.insert(path.clone());
            command_paths(subcommand, Some(&path), paths);
        }
    }

    /// CLI の公開面を完全一致で固定する。`edit` を含む人間所有の書き込み口や、
    /// 将来追加された未監査commandを「禁止名の列挙漏れ」で通さない。
    #[test]
    fn cli_command_tree_matches_the_reviewed_allowlist() {
        let mut actual = BTreeSet::new();
        command_paths(&Cli::command(), None, &mut actual);
        let expected = BTreeSet::from([
            "care".to_string(),
            "care dismiss".to_string(),
            "care list".to_string(),
            "distill".to_string(),
            "distill apply".to_string(),
            "distill audit".to_string(),
            "distill cadence".to_string(),
            "distill cadence run".to_string(),
            "distill cadence status".to_string(),
            "distill plan".to_string(),
            "distill rollback".to_string(),
            "embed".to_string(),
            "embed enable".to_string(),
            "embed status".to_string(),
            "eval".to_string(),
            "eval retrieval".to_string(),
            "eval rule-delivery-fixture".to_string(),
            "eval rule-delivery-plan".to_string(),
            "eval rule-delivery-score".to_string(),
            "get".to_string(),
            "import".to_string(),
            "mcp".to_string(),
            "migrate-files".to_string(),
            "propose".to_string(),
            "recent".to_string(),
            "search".to_string(),
            "settings".to_string(),
            "settings ai-enabled".to_string(),
            "settings ai-guard-status".to_string(),
            "settings install-ai-guard".to_string(),
            "storage".to_string(),
            "storage export".to_string(),
            "storage verify".to_string(),
            "sync".to_string(),
            "vault".to_string(),
            "vault create".to_string(),
            "vault list".to_string(),
        ]);

        assert_eq!(
            actual, expected,
            "CLI commandを追加・削除する場合は、所有境界を監査してallowlistも更新する"
        );
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

    #[test]
    fn retrieval_evaluation_defaults_to_markdown_and_stdout() {
        let cli = Cli::try_parse_from([
            "kb",
            "--vault",
            "work",
            "eval",
            "retrieval",
            "--cases",
            "/private/golden.json",
        ])
        .unwrap();
        let Command::Eval {
            command:
                EvalCommand::Retrieval {
                    cases,
                    format,
                    output,
                },
        } = cli.command
        else {
            panic!("eval retrievalとして解釈されなかった");
        };
        assert_eq!(cases, PathBuf::from("/private/golden.json"));
        assert!(matches!(format, ReportFormat::Markdown));
        assert!(output.is_none());
    }

    #[test]
    fn rule_delivery_plan_requires_an_explicit_mode() {
        let cli = Cli::try_parse_from([
            "kb",
            "eval",
            "rule-delivery-plan",
            "--suite",
            "/private/rules.json",
            "--mode",
            "always-topic-event",
        ])
        .unwrap();
        let Command::Eval {
            command:
                EvalCommand::RuleDeliveryPlan {
                    suite,
                    mode,
                    format,
                    output,
                },
        } = cli.command
        else {
            panic!("eval rule-delivery-planとして解釈されなかった");
        };
        assert_eq!(suite, PathBuf::from("/private/rules.json"));
        assert!(matches!(mode, RuleDeliveryModeArg::AlwaysTopicEvent));
        assert!(matches!(format, ReportFormat::Json));
        assert!(output.is_none());
    }

    #[test]
    fn distillation_plan_defaults_to_deterministic_json_and_stdout() {
        let cli = Cli::try_parse_from(["kb", "--vault", "work", "distill", "plan"]).unwrap();
        let Command::Distill {
            command: DistillCommand::Plan { format, output },
        } = cli.command
        else {
            panic!("distill planとして解釈されなかった");
        };
        assert!(matches!(format, ReportFormat::Json));
        assert!(output.is_none());
    }

    #[test]
    fn distillation_audit_accepts_an_optional_checkpoint_and_defaults_to_json() {
        let cli = Cli::try_parse_from([
            "kb",
            "--vault",
            "work",
            "distill",
            "audit",
            "--baseline",
            "/private/previous-audit.json",
        ])
        .unwrap();
        let Command::Distill {
            command:
                DistillCommand::Audit {
                    baseline,
                    format,
                    output,
                },
        } = cli.command
        else {
            panic!("distill auditとして解釈されなかった");
        };
        assert_eq!(
            baseline,
            Some(PathBuf::from("/private/previous-audit.json"))
        );
        assert!(matches!(format, ReportFormat::Json));
        assert!(output.is_none());
    }

    #[test]
    fn distillation_cadence_defaults_to_due_and_accepts_an_explicit_lane() {
        let status = Cli::try_parse_from(["kb", "distill", "cadence", "status"]).unwrap();
        let Command::Distill {
            command:
                DistillCommand::Cadence {
                    command: DistillationCadenceCommand::Status { format, output },
                },
        } = status.command
        else {
            panic!("distill cadence statusとして解釈されなかった");
        };
        assert!(matches!(format, ReportFormat::Json));
        assert!(output.is_none());

        let run =
            Cli::try_parse_from(["kb", "distill", "cadence", "run", "--lane", "weekly"]).unwrap();
        let Command::Distill {
            command:
                DistillCommand::Cadence {
                    command:
                        DistillationCadenceCommand::Run {
                            lane,
                            format,
                            output,
                        },
                },
        } = run.command
        else {
            panic!("distill cadence runとして解釈されなかった");
        };
        assert!(matches!(lane, Some(DistillationCadenceLaneArg::Weekly)));
        assert!(matches!(format, ReportFormat::Json));
        assert!(output.is_none());
    }

    #[test]
    fn distillation_apply_and_rollback_require_explicit_audit_identity() {
        let apply = Cli::try_parse_from([
            "kb",
            "distill",
            "apply",
            "--input",
            "/private/execution.json",
        ])
        .unwrap();
        let Command::Distill {
            command: DistillCommand::Apply { input, client },
        } = apply.command
        else {
            panic!("distill applyとして解釈されなかった");
        };
        assert_eq!(input, PathBuf::from("/private/execution.json"));
        assert_eq!(client, "cli/unknown");

        let rollback = Cli::try_parse_from([
            "kb",
            "distill",
            "rollback",
            "--execution-id",
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        ])
        .unwrap();
        let Command::Distill {
            command:
                DistillCommand::Rollback {
                    execution_id,
                    client,
                },
        } = rollback.command
        else {
            panic!("distill rollbackとして解釈されなかった");
        };
        assert!(execution_id.starts_with("sha256:"));
        assert_eq!(client, "cli/unknown");
    }
}
