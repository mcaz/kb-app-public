//! 「繋ぐ」(FR-A1 の代行+FR-A6 の最小)。
//! - Claude Desktop 接続: claude_desktop_config.json への追記をアプリが代行
//!   (バックアップ作成・既存サーバー不侵害。M1 で手作業だった手順の機構化)
//! - バックアップ: 紐付け済み remote(origin 固定)への明示 push と滞留表示。
//!   宛先は origin のみ(FR-A6: push 先固定)。private 実確認は GitHub 接続実装時に追加

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::frontmatter::today;
use crate::vault::Vault;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DesktopStatus {
    /// Claude Desktop の設定ファイルが見つからない(未インストールか未起動)
    NotFound,
    NotConnected,
    Connected,
}

pub fn claude_desktop_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("Claude").join("claude_desktop_config.json"))
}

pub fn desktop_status_at(config: &Path) -> DesktopStatus {
    let Ok(text) = fs::read_to_string(config) else {
        return DesktopStatus::NotFound;
    };
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(v) if v.get("mcpServers").and_then(|s| s.get("kb-app")).is_some() => {
            DesktopStatus::Connected
        }
        Ok(_) => DesktopStatus::NotConnected,
        Err(_) => DesktopStatus::NotConnected,
    }
}

/// kb-app サーバーを Desktop 設定へ追記。既存キーは触らない。バックアップを残す。
/// `exe` は MCP を起動する実行ファイル(アプリ自身+`--mcp`)。
pub fn connect_desktop_at(config: &Path, exe: &Path, vault_name: &str) -> Result<()> {
    let text = fs::read_to_string(config)
        .with_context(|| format!("Claude Desktop の設定が見つからない: {}", config.display()))?;
    let mut v: serde_json::Value = serde_json::from_str(&text).context("設定の parse")?;
    let servers = v
        .as_object_mut()
        .context("設定がオブジェクトでない")?
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    let servers = servers.as_object_mut().context("mcpServers がオブジェクトでない")?;
    let backup = config.with_file_name(format!(
        "claude_desktop_config.json.bak-kbapp-{}",
        today()
    ));
    fs::copy(config, &backup).context("バックアップ作成")?;
    servers.insert(
        "kb-app".to_string(),
        serde_json::json!({
            "command": exe.to_string_lossy(),
            "args": ["--mcp", "--vault", vault_name, "--client", "claude-desktop/claude"],
        }),
    );
    fs::write(config, serde_json::to_string_pretty(&v)?).context("設定の書き込み")?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct BackupStatus {
    /// origin の URL(未設定なら None = 未接続)
    pub remote: Option<String>,
    /// 未バックアップのコミット数(remote 未設定時は全コミット数)
    pub pending: usize,
}

pub fn backup_status(vault: &Vault) -> Result<BackupStatus> {
    let repo = git2::Repository::open(&vault.root)?;
    let remote = repo
        .find_remote("origin")
        .ok()
        .and_then(|r| r.url().map(String::from));
    let head = match repo.head().ok().and_then(|h| h.peel_to_commit().ok()) {
        Some(c) => c,
        None => return Ok(BackupStatus { remote, pending: 0 }),
    };
    let tracking = repo
        .branch_upstream_name(repo.head()?.name().unwrap_or("refs/heads/main"))
        .ok()
        .and_then(|b| b.as_str().map(String::from))
        .and_then(|name| repo.find_reference(&name).ok())
        .and_then(|r| r.peel_to_commit().ok());
    let pending = match tracking {
        Some(t) => repo.graph_ahead_behind(head.id(), t.id())?.0,
        None => {
            let mut walk = repo.revwalk()?;
            walk.push(head.id())?;
            walk.count()
        }
    };
    Ok(BackupStatus { remote, pending })
}

/// 同期(FR-A6 改定 2026-08-10): 随時 push・メッセージ時 pull。
/// 認証はシステム git の資格情報(keychain / ssh-agent)に委ねる — GitHub 接続
/// (OAuth デバイスフロー+keyring)で置き換え予定。
/// git は非対話モード強制(資格情報プロンプトで GUI/MCP をハングさせない)。
fn git(vault: &Vault, args: &[&str]) -> Result<std::process::Output> {
    std::process::Command::new("git")
        .args(args)
        .current_dir(&vault.root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes")
        .output()
        .context("git 実行")
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).trim().to_string()
}

/// 生成ファイルを競合させない設定(複数デバイス同期の前提)。
/// index.md は派生物 — 競合したら相手側を取り、pull 後に再生成で自己修復。
/// log.md は追記ログ — union merge で両側の行を残す。
/// merge driver の設定はリポジトリローカルなので、clone した側でも毎回冪等に張り直す。
fn ensure_merge_config(vault: &Vault) -> Result<()> {
    let attrs = vault.root.join(".gitattributes");
    let want = "index.md merge=ours\nlog.md merge=union\n";
    let current = fs::read_to_string(&attrs).unwrap_or_default();
    if current != want {
        fs::write(&attrs, want)?;
        vault.commit(&[".gitattributes"], "vault: 同期用の merge 属性")?;
    }
    let repo = git2::Repository::open(&vault.root)?;
    repo.config()?.set_str("merge.ours.driver", "true")?;
    Ok(())
}

/// バックアップ先の設定(origin 固定)。GitHub リポジトリ(またはテスト用のローカルパス)のみ。
/// 設定後に初回 push(-u で追跡を張り、以後の滞留判定を成立させる)。
pub fn set_backup_remote(vault: &Vault, url: &str) -> Result<()> {
    ensure_merge_config(vault)?;
    let url = url.trim();
    let is_github = url.starts_with("git@github.com:") || url.starts_with("https://github.com/");
    let is_local = url.starts_with("file://") || Path::new(url).is_absolute();
    if !is_github && !is_local {
        bail!("バックアップ先は GitHub リポジトリの URL を指定(git@github.com:… か https://github.com/…)");
    }
    let repo = git2::Repository::open(&vault.root)?;
    match repo.find_remote("origin") {
        Ok(_) => repo.remote_set_url("origin", url)?,
        Err(_) => {
            repo.remote("origin", url)?;
        }
    }
    push_now(vault)
}

/// いま push(随時 push の実体)。非 fast-forward なら pull --rebase して1回だけ再試行。
pub fn push_now(vault: &Vault) -> Result<()> {
    let _lock = sync_lock(vault)?;
    let _ = ensure_merge_config(vault);
    let out = git(vault, &["push", "-u", "origin", "HEAD"])?;
    if out.status.success() {
        record_sync(vault, None);
        return Ok(());
    }
    let pull = git(vault, &["pull", "--rebase", "--autostash"])?;
    if pull.status.success() {
        let retry = git(vault, &["push", "-u", "origin", "HEAD"])?;
        if retry.status.success() {
            record_sync(vault, None);
            return Ok(());
        }
        let e = format!("push 失敗: {}", stderr_of(&retry));
        record_sync(vault, Some(&e));
        bail!(e);
    }
    let e = format!("push 失敗(pull --rebase も失敗): {}", stderr_of(&pull));
    record_sync(vault, Some(&e));
    bail!(e);
}

/// 同期操作(pull/push)のプロセス間ロック。GUI・MCP・CLI が同時に git を叩くと
/// FETCH_HEAD の競合で「Cannot rebase onto multiple branches」等の一過性エラーになる
/// (実機で観測)。flock で直列化する — git 自体は取らない advisory lock なので、
/// この3者(自アプリ群)の間でだけ効けばよい。
fn sync_lock(vault: &Vault) -> Result<fs::File> {
    use fs4::fs_std::FileExt;
    let dir = vault.root.join(".kb");
    fs::create_dir_all(&dir)?;
    let f = fs::File::create(dir.join("sync.lock"))?;
    f.lock_exclusive().context("同期ロック取得")?;
    Ok(f)
}

fn has_origin(vault: &Vault) -> bool {
    match git2::Repository::open(&vault.root) {
        Ok(repo) => repo.find_remote("origin").is_ok(),
        Err(_) => false,
    }
}

/// ノート操作の後に呼ぶ随時 push。remote 未設定なら何もしない。
/// **失敗してもノート操作は成功のまま**(fail-open)— 失敗は sync 状態に記録され画面に出る。
pub fn auto_push(vault: &Vault) {
    if has_origin(vault) {
        let _ = push_now(vault);
    }
}

const PULL_THROTTLE_SECS: u64 = 60;

fn sync_state_path(vault: &Vault) -> PathBuf {
    vault.root.join(".kb").join("sync.json")
}

#[derive(Debug, Default, Clone, Serialize, serde::Deserialize)]
pub struct SyncState {
    pub last_pull_epoch: u64,
    pub last_error: Option<String>,
}

pub fn sync_state(vault: &Vault) -> SyncState {
    fs::read_to_string(sync_state_path(vault))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn record_sync(vault: &Vault, error: Option<&str>) {
    let mut st = sync_state(vault);
    st.last_error = error.map(String::from);
    if error.is_none() {
        st.last_pull_epoch = epoch_now();
    }
    let path = sync_state_path(vault);
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(path, serde_json::to_string(&st).unwrap_or_default());
}

fn epoch_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// メッセージのやり取り・画面更新の際の pull(複数デバイス同期)。
/// 時間スロットリング付き — 全呼び出しで同期待ちしない(旧 KB のレイテンシ教訓)。
/// 戻り値: 劣化情報(None = 正常またはスキップ)。
pub fn pull_if_stale(vault: &Vault) -> Option<String> {
    if !has_origin(vault) {
        return None;
    }
    if epoch_now().saturating_sub(sync_state(vault).last_pull_epoch) < PULL_THROTTLE_SECS {
        return sync_state(vault).last_error.map(|e| format!("同期エラー(前回): {e}"));
    }
    pull_now(vault).err().map(|e| e.to_string())
}

/// いま pull(スロットリング無視)。成功後は index.md を再生成して自己修復
/// (merge=ours で相手側が勝った場合や、他デバイス追加分の反映)。
pub fn pull_now(vault: &Vault) -> Result<()> {
    let _lock = sync_lock(vault)?;
    let _ = ensure_merge_config(vault);
    let out = git(vault, &["pull", "--rebase", "--autostash"])?;
    if out.status.success() {
        record_sync(vault, None);
        let _ = vault.write_index_md();
        Ok(())
    } else {
        let e = format!("pull 失敗: {}", stderr_of(&out));
        record_sync(vault, Some(&e));
        bail!(e)
    }
}

/// 明示同期(pull → push)。「今すぐバックアップ」ボタンの実体。
pub fn backup_push(vault: &Vault) -> Result<String> {
    let status = backup_status(vault)?;
    if status.remote.is_none() {
        bail!("バックアップ先が未設定(繋ぐ画面で GitHub リポジトリの URL を設定)");
    }
    pull_now(vault)?;
    push_now(vault)?;
    Ok(format!("同期完了({} 件を送信)", status.pending))
}

/// FR-A5 最小: 「いま見ているノート」をコアの状態として記録(将来 MCP 側から参照)。
pub fn set_current_note(vault: &Vault, id: &str) -> Result<()> {
    let dir = vault.root.join(".kb");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("current-note"), id)?;
    Ok(())
}

pub fn current_note(vault: &Vault) -> Option<String> {
    fs::read_to_string(vault.root.join(".kb").join("current-note"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_connect_preserves_existing_servers() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("claude_desktop_config.json");
        fs::write(&cfg, r#"{"mcpServers": {"vault": {"command": "x"}}}"#).unwrap();
        assert_eq!(desktop_status_at(&cfg), DesktopStatus::NotConnected);
        connect_desktop_at(&cfg, Path::new("/usr/bin/true"), "try").unwrap();
        assert_eq!(desktop_status_at(&cfg), DesktopStatus::Connected);
        let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&cfg).unwrap()).unwrap();
        assert!(v["mcpServers"]["vault"].is_object(), "既存サーバーが保持される");
        assert_eq!(v["mcpServers"]["kb-app"]["args"][2], "try");
        // バックアップが残る
        assert!(fs::read_dir(dir.path()).unwrap().count() >= 2);
    }

    #[test]
    fn backup_status_without_remote() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        vault.new_human_note("メモ", "本文", "human:o").unwrap();
        let st = backup_status(&vault).unwrap();
        assert!(st.remote.is_none());
        assert!(st.pending >= 2); // initialize + note
    }

    /// 複数デバイス同期の一周: A が書く → 随時 push → B がメッセージ時 pull で受け取る
    #[test]
    fn multi_device_sync_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("backup.git");
        let run = |cwd: &Path, args: &[&str]| {
            let out = std::process::Command::new("git").args(args).current_dir(cwd).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(dir.path(), &["init", "--bare", bare.to_str().unwrap()]);

        // デバイス A: vault 作成 → バックアップ先設定(初回 push)→ ノート追加(随時 push)
        let a = Vault::create(dir.path().join("a")).unwrap();
        set_backup_remote(&a, bare.to_str().unwrap()).unwrap();
        a.new_human_note("同期テスト", "デバイス A で書いた。", "human:o").unwrap();
        assert_eq!(backup_status(&a).unwrap().pending, 0, "随時 push 済みなら滞留ゼロ");

        // bare の HEAD を A のブランチ名に合わせる(git2 と system git の
        // 既定ブランチ名差で clone が空チェックアウトになるのを防ぐ)
        let branch = git2::Repository::open(&a.root).unwrap().head().unwrap().shorthand().unwrap().to_string();
        run(&bare, &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")]);

        // デバイス B: clone で復元 → 会話(pull)で A の変化を受け取る
        run(dir.path(), &["clone", bare.to_str().unwrap(), "b"]);
        let b = Vault::open(dir.path().join("b")).unwrap();
        assert_eq!(b.list_note_files().len(), 1);
        a.new_human_note("追加分", "A の2本目。", "human:o").unwrap();
        pull_now(&b).unwrap();
        assert_eq!(b.list_note_files().len(), 2, "B が pull で A の追加分を受け取る");

        // B 側で書いても push が通る(非 fast-forward 時の rebase 再試行経路)
        a.new_human_note("三本目", "A の3本目(B の pull 後)。", "human:o").unwrap();
        b.new_human_note("B のメモ", "デバイス B で書いた。", "human:o").unwrap();
        assert_eq!(backup_status(&b).unwrap().pending, 0, "rebase 再試行で push が通る");
    }

    #[test]
    fn current_note_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        assert!(current_note(&vault).is_none());
        set_current_note(&vault, "notes/foo").unwrap();
        assert_eq!(current_note(&vault).as_deref(), Some("notes/foo"));
    }
}
