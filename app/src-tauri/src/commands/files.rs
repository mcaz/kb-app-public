//! ノートに付いたファイル(Artifact)— ADR-0003 実装順5・7。
//!
//! ここは**判断を持たない**。取り込みの可否・区分の固定・拒否はすべて
//! `kb_core::intake` が行い(決定6)、この層は画面が1行を描くのに要る形へ
//! 詰め替えるだけにする。画面を通らない経路(CLI・MCP)が同じコアへ合流するので、
//! ここに条件を書けばその分だけ穴が空く。
//!
//! 取り込みで渡すのは**パスだけ**で、中身は運ばない(決定8。base64 IPC の廃止)。
//! 唯一の例外はクリップボード画像で、これは path が無いため一時ファイルへ
//! 書き出してから同じ入口へ渡す。

use std::path::{Path, PathBuf};
use std::str::FromStr;

use kb_core::artifact::{ArtifactId, Manifest, Role, Sensitivity, SyncPolicy};
use kb_core::intake;
use kb_core::resolve;
use kb_core::store::{Availability, Stores, availability};
use kb_core::vault::Vault;
use serde::Serialize;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// クリップボード画像の上限。**ファイルの上限ではない**(決定8)。
/// この経路だけ streaming できず、RGBA と PNG が同時にメモリへ載るため
/// その保護。4032×3024(iPhone の写真)の RGBA が約 47MB で、そこを通す幅。
const CLIPBOARD_MAX_BYTES: usize = 64 * 1024 * 1024;

/// 画面が1行を描くのに要る分だけ。台帳をそのまま渡すと、画面が内部の語を
/// 知ることになる(正本「内部語を見せない語彙」)。
#[derive(Serialize, specta::Type)]
pub struct FileRow {
    id: String,
    /// 更新に必要(競合を検出するため画面が持ち回る)
    version: u64,
    name: String,
    size: u64,
    media_type: String,
    /// この端末で開けるか。**同期される値ではなく、都度算出する**(決定9)
    availability: Availability,
    sensitivity: Sensitivity,
    sync: SyncPolicy,
    /// 元の場所を指しているだけ(この保管庫は複製を持っていない)
    linked: bool,
    client_repo: bool,
    /// 取り寄せを提案してよいか。**方針で閉じているものには提案しない**
    can_fetch: bool,
    added_at: String,
}

/// 移行前の添付(`<id>.files/`)。読み取り専用の旧経路(決定4)。
#[derive(Serialize, specta::Type)]
pub struct LegacyFile {
    name: String,
    size: u64,
}

#[derive(Serialize, specta::Type)]
pub struct NoteFiles {
    files: Vec<FileRow>,
    /// まだ台帳に載っていない旧添付。移行までは並べて見せるだけにする
    legacy: Vec<LegacyFile>,
}

/// 取り込んだ結果。警告と「区分を固定した」は拒否ではないので、行と一緒に返す。
#[derive(Serialize, specta::Type)]
pub struct Added {
    file: FileRow,
    /// 大きいという警告(拒否ではない)。閾値をバイトで返す
    warn_over_bytes: Option<u64>,
    /// 仕事のリポジトリ内なので同期しない設定に固定した。画面はこの理由を出す
    forced_local_only: bool,
}

fn row(vault: &Vault, stores: &Stores, m: &Manifest) -> FileRow {
    let availability = availability(vault, stores, m);
    FileRow {
        id: m.id.to_string(),
        version: m.version,
        name: m.display_name.clone(),
        size: m.created.size,
        media_type: m.created.media_type.clone(),
        // 取り寄せは「手元に無い」かつ「本体も同期する」ものにだけ提案する
        can_fetch: availability == Availability::Missing && m.policy.sync == SyncPolicy::Full,
        availability,
        sensitivity: m.policy.sensitivity,
        sync: m.policy.sync,
        linked: !matches!(m.locator, kb_core::artifact::Locator::Managed { .. }),
        client_repo: m.policy.client_repo,
        added_at: m.created.at.clone(),
    }
}

fn artifact_id(raw: &str) -> AppResult<ArtifactId> {
    ArtifactId::from_str(raw).map_err(AppError::from)
}

