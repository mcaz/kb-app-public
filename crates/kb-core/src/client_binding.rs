//! クライアント接続時に選んだ保管庫のIDを、端末内で固定する。
//!
//! registry の現在の既定と比較しても、hookとMCPの取り違えは検出できない。
//! trusted GUI の接続操作だけがこの期待値を更新し、MCPは読み取りに限定する。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::client_surface::ClientSurface;
use crate::error::{CoreError, Result};

const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientBinding {
    pub vault_name: String,
    pub workspace_id: String,
}

impl ClientBinding {
    pub fn new(vault_name: String, workspace_id: String) -> Result<Self> {
        let binding = Self {
            vault_name,
            workspace_id,
        };
        binding.validate()?;
        Ok(binding)
    }

    fn validate(&self) -> Result<()> {
        if self.vault_name.trim().is_empty()
            || self.vault_name.chars().any(char::is_control)
            || !crate::artifact::is_ulid(&self.workspace_id)
        {
            return Err(invalid_binding());
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredBinding {
    schema_version: u32,
    surface: ClientSurface,
    binding: ClientBinding,
}

/// MCPはKBのONを確かめてから読む。trusted GUIの接続・診断はOFF時も利用できる。
/// 欠落と破損は区別し、どちらも一致の証拠にしない。
pub fn load(surface: ClientSurface) -> Result<Option<ClientBinding>> {
    load_at(&binding_path(surface)?, surface)
}

/// GUIでの接続操作用。保管庫の内容を根拠に、自動で期待値を書き換えてはならない。
pub fn bind(surface: ClientSurface, binding: &ClientBinding) -> Result<()> {
    bind_at(&binding_path(surface)?, surface, binding)
}

fn binding_path(surface: ClientSurface) -> Result<PathBuf> {
    Ok(crate::app_data_dir()?
        .join("client-bindings")
        .join(format!("{}.json", surface_key(surface)?)))
}

fn surface_key(surface: ClientSurface) -> Result<&'static str> {
    match surface {
        ClientSurface::CodexCli => Ok("codex_cli"),
        ClientSurface::ClaudeCode => Ok("claude_code"),
        ClientSurface::ClaudeDesktop => Ok("claude_desktop"),
        ClientSurface::ChatGpt => Ok("chat_gpt"),
        ClientSurface::RuleDeliveryEvaluation | ClientSurface::Unknown => Err(invalid_binding()),
    }
}

fn load_at(path: &Path, surface: ClientSurface) -> Result<Option<ClientBinding>> {
    surface_key(surface)?;
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(CoreError::configuration(error)),
    };
    let stored: StoredBinding = serde_json::from_slice(&bytes).map_err(CoreError::configuration)?;
    if stored.schema_version != SCHEMA_VERSION || stored.surface != surface {
        return Err(invalid_binding());
    }
    stored.binding.validate()?;
    Ok(Some(stored.binding))
}

fn bind_at(path: &Path, surface: ClientSurface, binding: &ClientBinding) -> Result<()> {
    surface_key(surface)?;
    binding.validate()?;
    let parent = path.parent().ok_or_else(invalid_binding)?;
    fs::create_dir_all(parent).map_err(CoreError::configuration)?;

    // クライアントごとに置換する。他の接続を失わず、破損した設定もGUIから再導入できる。
    // 同一ディレクトリのrenameにより、MCPへ途中のJSONを見せない。
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(CoreError::configuration)?;
    serde_json::to_writer_pretty(
        &mut temp,
        &StoredBinding {
            schema_version: SCHEMA_VERSION,
            surface,
            binding: binding.clone(),
        },
    )
    .map_err(CoreError::configuration)?;
    temp.write_all(b"\n").map_err(CoreError::configuration)?;
    temp.as_file()
        .sync_all()
        .map_err(CoreError::configuration)?;
    temp.persist(path)
        .map_err(|error| CoreError::configuration(error.error))?;
    Ok(())
}

fn invalid_binding() -> CoreError {
    CoreError::configuration(anyhow::anyhow!("クライアントの接続先ID設定が不正"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(name: &str) -> ClientBinding {
        ClientBinding::new(name.into(), "01AAAAAAAAAAAAAAAAAAAAAAAA".into()).unwrap()
    }

    #[test]
    fn missing_binding_is_unverified_without_creating_storage() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("absent/codex_cli.json");
        assert_eq!(load_at(&path, ClientSurface::CodexCli).unwrap(), None);
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn binding_is_surface_specific_and_can_be_replaced_after_corruption() {
        let temp = tempfile::tempdir().unwrap();
        let codex_path = temp.path().join("codex_cli.json");
        let claude_path = temp.path().join("claude_code.json");
        bind_at(&codex_path, ClientSurface::CodexCli, &binding("work")).unwrap();
        bind_at(
            &claude_path,
            ClientSurface::ClaudeCode,
            &binding("personal"),
        )
        .unwrap();
        assert_eq!(
            load_at(&codex_path, ClientSurface::CodexCli).unwrap(),
            Some(binding("work"))
        );
        assert!(load_at(&codex_path, ClientSurface::ClaudeCode).is_err());
        fs::write(&codex_path, b"{broken").unwrap();
        assert!(load_at(&codex_path, ClientSurface::CodexCli).is_err());
        bind_at(&codex_path, ClientSurface::CodexCli, &binding("new")).unwrap();
        assert_eq!(
            load_at(&codex_path, ClientSurface::CodexCli).unwrap(),
            Some(binding("new"))
        );
        assert_eq!(
            load_at(&claude_path, ClientSurface::ClaudeCode).unwrap(),
            Some(binding("personal"))
        );
    }

    #[test]
    fn invalid_or_future_binding_never_falls_back_to_missing() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("codex_cli.json");
        let valid = serde_json::to_value(StoredBinding {
            schema_version: SCHEMA_VERSION,
            surface: ClientSurface::CodexCli,
            binding: binding("work"),
        })
        .unwrap();
        for (pointer, replacement) in [
            ("/schema_version", serde_json::json!(2)),
            ("/binding/workspace_id", serde_json::json!("invalid")),
            ("/binding/vault_name", serde_json::json!("\n")),
            ("/surface", serde_json::json!("unknown")),
        ] {
            let mut fixture = valid.clone();
            *fixture.pointer_mut(pointer).unwrap() = replacement;
            fs::write(&path, serde_json::to_vec(&fixture).unwrap()).unwrap();
            assert!(
                load_at(&path, ClientSurface::CodexCli).is_err(),
                "{pointer}"
            );
        }
        assert!(ClientBinding::new("".into(), binding("work").workspace_id).is_err());
        assert!(bind_at(&path, ClientSurface::Unknown, &binding("work")).is_err());
    }
}
