//! 管理アプリの Tauri 層 — コア(kb-core)の薄い GUI(NFR-M2 相当: 統治ロジックを
//! アプリ側に実装しない)。全コマンドは kb-core の API を呼ぶだけ。

use kb_core::OWNER_ACTOR;
use kb_core::index::{open_db, sync};
use kb_core::registry::Registry;
use kb_core::search::{Hit, SearchOutcome, Stats, recent, related_of, search, stats};
use kb_core::vault::Vault;
use serde::Serialize;

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// 現在の vault のレジストリ名(お気に入り等、vault ごとの UI 設定のキー)。
fn current_vault_name() -> CmdResult<String> {
    let reg = Registry::load().map_err(err)?;
    let path = reg.resolve(None).map_err(err)?;
    reg.vaults
        .iter()
        .find(|v| v.path == path)
        .map(|v| v.name.clone())
        .ok_or_else(|| "vault 名が特定できない".to_string())
}

#[tauri::command]
fn favorites_list() -> CmdResult<Vec<kb_core::favorites::Favorite>> {
    Ok(kb_core::favorites::list(&current_vault_name()?))
}

#[tauri::command]
fn favorite_add(fav: kb_core::favorites::Favorite) -> CmdResult<()> {
    kb_core::favorites::add(&current_vault_name()?, fav).map_err(err)
}

#[tauri::command]
fn favorite_remove(name: String) -> CmdResult<()> {
    kb_core::favorites::remove(&current_vault_name()?, &name).map_err(err)
}

fn default_vault() -> CmdResult<Vault> {
    let reg = Registry::load().map_err(err)?;
    let path = reg.resolve(None).map_err(err)?;
    Vault::open(path).map_err(err)
}

fn synced_conn(vault: &Vault) -> CmdResult<(kb_core::rusqlite::Connection, Option<String>)> {
    let conn = open_db(vault).map_err(err)?;
    // 索引更新の失敗は fail-open(劣化情報として画面に出す — 原則4)
    let degraded = match sync(vault, &conn) {
        Ok(_) => kb_core::index::embed_step(&conn),
        Err(e) => Some(format!("索引の更新に失敗: {e}")),
    };
    Ok((conn, degraded))
}

#[derive(Serialize)]
struct SetupState {
    needs_onboarding: bool,
    vault_name: Option<String>,
    vault_path: Option<String>,
}

#[tauri::command]
fn setup_state() -> CmdResult<SetupState> {
    let reg = Registry::load().map_err(err)?;
    match reg.resolve(None) {
        Ok(path) => Ok(SetupState {
            needs_onboarding: false,
            vault_name: reg.vaults.iter().find(|v| v.path == path).map(|v| v.name.clone()),
            vault_path: Some(path.display().to_string()),
        }),
        Err(_) => Ok(SetupState { needs_onboarding: true, vault_name: None, vault_path: None }),
    }
}

/// オンボーディング: 最初の vault を自動作成(FR-A1。既定名「わたしのノート」実体 my-notes)。
#[tauri::command]
fn onboard() -> CmdResult<SetupState> {
    let path = dirs::home_dir().ok_or("home が特定できない")?.join("kb").join("my-notes");
    let vault = Vault::create(&path).map_err(err)?;
    let mut reg = Registry::load().map_err(err)?;
    reg.add("my-notes", vault.root.clone()).map_err(err)?;
    reg.save().map_err(err)?;
    setup_state()
}

#[derive(Serialize)]
struct HomeState {
    stats: Stats,
    notes: Vec<Hit>,
    care: Vec<kb_core::care::CareProposal>,
    tags: Vec<(String, usize)>,
    degraded: Vec<String>,
}

#[tauri::command]
fn home_state() -> CmdResult<HomeState> {
    let vault = default_vault()?;
    // 画面更新の際にも他デバイスの変化を取り込む(スロットリング付き・fail-open)
    let pull_degraded = kb_core::connect::pull_if_stale(&vault);
    let (conn, degraded) = synced_conn(&vault)?;
    let degraded = degraded.or(pull_degraded);
    let notes = recent(&conn, 500).map_err(err)?;
    // お手入れの検知(FR-C7 最小形)。失敗しても画面は出す(fail-open)
    let _ = kb_core::care::detect(&conn, &vault);
    let care = kb_core::care::list_open(&conn).unwrap_or_default();
    let tags = kb_core::search::tag_counts(&conn, 30).unwrap_or_default();
    Ok(HomeState {
        stats: stats(&conn).map_err(err)?,
        notes,
        care,
        tags,
        degraded: degraded.into_iter().collect(),
    })
}


