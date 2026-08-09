//! マルチ vault レジストリ(FR-C2)。設定ディレクトリの registry.json に
//! {name, path} を持つだけの薄い台帳。vault 実体には何も足さない。

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultEntry {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub vaults: Vec<VaultEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

pub fn registry_path() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("設定ディレクトリが特定できない")?
        .join("kb-app");
    Ok(dir.join("registry.json"))
}

impl Registry {
    pub fn load() -> Result<Registry> {
        let path = registry_path()?;
        match fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).context("registry.json parse"),
            Err(_) => Ok(Registry::default()),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = registry_path()?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn add(&mut self, name: &str, path: PathBuf) -> Result<()> {
        if self.vaults.iter().any(|v| v.name == name) {
            bail!("vault 名 {name} は登録済み");
        }
        self.vaults.push(VaultEntry { name: name.into(), path });
        if self.default.is_none() {
            self.default = Some(name.into());
        }
        Ok(())
    }

    pub fn resolve(&self, name: Option<&str>) -> Result<PathBuf> {
        if let Some(n) = name {
            return self
                .vaults
                .iter()
                .find(|v| v.name == n)
                .map(|v| v.path.clone())
                .with_context(|| format!("vault {n} は未登録"));
        }
        if let Ok(env) = std::env::var("KB_VAULT") {
            return Ok(PathBuf::from(env));
        }
        let def = self.default.as_deref();
        self.vaults
            .iter()
            .find(|v| Some(v.name.as_str()) == def)
            .or_else(|| self.vaults.first())
            .map(|v| v.path.clone())
            .context("vault が1つも登録されていない(kb vault create <name> で作成)")
    }
}