/// そのノートのファイル(最新版だけ)と、まだ移行していない旧添付。
#[tauri::command]
#[specta::specta]
pub fn note_files(state: State<'_, AppState>, id: String) -> AppResult<NoteFiles> {
    state.with_artifacts(|vault, stores, ledger, _| {
        let mut files: Vec<FileRow> = ledger
            .list_for_note(&id)
            .iter()
            .map(|m| row(vault, stores, m))
            .collect();
        files.sort_by(|a, b| a.added_at.cmp(&b.added_at));

        let legacy = vault
            .list_attachments(&id)
            .into_iter()
            .map(|(name, size)| LegacyFile { name, size })
            .collect();
        Ok(NoteFiles { files, legacy })
    })
}

/// パスから取り込む(選択・ドラッグ&ドロップ)。
///
/// `supersedes` を渡すと**新しい版**になる(前の版は残る)。
#[tauri::command]
#[specta::specta]
pub fn file_add(
    state: State<'_, AppState>,
    note_id: String,
    path: String,
    supersedes: Option<String>,
) -> AppResult<Added> {
    let src = PathBuf::from(&path);
    let supersedes = supersedes.as_deref().map(artifact_id).transpose()?;
    take(
        &state,
        &src,
        note_id,
        display_name(&src),
        "picker",
        supersedes,
    )
}

/// クリップボードの画像を取り込む。画像が無ければ `None`(テキストの貼り付けを邪魔しない)。
///
/// WKWebView は DOM の paste にクリップボード画像を渡さないため、この経路が要る。
#[tauri::command]
#[specta::specta]
pub fn file_add_from_clipboard(
    state: State<'_, AppState>,
    note_id: String,
) -> AppResult<Option<Added>> {
    let mut clipboard = arboard::Clipboard::new().map_err(AppError::unexpected)?;
    let Ok(img) = clipboard.get_image() else {
        return Ok(None);
    };
    if img.bytes.len() > CLIPBOARD_MAX_BYTES {
        return Err(AppError::ClipboardImageTooLarge);
    }
    let rgba =
        image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned())
            .ok_or_else(|| AppError::unexpected("クリップボード画像の変換に失敗"))?;

    // 取り込みは path 経由の streaming に一本化してあるので、ここでも
    // 一度ファイルにしてから同じ入口へ渡す(この経路だけ例外を作らない)
    let name = format!(
        "pasted-{}.png",
        kb_core::frontmatter::now_iso().replace(':', "-")
    );
    let tmp = std::env::temp_dir().join(&name);
    image::DynamicImage::ImageRgba8(rgba)
        .save_with_format(&tmp, image::ImageFormat::Png)
        .map_err(AppError::unexpected)?;

    let taken = take(&state, &tmp, note_id, name, "clipboard", None);
    let _ = std::fs::remove_file(&tmp);
    taken.map(Some)
}

fn display_name(src: &Path) -> String {
    src.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string()
}

fn take(
    state: &State<'_, AppState>,
    src: &Path,
    note_id: String,
    name: String,
    origin: &str,
    supersedes: Option<ArtifactId>,
) -> AppResult<Added> {
    state.with_artifacts(|vault, stores, ledger, workspace_id| {
        let req = intake::Request {
            note_id: Some(note_id.clone()),
            display_name: name.clone(),
            media_type: intake::guess_media_type(src),
            role: Role::File,
            // 保存方法と区分はコアが場所から決める。画面が既定を持つと、
            // 画面を通らない経路と食い違う(決定6)
            keep: None,
            policy: None,
            ref_name: None,
            supersedes: supersedes.clone(),
            origin: origin.to_string(),
            by: kb_core::OWNER_ACTOR.to_string(),
            at: kb_core::frontmatter::now_iso(),
        };
        let taken = intake::take(vault, stores, ledger, workspace_id, src, req)?;
        Ok(Added {
            file: row(vault, stores, &taken.manifest),
            warn_over_bytes: taken.warn_over,
            forced_local_only: taken.forced_local_only,
        })
    })
}

