//! kb-core — ノートストア+索引+検索+ライフサイクル API+MCP サーバー(ヘッドレス)。
//! GUI(Tauri)・CLI・AI ツールはすべてこの crate を呼ぶ(docs/requirements.md システム構成)。
//! ノート形式は OKF v0.2 互換(docs/okf-conformance.md)。

pub use rusqlite;

pub mod frontmatter;
pub mod index;
pub mod mcp;
pub mod registry;
pub mod search;
pub mod tokenize;
pub mod vault;

/// コアのバージョン。GUI / CLI / MCP が同一コアを共有していることの確認用。
pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// ローカルユーザーの actor ID(OKF §7)。命名は未決のため暫定値を一箇所に集約。
pub const OWNER_ACTOR: &str = "human:owner";
