//! `kb-app --mcp --vault <name> [--client <actor>] [--mcp-surface <surface>]
//! [--retrieval-profile <profile>]` で GUI を開かず MCP サーバーとして動く。
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
    let remote_sync = !args.iter().any(|arg| arg == "--no-remote-sync");
    let tool_surface = flag("--mcp-surface")
        .as_deref()
        .map(kb_core::mcp::ToolSurface::parse)
        .transpose()
        .unwrap_or_else(|error| {
            eprintln!("kb-app --mcp: {error}");
            std::process::exit(2);
        })
        .unwrap_or_default();
    // 配信 profile も surface と同じく起動時に固定し、未知値は既定へ落とさず終了する。
    let retrieval_profile = flag("--retrieval-profile")
        .as_deref()
        .map(kb_core::retrieval_profile::RetrievalProfile::parse)
        .transpose()
        .unwrap_or_else(|error| {
            eprintln!("kb-app --mcp: {error}");
            std::process::exit(2);
        });

    let result = kb_core::mcp::serve_with_options(
        &client,
        kb_core::mcp::ServeOptions {
            remote_sync,
            tool_surface,
            retrieval_profile,
        },
        || {
            let reg = kb_core::registry::Registry::load()?;
            let path = reg.resolve(flag("--vault").as_deref())?;
            kb_core::vault::Vault::open(path)
        },
    );

    if let Err(e) = result {
        eprintln!("kb-app --mcp: {e}");
        std::process::exit(1);
    }
    true
}
