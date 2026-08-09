//! kb-core — ノートストア+索引+検索+ライフサイクル API+MCP サーバー(ヘッドレス)。
//! GUI(Tauri)・CLI・AI ツールはすべてこの crate を呼ぶ(docs/requirements.md システム構成)。

/// コアのバージョン。GUI / CLI / MCP が同一コアを共有していることの確認用。
pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");
