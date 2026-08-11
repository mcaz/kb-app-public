//! タグのお気に入り(名前付きタグセット)。UI の道具立てであって知識ではないため、
//! vault(ノートのみ)ではなくアプリ設定ディレクトリに置く。vault ごとに分けて持つ。

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// 名前付きの絞り込みセット。タグだけでなく検索語・期間・並び順も保存する
/// (2026-08-11: 「お気に入り = 画面の絞り込み状態そのもの」に拡張)。
/// 旧形式(name/tags のみ)も読めるよう追加項目はすべて default。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Favorite {
    pub name: String,
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
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

/// 追加(同名は上書き)。絞り込みが空(タグも検索語も無し)なら不可。
pub fn add(vault: &str, fav: Favorite) -> Result<()> {
    let name = fav.name.trim().to_string();
    if name.is_empty() {
        bail!("お気に入りの名前が空");
    }
    if fav.tags.is_empty() && fav.query.as_deref().unwrap_or("").trim().is_empty() {
        bail!("タグか検索語を指定してから保存する");
    }
    let mut store = load();
    let favs = store.entry(vault.to_string()).or_default();
    favs.retain(|f| f.name != name);
    favs.push(Favorite { name, ..fav });
    save(&store)
}

pub fn remove(vault: &str, name: &str) -> Result<()> {
    let mut store = load();
    if let Some(favs) = store.get_mut(vault) {
        favs.retain(|f| f.name != name);
    }
    save(&store)
}
