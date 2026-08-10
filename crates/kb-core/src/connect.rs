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

/// 明示バックアップ(origin へ push)。宛先は origin 固定(FR-A6)。
/// 認証はシステム git の資格情報(keychain / ssh-agent)に委ねる — GitHub 接続
/// (OAuth デバイスフロー+keyring)は v0.3 後半で置き換える。
pub fn backup_push(vault: &Vault) -> Result<String> {
    let status = backup_status(vault)?;
    if status.remote.is_none() {
        bail!("バックアップ先が未設定(エンジニア向け: git remote add origin <url> で紐付け可能)");
    }
    let out = std::process::Command::new("git")
        .args(["push", "origin", "HEAD"])
        .current_dir(&vault.root)
        .output()
        .context("git 実行")?;
    if !out.status.success() {
        bail!("push 失敗: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(format!("バックアップ完了({} 件)", status.pending))
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

    #[test]
    fn current_note_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        assert!(current_note(&vault).is_none());
        set_current_note(&vault, "notes/foo").unwrap();
        assert_eq!(current_note(&vault).as_deref(), Some("notes/foo"));
    }
}
