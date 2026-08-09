//! kb — エンジニア向け CLI(将来 `kb mcp` で MCP サーバー起動)。

fn main() {
    println!("kb {} (core {})", env!("CARGO_PKG_VERSION"), kb_core::CORE_VERSION);
}
