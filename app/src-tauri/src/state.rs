//! アプリ全体で共有する vault と索引接続。
//!
//! 以前は各コマンドが `Vault::open` と `open_db` を呼び直していた
//! (vault 16箇所・DB 11箇所)。ノートを1本開くだけでも vault と DB を開き直し、
//! 索引の全体 sync まで走っていたため、ここに集約する。
//!
//! 生成は遅延。オンボーディング前は vault が存在しないため、起動時には作れない。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use kb_core::index::open_db;
use kb_core::ledger::Ledger;
use kb_core::registry::Registry;
use kb_core::rusqlite::Connection;
use kb_core::store::Stores;
use kb_core::vault::Vault;
use tauri::Manager;

use crate::error::{AppError, AppResult};

/// asset protocolの静的scopeは空にし、現在のVaultだけを実行時に許可する。
/// Tauri側がcanonical pathでscopeを判定するため、symlinkや`..`で外へ出られない。
pub fn allow_vault_assets(app: &tauri::AppHandle, root: &Path) -> AppResult<()> {
    let root = root.canonicalize().map_err(AppError::vault)?;
    let scope = app.asset_protocol_scope();
    scope
        .allow_directory(&root, true)
        .map_err(AppError::unexpected)?;
    // Git内部と派生DBはMarkdown画像として読む必要がない。
    scope
        .forbid_directory(root.join(".git"), true)
        .map_err(AppError::unexpected)?;
    scope
        .forbid_directory(root.join(".kb"), true)
        .map_err(AppError::unexpected)?;
    Ok(())
}

struct VaultCtx {
    vault: Vault,
    conn: Connection,
    /// 最後のバックグラウンド保守で得た劣化情報。
    degraded: Vec<kb_core::degradation::Degradation>,
}

#[derive(Default)]
pub struct AppState {
    ctx: Mutex<Option<VaultCtx>>,
}

impl AppState {
    /// 既定 vault の名前(お気に入り等、vault ごとの UI 設定のキー)。
    pub fn vault_name(&self) -> AppResult<String> {
        let reg = Registry::load().map_err(AppError::configuration)?;
        let path = reg.resolve(None).map_err(AppError::vault)?;
        reg.vaults
            .iter()
            .find(|v| v.path == path)
            .map(|v| v.name.clone())
            .ok_or(AppError::VaultUnavailable)
    }

    /// vault だけを使う(索引に触らない操作)。
    pub fn with_vault<T>(&self, f: impl FnOnce(&Vault) -> AppResult<T>) -> AppResult<T> {
        let mut guard = self.ctx.lock().map_err(|_| poisoned())?;
        let ctx = ensure(&mut guard)?;
        f(&ctx.vault)
    }

    /// ファイル(Artifact)を扱う。台帳・実体の置き場・保管庫 ID を揃えて渡す。
    ///
    /// 3つとも保管庫 ID に紐づくので、ここで一度に解決する。生成は毎回で、
    /// 実費は ID ファイル1回の読み取り(台帳の読み込みは呼ばれた操作の側)。
    pub fn with_artifacts<T>(
        &self,
        f: impl FnOnce(&Vault, &Stores, &Ledger, &str) -> AppResult<T>,
    ) -> AppResult<T> {
        let mut guard = self.ctx.lock().map_err(|_| poisoned())?;
        let ctx = ensure(&mut guard)?;
        let workspace_id =
            kb_core::workspace::workspace_id(&ctx.vault).map_err(AppError::storage)?;
        let stores = Stores::open(&workspace_id).map_err(AppError::storage)?;
        let ledger = Ledger::open(&ctx.vault, &workspace_id).map_err(AppError::storage)?;
        f(&ctx.vault, &stores, &ledger, &workspace_id)
    }

    /// UIの前景処理から共有DBを使う。ここでは同期・export・埋め込みを起動しない。
    /// `degraded` は最後のバックグラウンド保守の結果を画面へ引き継ぐ。
    pub fn with_db<T>(
        &self,
        f: impl FnOnce(&Vault, &Connection, Vec<kb_core::degradation::Degradation>) -> AppResult<T>,
    ) -> AppResult<T> {
        let mut guard = self.ctx.lock().map_err(|_| poisoned())?;
        let ctx = ensure(&mut guard)?;
        f(&ctx.vault, &ctx.conn, ctx.degraded.clone())
    }

    /// バックグラウンド保守用に、共有ロックを握らず開けるvaultルートを返す。
    pub fn vault_root(&self) -> AppResult<PathBuf> {
        let mut guard = self.ctx.lock().map_err(|_| poisoned())?;
        Ok(ensure(&mut guard)?.vault.root.clone())
    }

    /// 同じvaultに対する保守結果だけを共有状態へ反映する。
    /// 保守中にオンボーディングでvaultが切り替わった場合は古い結果を捨てる。
    pub fn set_degraded_for(
        &self,
        root: &Path,
        degraded: Vec<kb_core::degradation::Degradation>,
    ) -> AppResult<()> {
        let mut guard = self.ctx.lock().map_err(|_| poisoned())?;
        let ctx = ensure(&mut guard)?;
        if ctx.vault.root == root {
            ctx.degraded = degraded;
        }
        Ok(())
    }

    /// 開いている vault を手放す(オンボーディング直後など、開き直しが要るとき)。
    pub fn reset(&self) {
        if let Ok(mut guard) = self.ctx.lock() {
            *guard = None;
        }
    }
}

fn ensure(guard: &mut Option<VaultCtx>) -> AppResult<&mut VaultCtx> {
    if guard.is_none() {
        let reg = Registry::load().map_err(AppError::configuration)?;
        let path = reg.resolve(None).map_err(AppError::vault)?;
        let vault = Vault::open(path).map_err(AppError::vault)?;
        let conn = open_db(&vault).map_err(AppError::index)?;
        *guard = Some(VaultCtx {
            vault,
            conn,
            degraded: Vec::new(),
        });
    }
    Ok(guard.as_mut().expect("直前に生成している"))
}

fn poisoned() -> AppError {
    AppError::Unexpected {
        message: "内部状態のロックが壊れている(再起動してください)".into(),
    }
}
