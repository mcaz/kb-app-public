#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // `kb-app --mcp --vault <name> [--client <actor>]` は GUI を開かず MCP サーバーとして動く。
    // Claude Desktop の設定がこの実行ファイル1つを指せる(配布形の前提)。
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--mcp") {
        let get = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .and_then(|i| args.get(i + 1))
                .cloned()
        };
        let client = get("--client").unwrap_or_else(|| "mcp-client/unknown".into());
        let result = (|| -> anyhow::Result<()> {
            let reg = kb_core::registry::Registry::load()?;
            let path = reg.resolve(get("--vault").as_deref())?;
            let vault = kb_core::vault::Vault::open(path)?;
            kb_core::mcp::serve(&vault, &client)
        })();
        if let Err(e) = result {
            eprintln!("kb-app --mcp: {e}");
            std::process::exit(1);
        }
        return;
    }
    kb_app_lib::run()
}
