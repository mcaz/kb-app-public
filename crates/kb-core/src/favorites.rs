//! タグのお気に入り(名前付きタグセット)。UI の道具立てであって知識ではないため、
//! vault(ノートのみ)ではなくアプリ設定ディレクトリに置く。vault ごとに分けて持つ。

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Favorite {
    pub name: String,
    pub tags: Vec<String>,
}

type Store = BTreeMap<String, Vec<Favorite>>;

fn store_path() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("設定ディレクトリが特定できない")?
        .join("kb-app")
        .join("favorites.json"))
}

fn load() -> Store {
    store_path()
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(store: &Store) -> Result<()> {
    let path = store_path()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&path, serde_json::to_string_pretty(store)?)?;
    Ok(())
}

pub fn list(vault: &str) -> Vec<Favorite> {
    load().get(vault).cloned().unwrap_or_default()
}

/// 追加(同名は上書き)。タグ空は不可。
pub fn add(vault: &str, name: &str, tags: &[String]) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        bail!("お気に入りの名前が空");
    }
    if tags.is_empty() {
        bail!("タグを1つ以上選んでから保存する");
    }
    let mut store = load();
    let favs = store.entry(vault.to_string()).or_default();
    favs.retain(|f| f.name != name);
    favs.push(Favorite { name: name.to_string(), tags: tags.to_vec() });
    save(&store)
}

pub fn remove(vault: &str, name: &str) -> Result<()> {
    let mut store = load();
    if let Some(favs) = store.get_mut(vault) {
        favs.retain(|f| f.name != name);
    }
    save(&store)
}
