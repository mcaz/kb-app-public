//! アプリ全体で共有する vault と索引接続。
//!
//! 以前は各コマンドが `Vault::open` と `open_db` を呼び直していた
//! (vault 16箇所・DB 11箇所)。ノートを1本開くだけでも vault と DB を開き直し、
//! 索引の全体 sync まで走っていたため、ここに集約する。
//!
//! 生成は遅延。オンボーディング前は vault が存在しないため、起動時には作れない。

use std::sync::Mutex;
use std::time::{Duration, Instant};

use kb_core::index::{open_db, sync};
use kb_core::registry::Registry;
use kb_core::rusqlite::Connection;
use kb_core::vault::Vault;

use crate::error::{AppError, AppResult};

/// 索引 sync の最小間隔。変更が無ければ sync 自体は数十msだが、ノートを
/// 連続で開くたびに走らせる意味はないので間引く。画面の更新は TanStack Query が
/// 再取得するので、この窓の間に外部変更を取りこぼしても次の取得で追いつく。
const SYNC_INTERVAL: Duration = Duration::from_secs(3);

/// 索引を更新するかどうか。
#[derive(Clone, Copy, PartialEq)]
pub enum Sync {
    /// 必ず更新する(一覧の鮮度が要る画面の入口)。
    Force,
    /// 直近に更新していれば省く。
    Throttled,
}

struct VaultCtx {
    vault: Vault,
    conn: Connection,
    last_sync: Option<Instant>,
    /// 最後に実際へ sync したときの劣化情報(間引いた回でも画面に出し続ける)。
    degraded: Option<String>,
}

#[derive(Default)]
pub struct AppState {
    ctx: Mutex<Option<VaultCtx>>,
}

impl AppState {
    /// 既定 vault の名前(お気に入り等、vault ごとの UI 設定のキー)。
    pub fn vault_name(&self) -> AppResult<String> {
        let reg = Registry::load().map_err(AppError::from)?;
        let path = reg.resolve(None).map_err(AppError::from)?;
        reg.vaults
            .iter()
            .find(|v| v.path == path)
            .map(|v| v.name.clone())
            .ok_or_else(|| AppError::VaultUnavailable {
                message: "vault 名が特定できない".into(),
            })
    }

    /// vault だけを使う(索引に触らない操作)。
    pub fn with_vault<T>(&self, f: impl FnOnce(&Vault) -> AppResult<T>) -> AppResult<T> {
        let mut guard = self.ctx.lock().map_err(|_| poisoned())?;
        let ctx = ensure(&mut guard)?;
        f(&ctx.vault)
    }

    /// vault と索引を使う。`degraded` は索引更新の失敗(fail-open — 原則4)。
    pub fn with_index<T>(
        &self,
        policy: Sync,
        f: impl FnOnce(&Vault, &Connection, Option<String>) -> AppResult<T>,
    ) -> AppResult<T> {
        let mut guard = self.ctx.lock().map_err(|_| poisoned())?;
        let ctx = ensure(&mut guard)?;

        let stale = ctx.last_sync.is_none_or(|at| at.elapsed() >= SYNC_INTERVAL);
        if policy == Sync::Force || stale {
            ctx.degraded = match sync(&ctx.vault, &ctx.conn) {
                Ok(_) => kb_core::index::embed_step(&ctx.conn),
                Err(e) => Some(format!("索引の更新に失敗: {e}")),
            };
            ctx.last_sync = Some(Instant::now());
        }

        f(&ctx.vault, &ctx.conn, ctx.degraded.clone())
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
        let reg = Registry::load().map_err(AppError::from)?;
        let path = reg.resolve(None).map_err(|e| AppError::VaultUnavailable {
            message: e.to_string(),
        })?;
        let vault = Vault::open(path).map_err(AppError::from)?;
        let conn = open_db(&vault).map_err(AppError::from)?;
        *guard = Some(VaultCtx {
            vault,
            conn,
            last_sync: None,
            degraded: None,
        });
    }
    Ok(guard.as_mut().expect("直前に生成している"))
}

fn poisoned() -> AppError {
    AppError::Unexpected {
        message: "内部状態のロックが壊れている(再起動してください)".into(),
    }
}
