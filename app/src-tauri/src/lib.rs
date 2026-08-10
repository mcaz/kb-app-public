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
    drafts: Vec<Hit>,
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
    let drafts = notes.iter().filter(|h| h.status == "draft").cloned().collect();
    Ok(HomeState {
        stats: stats(&conn).map_err(err)?,
        notes,
        drafts,
        degraded: degraded.into_iter().collect(),
    })
}

#[derive(Serialize)]
struct NoteView {
    id: String,
    title: String,
    body: String,
    status: String,
    origin: Option<String>,
    tags: Vec<String>,
    generated_at: Option<String>,
    related: Vec<(String, Option<String>)>,
}

#[tauri::command]
fn note_get(id: String) -> CmdResult<NoteView> {
    let vault = default_vault()?;
    let note = vault.read_note(&id).map_err(err)?;
    let (conn, _) = synced_conn(&vault)?;
    Ok(NoteView {
        title: note.front.title.clone().unwrap_or_else(|| id.clone()),
        body: note.body.clone(),
        status: note.front.effective_status().to_string(),
        origin: note.front.origin.clone(),
        tags: note.front.tags.clone(),
        generated_at: note.front.generated.as_ref().map(|g| g.at.clone()),
        related: related_of(&conn, Some(&id)).unwrap_or_default(),
        id,
    })
}

#[tauri::command]
fn note_save(id: String, title: String, body: String) -> CmdResult<()> {
    let vault = default_vault()?;
    vault.edit_note(&id, &title, &body, OWNER_ACTOR).map_err(err)
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

/// 受信箱: 下書きの確定(「追加する」)。
#[tauri::command]
fn draft_confirm(id: String) -> CmdResult<()> {
    let vault = default_vault()?;
    vault.confirm(&id, OWNER_ACTOR).map_err(err)
}

/// 受信箱: 下書きの差し戻し(「やめておく」= deprecated)。
#[tauri::command]
fn draft_reject(id: String) -> CmdResult<()> {
    let vault = default_vault()?;
    vault.archive(&id).map_err(err)
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
    let ok = std::process::Command::new("open")
        .args(["-a", "Claude"])
        .status()
        .map_err(err)?
        .success();
    if !ok {
        return Err("Claude Desktop を起動できなかった(インストール確認を)".into());
    }
    Ok(())
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
            note_search,
            draft_confirm,
            draft_reject,
            connect_state,
            connect_desktop,
            backup_now,
            backup_set_remote,
            embed_enable,
            launch_ai,
        ])
        .run(tauri::generate_context!())
        .expect("tauri run");
}
