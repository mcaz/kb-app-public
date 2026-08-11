//! `kb-app --mcp --vault <name> [--client <actor>]` で GUI を開かず MCP サーバーとして動く。
//!
//! Claude Desktop の設定がこの実行ファイル1つを指せるようにするための経路
//! (配布形の前提 — 非エンジニアに別バイナリの導入を求めない)。

/// MCP モードなら実行して true を返す。通常起動なら false。
pub fn run_if_requested() -> bool {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|a| a == "--mcp") {
        return false;
    }

    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let client = flag("--client").unwrap_or_else(|| "mcp-client/unknown".into());

    let result = (|| -> anyhow::Result<()> {
        let reg = kb_core::registry::Registry::load()?;
        let path = reg.resolve(flag("--vault").as_deref())?;
        let vault = kb_core::vault::Vault::open(path)?;
        kb_core::mcp::serve(&vault, &client)
    })();

    if let Err(e) = result {
        eprintln!("kb-app --mcp: {e}");
        std::process::exit(1);
    }
    true
}
