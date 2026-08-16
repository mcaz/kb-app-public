//! AI クライアントからKBを使うかどうかの端末設定。
//!
//! WebView の localStorage では別プロセスのMCPから読めず、Vaultへ置くと
//! 別端末の比較条件まで変えてしまう。GUIとMCPが共有する設定ディレクトリを正本にする。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(default)]
pub struct Settings {
    pub ai_kb_enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // 設定ファイル導入前から接続済みの利用者の挙動を変えない。
            ai_kb_enabled: true,
        }
    }
}

pub fn load() -> Result<Settings> {
    load_at(&settings_path()?)
}

pub fn set_ai_kb_enabled(enabled: bool) -> Result<Settings> {
    let settings = Settings {
        ai_kb_enabled: enabled,
    };
    save_at(&settings_path()?, &settings)?;
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
            },
        )
        .unwrap();

        assert_eq!(
            load_at(&path).unwrap(),
            Settings {
                ai_kb_enabled: false,
            }
        );

        save_at(
            &path,
            &Settings {
                ai_kb_enabled: true,
            },
        )
        .unwrap();

        assert_eq!(
            load_at(&path).unwrap(),
            Settings {
                ai_kb_enabled: true,
            }
        );
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