#[derive(Serialize)]
struct TagOverview {
    tags: Vec<kb_core::search::TagInfo>,
    glossary_note: Option<String>,
}

/// ホームのタグ一覧(説明は KB の「タグ運用」ノート由来)。
#[tauri::command]
fn tag_overview() -> CmdResult<TagOverview> {
    let vault = default_vault()?;
    let (conn, _) = synced_conn(&vault)?;
    let (tags, glossary_note) = kb_core::search::tag_overview(&conn).map_err(err)?;
    Ok(TagOverview { tags, glossary_note })
}

#[tauri::command]
fn care_dismiss(key: String) -> CmdResult<()> {
    let vault = default_vault()?;
    let conn = open_db(&vault).map_err(err)?;
    kb_core::care::dismiss(&conn, &key).map_err(err)
}

#[derive(Serialize)]
struct NoteView {
    id: String,
    title: String,
    body: String,
    status: String,
    origin: Option<String>,
    tags: Vec<String>,
    created_at: Option<String>,
    generated_at: Option<String>,
    related: Vec<(String, Option<String>)>,
    similar: Vec<(String, Option<String>, f32)>,
    attachments: Vec<(String, u64)>,
    vault_root: String,
}

#[tauri::command]
fn note_get(id: String) -> CmdResult<NoteView> {
    let vault = default_vault()?;
    let note = vault.read_note(&id).map_err(err)?;
    // 「このノート」文脈(FR-A5): 開いたノートを現在ノートとして記録(MCP の get 引数なしが返す)
    let _ = kb_core::connect::set_current_note(&vault, &id);
    let (conn, _) = synced_conn(&vault)?;
    Ok(NoteView {
        title: note.front.title.clone().unwrap_or_else(|| id.clone()),
        body: note.body.clone(),
        status: note.front.effective_status().to_string(),
        origin: note.front.origin.clone(),
        tags: note.front.tags.clone(),
        created_at: note.front.created_at(),
        generated_at: note.front.updated_at(),
        related: related_of(&conn, Some(&id)).unwrap_or_default(),
        similar: kb_core::search::similar_notes(&conn, &id, 6).unwrap_or_default(),
        attachments: vault.list_attachments(&id),
        vault_root: vault.root.display().to_string(),
        id,
    })
}

/// 添付の追加(FR-C8)。データは base64。戻り値 = (保存名, 警告)。
#[tauri::command]
fn attachment_add(id: String, name: String, data_base64: String) -> CmdResult<(String, Option<String>)> {
    use base64::Engine;
    let vault = default_vault()?;
    let data = base64::engine::general_purpose::STANDARD
        .decode(data_base64.as_bytes())
        .map_err(err)?;
    vault.add_attachment(&id, &name, &data).map_err(err)
}

#[tauri::command]
fn attachment_remove(id: String, name: String) -> CmdResult<()> {
    let vault = default_vault()?;
    vault.remove_attachment(&id, &name).map_err(err)
}

/// パス指定で添付(ドラッグ&ドロップ用)。Tauri はファイルドロップを DOM に渡さず
/// 自前イベントでパスをくれるので、Rust 側で直接読む(base64 経由より大きいファイルに強い)。
#[tauri::command]
fn attachment_add_from_path(id: String, path: String) -> CmdResult<(String, Option<String>)> {
    let p = std::path::Path::new(&path);
    let size = std::fs::metadata(p).map_err(err)?.len();
    if size > kb_core::vault::ATTACH_MAX_BYTES {
        return Err(format!("50MB を超えるファイルは添付できない({} MB)", size / 1024 / 1024));
    }
    let data = std::fs::read(p).map_err(err)?;
    let name = p.file_name().and_then(|f| f.to_str()).unwrap_or("file");
    let vault = default_vault()?;
    vault.add_attachment(&id, name, &data).map_err(err)
}

