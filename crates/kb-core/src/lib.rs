//! kb-core — ノートストア+索引+検索+ライフサイクル API+MCP サーバー(ヘッドレス)。
//! GUI(Tauri)・CLI・AI ツールはすべてこの crate を呼ぶ(docs/requirements.md システム構成)。
//! ノート形式は OKF v0.2 互換(docs/okf-conformance.md)。

pub use rusqlite;

pub mod artifact;
pub mod artifact_search;
pub mod backup;
pub mod care;
pub mod connect;
pub mod embed;
pub mod favorites;
pub mod frontmatter;
pub mod github;
pub mod github_auth;
pub mod import;
pub mod index;
pub mod intake;
pub mod ledger;
pub mod lfs;
pub mod mcp;
pub mod migrate;
pub mod registry;
pub mod resolve;
pub mod search;
pub mod storage_contract;
pub mod store;
pub mod tags;
pub mod tokenize;
pub mod vault;
pub mod workspace;

/// コアのバージョン。GUI / CLI / MCP が同一コアを共有していることの確認用。
pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 保管庫の**外**に置くもの(実体・同期しない台帳・埋め込みモデル)の親。
///
/// **1箇所に集約している理由**(2026-08-13 の事故): 置き場の組み立てが4箇所に
/// 散っていて、そのすべてが実ユーザーのデータ領域を直に指していた。テストは
/// 保管庫を tempdir に作るが**保管庫 ID は毎回新しく発行される**ので、
/// `cargo test` のたびに `<データ領域>/kb-app/artifacts/<新しい ID>/` が増え、
/// 気づいたときには 168 個・19MB 溜まっていた。
///
/// テストビルドでは**実ユーザーの領域へ到達できない**(下の `#[cfg(test)]`)。
/// 「テストでは触らない」を文章ではなく経路で守る。
pub fn app_data_dir() -> anyhow::Result<std::path::PathBuf> {
    #[cfg(test)]
    {
        Ok(test_data_root())
    }
    #[cfg(not(test))]
    {
        use anyhow::Context;
        Ok(dirs::data_dir()
            .context("データ領域が特定できない")?
            .join("kb-app"))
    }
}

/// テスト専用の置き場。プロセスで1つを共有する(保管庫 ID で分かれるので衝突しない)。
#[cfg(test)]
fn test_data_root() -> std::path::PathBuf {
    static ROOT: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| tempfile::tempdir().expect("一時ディレクトリ"))
        .path()
        .join("kb-app")
}

/// ローカルユーザーの actor ID(OKF §7)。命名は未決のため暫定値を一箇所に集約。
pub const OWNER_ACTOR: &str = "human:owner";

#[cfg(test)]
mod tests {
    /// 2026-08-13: テストが実ユーザーのデータ領域に置き場を作り続けていた
    /// (168 個・19MB)。**テストビルドからは実領域へ到達できない**ことを固定する。
    #[test]
    fn tests_never_reach_the_real_data_dir() {
        let used = super::app_data_dir().unwrap();
        assert_ne!(
            Some(used.clone()),
            dirs::data_dir().map(|d| d.join("kb-app"))
        );
        assert!(!used.starts_with(dirs::home_dir().unwrap_or_default()));
    }
}