/// 開く。**中身は必ず resolver 経由で取り出す。**
///
/// OS のアプリへ渡すにはパスが要るが、置き場のファイル名は照合値(hash)なので、
/// 表示名を付けた複製を一時領域に作ってから渡す。内容は不変なので使い回せる。
///
/// 経路をここに一本化しているのは、**画面へパスを渡さない**ため。パスを渡すと
/// 「手元に無いものの中身を開かない」が画面側の作法に落ちる(決定9・resolver の doc)。
#[tauri::command]
#[specta::specta]
pub fn file_open(app: tauri::AppHandle, state: State<'_, AppState>, id: String) -> AppResult<()> {
    let id = artifact_id(&id)?;
    let path = state.with_artifacts(|vault, stores, ledger, _| {
        let resolved = resolve::resolve(vault, stores, ledger, &resolve::Link::Fixed(id.clone()))
            .map_err(AppError::from)?
            .ok_or_else(|| AppError::Unexpected {
                message: format!("ファイルの台帳が無い: {id}"),
            })?;
        // 手元に無い / 方針で閉じている場合、ここが None を返す
        let mut file = resolved
            .open(vault, stores)
            .map_err(AppError::from)?
            .ok_or(AppError::FileNotHere)?;
        export(&resolved.manifest, &mut file)
    })?;
    open_with_os(&app, &path)
}

/// 移行前の添付を開く。台帳が無いので保管庫の中の実ファイルを直接指す(決定4)。
#[tauri::command]
#[specta::specta]
pub fn legacy_open(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    note_id: String,
    name: String,
) -> AppResult<()> {
    let path = state.with_vault(|vault| {
        // 名前は成分だけ使う(パス潜り対策 — 旧実装と同じ扱い)
        let base = Path::new(&name)
            .file_name()
            .ok_or_else(|| AppError::unexpected("ファイル名が不正"))?;
        let path = vault.attach_dir(&note_id).join(base);
        if !path.is_file() {
            return Err(AppError::FileNotHere);
        }
        Ok(path)
    })?;
    open_with_os(&app, &path)
}

/// 表示名を付けた複製を一時領域へ。内容は不変なので、同じものがあれば使い回す。
fn export(m: &Manifest, src: &mut std::fs::File) -> AppResult<PathBuf> {
    // 表示名は直せる field なので、パスとして解釈させない
    let name = Path::new(&m.display_name)
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("file"));
    let dir = std::env::temp_dir()
        .join("kb-app-open")
        .join(&m.hash.as_str()[..12]);
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(name);
    if std::fs::metadata(&dest).map(|meta| meta.len()).ok() != Some(m.created.size) {
        let mut out = std::fs::File::create(&dest)?;
        // 固定サイズの塊で写す(全量をメモリに載せない — 決定8 と同じ理由)
        std::io::copy(src, &mut out)?;
    }
    Ok(dest)
}

/// OS の既定のアプリへ渡す。**画面から直接は呼べない** — 権限を webview に与えて
/// いないので、開く経路はこの上の2コマンドだけになる。
fn open_with_os(app: &tauri::AppHandle, path: &Path) -> AppResult<()> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(AppError::unexpected)
}

/// このノートから外す。**実体は消えない**(GC を持たない MVP で「削除」と言わない)。
#[tauri::command]
#[specta::specta]
pub fn file_detach(
    state: State<'_, AppState>,
    note_id: String,
    id: String,
    expected_version: u64,
) -> AppResult<()> {
    let id = artifact_id(&id)?;
    state.with_artifacts(|vault, _, ledger, _| {
        let mut manifest =
            ledger
                .get(&id)
                .map_err(AppError::from)?
                .ok_or_else(|| AppError::Unexpected {
                    message: format!("ファイルの台帳が無い: {id}"),
                })?;
        manifest.detach(expected_version, &note_id)?;
        manifest.record(
            &kb_core::frontmatter::now_iso(),
            "detached",
            &format!("{note_id} から外した"),
        );
        ledger.put(vault, &manifest).map_err(AppError::from)
    })
}

/// 手元に無い実体を取り寄せる。戻り値は取り寄せた後の状態(都度算出)。
#[tauri::command]
#[specta::specta]
pub fn file_fetch(state: State<'_, AppState>, id: String) -> AppResult<Availability> {
    let id = artifact_id(&id)?;
    state.with_artifacts(|vault, stores, ledger, _| {
        let manifest =
            ledger
                .get(&id)
                .map_err(AppError::from)?
                .ok_or_else(|| AppError::Unexpected {
                    message: format!("ファイルの台帳が無い: {id}"),
                })?;
        kb_core::lfs::fetch(vault, &manifest.hash).map_err(AppError::from)?;
        Ok(availability(vault, stores, &manifest))
    })
}