/// クリップボードの画像を添付(ペーストのフォールバック)。
/// WKWebView は DOM の paste イベントにクリップボード画像を渡さないため、
/// Rust 側で NSPasteboard から直接読む(prompt 無効と同じ「実機でだけ落ちる」型の対策)。
/// 画像が無ければ Ok(None)(テキストペーストの邪魔をしない)。
#[tauri::command]
fn attachment_paste(id: String) -> CmdResult<Option<(String, Option<String>)>> {
    let mut cb = arboard::Clipboard::new().map_err(err)?;
    let img = match cb.get_image() {
        Ok(i) => i,
        Err(_) => return Ok(None),
    };
    let rgba = image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned())
        .ok_or("クリップボード画像の変換に失敗")?;
    let mut png: Vec<u8> = Vec::new();
    image::DynamicImage::ImageRgba8(rgba)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(err)?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let vault = default_vault()?;
    vault
        .add_attachment(&id, &format!("pasted-{ts}.png"), &png)
        .map(Some)
        .map_err(err)
}

#[tauri::command]
fn note_save(id: String, title: String, body: String) -> CmdResult<()> {
    let vault = default_vault()?;
    vault.edit_note(&id, &title, &body, OWNER_ACTOR).map_err(err)
}

/// ノート削除(本体+添付。git 履歴には残る)。MCP には公開しない — 人の操作のみ。
#[tauri::command]
fn note_delete(id: String) -> CmdResult<()> {
    let vault = default_vault()?;
    vault.delete_note(&id).map_err(err)?;
    let (conn, _) = synced_conn(&vault)?;
    let _ = conn; // 索引から即時に消す(sync が削除を検知)
    Ok(())
}

/// 越境(原則9): AI のノートを「自分のメモにする」(origin → human)。
#[tauri::command]
fn note_make_mine(id: String) -> CmdResult<()> {
    let vault = default_vault()?;
    vault.make_mine(&id).map_err(err)
}

#[tauri::command]
fn note_new(title: String) -> CmdResult<String> {
    let vault = default_vault()?;
    vault.new_human_note(&title, "", OWNER_ACTOR).map_err(err)
}

#[tauri::command]
fn note_search(query: String) -> CmdResult<SearchOutcome> {
    let vault = default_vault()?;
    let (conn, degraded) = synced_conn(&vault)?;
    let mut out = search(&conn, &query, 30);
    out.degraded.extend(degraded);
    Ok(out)
}



#[derive(Serialize)]
struct SmartSearchState {
    state: &'static str, // "not_installed" | "downloading" | "enabled"
    embedded: usize,
    total: usize,
}

#[derive(Serialize)]
struct ConnectState {
    desktop: kb_core::connect::DesktopStatus,
    backup: kb_core::connect::BackupStatus,
    sync_error: Option<String>,
    smart_search: SmartSearchState,
}

#[tauri::command]
fn connect_state() -> CmdResult<ConnectState> {
    let vault = default_vault()?;
    let desktop = kb_core::connect::claude_desktop_config_path()
        .map(|p| kb_core::connect::desktop_status_at(&p))
        .unwrap_or(kb_core::connect::DesktopStatus::NotFound);
    let (conn, _) = synced_conn(&vault)?;
    let s = stats(&conn).map_err(err)?;
    let smart_search = SmartSearchState {
        state: if s.embed_enabled {
            "enabled"
        } else if kb_core::embed::downloading() {
            "downloading"
        } else {
            "not_installed"
        },
        embedded: s.embedded,
        total: s.total,
    };
    Ok(ConnectState {
        desktop,
        backup: kb_core::connect::backup_status(&vault).map_err(err)?,
        sync_error: kb_core::connect::sync_state(&vault).last_error,
        smart_search,
    })
}

#[derive(Serialize)]
struct GraphNode {
    id: String,
    title: String,
    origin: Option<String>,
    status: String,
    degree: usize,
}

#[derive(Serialize)]
struct GraphData {
    nodes: Vec<GraphNode>,
    edges: Vec<(String, String)>,
}

