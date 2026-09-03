//! AI クライアントからKBを使うかどうかの端末設定。
//!
//! WebView の localStorage では別プロセスのMCPから読めず、Vaultへ置くと
//! 別端末の比較条件まで変えてしまう。GUIとMCPが共有する設定ディレクトリを正本にする。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::client_surface::{ClientFamily, ClientSurface};
use crate::error::{CoreError, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(default)]
pub struct Settings {
    pub ai_kb_enabled: bool,
    pub claude_kb_enabled: bool,
    pub gpt_kb_enabled: bool,
    /// ログイン自動起動の既定を適用済みか。登録の正本はOS側のログイン項目で、
    /// ここは「一度でも既定を書いたか」だけを覚える(`autostart::initialize`)。
    pub launch_at_login_initialized: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // 設定ファイル導入前から接続済みの利用者の挙動を変えない。
            ai_kb_enabled: true,
            claude_kb_enabled: true,
            gpt_kb_enabled: true,
            // 既存利用者にも初回起動で1回だけ既定を適用する。
            launch_at_login_initialized: false,
        }
    }
}

impl Settings {
    /// 全体設定に加え、MCP登録時のclient hintに対応する個別設定を適用する。
    /// 未知のclientは全体設定だけを使い、新しい連携を黙って停止しない。
    pub fn ai_kb_enabled_for(&self, client: &str) -> bool {
        if !self.ai_kb_enabled {
            return false;
        }

        match ClientSurface::from_hint(client).family() {
            ClientFamily::Claude => self.claude_kb_enabled,
            ClientFamily::Gpt => self.gpt_kb_enabled,
            ClientFamily::Other => true,
        }
    }
}

pub fn load() -> Result<Settings> {
    load_at(&settings_path()?)
}

pub fn set_ai_kb_enabled(enabled: bool) -> Result<Settings> {
    update(|settings| settings.ai_kb_enabled = enabled)
}

pub fn set_claude_kb_enabled(enabled: bool) -> Result<Settings> {
    update(|settings| settings.claude_kb_enabled = enabled)
}

pub fn set_gpt_kb_enabled(enabled: bool) -> Result<Settings> {
    update(|settings| settings.gpt_kb_enabled = enabled)
}

pub fn mark_launch_at_login_initialized() -> Result<Settings> {
    update(|settings| settings.launch_at_login_initialized = true)
}

fn update(change: impl FnOnce(&mut Settings)) -> Result<Settings> {
    update_at(&settings_path()?, change)
}

fn update_at(path: &Path, change: impl FnOnce(&mut Settings)) -> Result<Settings> {
    let mut settings = load_at(path)?;
    change(&mut settings);
    save_at(path, &settings)?;
    Ok(settings)
}

fn settings_path() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("設定ディレクトリが特定できない")
        .map_err(CoreError::configuration)?
        .join("kb-app");
    Ok(dir.join("settings.json"))
}

fn load_at(path: &Path) -> Result<Settings> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .context("settings.json parse")
            .map_err(CoreError::configuration),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(error) => Err(CoreError::configuration(error)),
    }
}

fn save_at(path: &Path, settings: &Settings) -> Result<()> {
    let parent = path
        .parent()
        .context("settings.json に親ディレクトリがない")
        .map_err(CoreError::configuration)?;
    fs::create_dir_all(parent).map_err(CoreError::configuration)?;

    // MCPが同時に読んでも途中のJSONを見ないよう、同じディレクトリで書いてから置換する。
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(CoreError::configuration)?;
    serde_json::to_writer_pretty(&mut temp, settings).map_err(CoreError::configuration)?;
    temp.write_all(b"\n").map_err(CoreError::configuration)?;
    temp.as_file()
        .sync_all()
        .map_err(CoreError::configuration)?;
    temp.persist(path)
        .map_err(|error| CoreError::configuration(error.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_settings_preserve_the_existing_enabled_behavior() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load_at(&dir.path().join("missing.json")).unwrap(),
            Settings::default()
        );
    }

    #[test]
    fn setting_round_trips_without_touching_the_vault() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config/settings.json");
        save_at(
            &path,
            &Settings {
                ai_kb_enabled: false,
                claude_kb_enabled: false,
                gpt_kb_enabled: true,
                ..Settings::default()
            },
        )
        .unwrap();

        assert_eq!(
            load_at(&path).unwrap(),
            Settings {
                ai_kb_enabled: false,
                claude_kb_enabled: false,
                gpt_kb_enabled: true,
                ..Settings::default()
            }
        );

        update_at(&path, |settings| settings.ai_kb_enabled = true).unwrap();
        update_at(&path, |settings| settings.claude_kb_enabled = true).unwrap();
        update_at(&path, |settings| settings.gpt_kb_enabled = false).unwrap();

        assert_eq!(
            load_at(&path).unwrap(),
            Settings {
                ai_kb_enabled: true,
                claude_kb_enabled: true,
                gpt_kb_enabled: false,
                ..Settings::default()
            }
        );
    }

    #[test]
    fn older_settings_default_new_client_switches_to_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"ai_kb_enabled":false}"#).unwrap();

        assert_eq!(
            load_at(&path).unwrap(),
            Settings {
                ai_kb_enabled: false,
                claude_kb_enabled: true,
                gpt_kb_enabled: true,
                ..Settings::default()
            }
        );
    }

    /// 自動起動の既定は「1回だけ適用する」ので、この印が落ちると毎回登録し直して
    /// ユーザーがOS側で外した設定を打ち消す。
    #[test]
    fn the_launch_at_login_mark_persists_and_defaults_to_unapplied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"ai_kb_enabled":true}"#).unwrap();
        assert!(!load_at(&path).unwrap().launch_at_login_initialized);

        update_at(&path, |settings| {
            settings.launch_at_login_initialized = true;
        })
        .unwrap();
        assert!(load_at(&path).unwrap().launch_at_login_initialized);
    }

    #[test]
    fn master_and_client_switches_are_applied_independently() {
        let mut settings = Settings {
            ai_kb_enabled: true,
            claude_kb_enabled: false,
            gpt_kb_enabled: true,
            ..Settings::default()
        };

        assert!(!settings.ai_kb_enabled_for("claude-code/claude"));
        assert!(!settings.ai_kb_enabled_for("claude-desktop/claude"));
        assert!(settings.ai_kb_enabled_for("codex-cli/gpt-5-codex"));
        assert!(settings.ai_kb_enabled_for("chatgpt/openai"));
        assert!(settings.ai_kb_enabled_for("future-client/model"));
        // model名の部分一致で未知surfaceを既存familyへ誤分類しない。
        assert!(settings.ai_kb_enabled_for("future-client/claude-gpt-codex"));

        settings.ai_kb_enabled = false;
        assert!(!settings.ai_kb_enabled_for("codex-cli/gpt-5-codex"));
        assert!(!settings.ai_kb_enabled_for("future-client/model"));
    }

    #[test]
    fn broken_settings_do_not_silently_enable_kb_access() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, "not json").unwrap();

        let error = load_at(&path).unwrap_err();
        assert_eq!(
            error.kind(),
            Some(crate::error::CoreErrorKind::Configuration)
        );
    }
}