/// グラフビュー(FR-A7)用のノード・エッジ。退役ノートと未執筆リンク先は除く。
#[tauri::command]
fn graph_data() -> CmdResult<GraphData> {
    let vault = default_vault()?;
    let (conn, _) = synced_conn(&vault)?;
    let mut nodes: Vec<GraphNode> = {
        let mut stmt = conn
            .prepare("SELECT id, coalesce(title, id), origin, status FROM notes WHERE status != 'deprecated'")
            .map_err(err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(GraphNode {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    origin: r.get(2)?,
                    status: r.get(3)?,
                    degree: 0,
                })
            })
            .map_err(err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(err)?
    };
    let ids: std::collections::HashSet<String> = nodes.iter().map(|n| n.id.clone()).collect();
    let edges: Vec<(String, String)> = {
        let mut stmt = conn.prepare("SELECT src, dst FROM links").map_err(err)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(err)?;
        rows.filter_map(|r| r.ok())
            .filter(|(s, d)| ids.contains(s) && ids.contains(d))
            .collect()
    };
    for n in &mut nodes {
        n.degree = edges.iter().filter(|(s, d)| *s == n.id || *d == n.id).count();
    }
    Ok(GraphData { nodes, edges })
}

/// かしこい検索をオンにする(モデル導入+全ノート埋め込み)。数分かかる。
#[tauri::command]
async fn embed_enable() -> CmdResult<()> {
    let vault = default_vault()?;
    kb_core::embed::install_model().map_err(err)?;
    let (conn, _) = synced_conn(&vault)?;
    loop {
        if kb_core::embed::embed_pending(&conn, 10).map_err(err)? == 0 {
            break;
        }
    }
    Ok(())
}

#[tauri::command]
fn backup_set_remote(url: String) -> CmdResult<()> {
    let vault = default_vault()?;
    kb_core::connect::set_backup_remote(&vault, &url).map_err(err)
}

#[tauri::command]
fn connect_desktop() -> CmdResult<()> {
    let reg = Registry::load().map_err(err)?;
    let path = reg.resolve(None).map_err(err)?;
    let name = reg
        .vaults
        .iter()
        .find(|v| v.path == path)
        .map(|v| v.name.clone())
        .ok_or("vault 名が特定できない")?;
    let cfg = kb_core::connect::claude_desktop_config_path()
        .ok_or("設定ディレクトリが特定できない")?;
    let exe = std::env::current_exe().map_err(err)?;
    kb_core::connect::connect_desktop_at(&cfg, &exe, &name).map_err(err)
}

#[tauri::command]
fn backup_now() -> CmdResult<String> {
    let vault = default_vault()?;
    kb_core::connect::backup_push(&vault).map_err(err)
}

/// FR-A5 最小: 現在ノートを記録して Claude Desktop を前面に。
#[tauri::command]
fn launch_ai(note: Option<String>) -> CmdResult<()> {
    let vault = default_vault()?;
    if let Some(id) = note {
        kb_core::connect::set_current_note(&vault, &id).map_err(err)?;
    }
    launch_claude_desktop()?;
    Ok(())
}

/// Claude Desktop を前面に出す(OS ごとの起動方法)。
fn launch_claude_desktop() -> CmdResult<()> {
    #[cfg(target_os = "macos")]
    {
        let ok = std::process::Command::new("open")
            .args(["-a", "Claude"])
            .status()
            .map_err(err)?
            .success();
        if !ok {
            return Err("Claude Desktop を起動できなかった(インストール確認を)".into());
        }
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        // 既定のインストール先 → だめならプロトコルハンドラ経由
        if let Some(local) = dirs::data_local_dir() {
            let exe = local.join("AnthropicClaude").join("claude.exe");
            if exe.exists() && std::process::Command::new(&exe).spawn().is_ok() {
                return Ok(());
            }
        }
        let ok = std::process::Command::new("cmd")
            .args(["/C", "start", "", "claude://"])
            .status()
            .map_err(err)?
            .success();
        if !ok {
            return Err("Claude Desktop を起動できなかった(インストール確認を)".into());
        }
        return Ok(());
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let ok = std::process::Command::new("xdg-open")
            .arg("claude://")
            .status()
            .map_err(err)?
            .success();
        if !ok {
            return Err("Claude Desktop を起動できなかった(インストール確認を)".into());
        }
        Ok(())
    }
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            setup_state,
            onboard,
            home_state,
            note_get,
            note_save,
            note_new,
            note_delete,
            note_make_mine,
            note_search,
            care_dismiss,
            tag_overview,
            graph_data,
            favorites_list,
            favorite_add,
            favorite_remove,
            connect_state,
            connect_desktop,
            backup_now,
            backup_set_remote,
            embed_enable,
            attachment_add,
            attachment_add_from_path,
            attachment_remove,
            attachment_paste,
            launch_ai,
        ])
        .run(tauri::generate_context!())
        .expect("tauri run");
}
