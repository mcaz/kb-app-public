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
use std::{io::Read, io::Seek, io::Write};

use kb_core::artifact::{ArtifactId, Manifest, Role, Sensitivity, SyncPolicy};
use kb_core::intake;
use kb_core::resolve;
use kb_core::store::{Availability, Stores, availability};
use kb_core::vault::Vault;
use serde::Serialize;
use tauri::{Manager, State};

use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// クリップボード画像の上限。**ファイルの上限ではない**(決定8)。
/// この経路だけ streaming できず、RGBA と PNG が同時にメモリへ載るため
/// その保護。4032×3024(iPhone の写真)の RGBA が約 47MB で、そこを通す幅。
const CLIPBOARD_MAX_BYTES: usize = 64 * 1024 * 1024;
const UTF8_BOM: &[u8] = b"\xef\xbb\xbf";
const HTML_ENCODING_SCAN_BYTES: usize = 4096;
const TEXT_PREVIEW_MAX_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExportPurpose {
    External,
    Preview,
}

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

/// 横断一覧のカードと、プレビューの参照ノート一覧が使う。
///
/// 題名だけでは「どのノートだったか」を思い出せないので、関連 Modal の行と
/// 同じ材料(抜粋・タグ・更新)まで返す。索引の1行から取れるので追加の I/O は無い。
#[derive(Serialize, specta::Type)]
pub struct FileNote {
    id: String,
    title: String,
    /// description が無ければ本文の先頭。改行は畳んで1行に見せる
    snippet: String,
    tags: Vec<String>,
    /// RFC3339。索引に無ければ None
    updated: Option<String>,
}

/// 横断一覧の1枚。内部の台帳用語を画面へ渡さず、カードと操作に要る値だけを返す。
#[derive(Serialize, specta::Type)]
pub struct FileCard {
    id: String,
    version: u64,
    name: String,
    size: u64,
    media_type: String,
    availability: Availability,
    sensitivity: Sensitivity,
    sync: SyncPolicy,
    linked: bool,
    client_repo: bool,
    can_fetch: bool,
    added_at: String,
    notes: Vec<FileNote>,
}

#[derive(Serialize, specta::Type)]
pub struct FilesPage {
    files: Vec<FileCard>,
    degraded: Vec<kb_core::degradation::Degradation>,
}

/// resolver が許可した一時コピーだけを WebView に見せる。
#[derive(Serialize, specta::Type)]
pub struct PreviewFile {
    path: String,
    text: Option<String>,
}

/// 取り込んだ結果。警告と「区分を固定した」は拒否ではないので、行と一緒に返す。
#[derive(Serialize, specta::Type)]
pub struct Added {
    file: FileRow,
    /// 大きいという警告(拒否ではない)。閾値をバイトで返す
    warn_over_bytes: Option<u64>,
    /// 仕事のリポジトリ内なので同期しない設定に固定した。画面はこの理由を出す
    forced_local_only: bool,
    /// Full Artifact の remote 到達状態。失敗の詳細は同期状態に集約する。
    delivery: intake::DeliveryStatus,
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
    let id = kb_core::note_id::NoteId::parse(&id)
        .map_err(AppError::invalid_input)?
        .to_string();
    state.with_artifacts(|vault, stores, ledger, _| {
        let mut files: Vec<FileRow> = ledger
            .list_for_note(&id)
            .iter()
            .map(|m| row(vault, stores, m))
            .collect();
        files.sort_by(|a, b| a.added_at.cmp(&b.added_at));

        let legacy = vault
            .list_attachments(&id)
            .map_err(AppError::storage)?
            .into_iter()
            .map(|(name, size)| LegacyFile { name, size })
            .collect();
        Ok(NoteFiles { files, legacy })
    })
}

/// 全ノートを横断した現行ファイル。差し替え前の版は履歴なので一覧へ出さない。
#[tauri::command]
#[specta::specta]
pub fn files_list(state: State<'_, AppState>) -> AppResult<FilesPage> {
    state.with_db(|vault, conn, degraded| {
        let workspace_id = kb_core::workspace::workspace_id(vault).map_err(AppError::storage)?;
        let stores = Stores::open(&workspace_id).map_err(AppError::storage)?;
        let ledger =
            kb_core::ledger::Ledger::open(vault, &workspace_id).map_err(AppError::storage)?;
        let all = ledger.list();
        let superseded: std::collections::HashSet<ArtifactId> =
            all.iter().filter_map(|m| m.supersedes.clone()).collect();

        let mut files: Vec<FileCard> = all
            .into_iter()
            .filter(|manifest| !superseded.contains(&manifest.id))
            .map(|manifest| {
                let file = row(vault, &stores, &manifest);
                let notes = manifest
                    .notes
                    .iter()
                    .map(|id| {
                        conn.query_row(
                            "SELECT coalesce(title, id),
                                    coalesce(description, substr(body, 1, 120)),
                                    tags, generated_at
                             FROM notes WHERE id = ?1",
                            [id],
                            |record| {
                                Ok(FileNote {
                                    id: id.clone(),
                                    title: record.get::<_, String>(0)?,
                                    snippet: record
                                        .get::<_, Option<String>>(1)?
                                        .unwrap_or_default()
                                        .replace('\n', " "),
                                    tags: record
                                        .get::<_, Option<String>>(2)?
                                        .unwrap_or_default()
                                        .split_whitespace()
                                        .map(String::from)
                                        .collect(),
                                    updated: record.get::<_, Option<String>>(3)?,
                                })
                            },
                        )
                        // 索引に無いノート(まだ取り込まれていない等)でも行は出す
                        .unwrap_or_else(|_| FileNote {
                            id: id.clone(),
                            title: id.clone(),
                            snippet: String::new(),
                            tags: Vec::new(),
                            updated: None,
                        })
                    })
                    .collect();
                FileCard {
                    id: file.id,
                    version: file.version,
                    name: file.name,
                    size: file.size,
                    media_type: file.media_type,
                    availability: file.availability,
                    sensitivity: file.sensitivity,
                    sync: file.sync,
                    linked: file.linked,
                    client_repo: file.client_repo,
                    can_fetch: file.can_fetch,
                    added_at: file.added_at,
                    notes,
                }
            })
            .collect();
        files.sort_by(|a, b| {
            b.added_at
                .cmp(&a.added_at)
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(FilesPage { files, degraded })
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
    let note_id = kb_core::note_id::NoteId::parse(&note_id)
        .map_err(AppError::invalid_input)?
        .to_string();
    state.with_artifacts(|vault, stores, ledger, workspace_id| {
        let req = intake::Request {
            note_id: Some(note_id.clone()),
            display_name: name.clone(),
            media_type: intake::guess_media_type(src),
            role: Role::File,
            // 区分はコアが場所から決める。画面が既定を持つと、
            // 画面を通らない経路と食い違う(決定6)
            policy: None,
            ref_name: None,
            supersedes: supersedes.clone(),
            origin: origin.to_string(),
            by: kb_core::OWNER_ACTOR.to_string(),
            at: kb_core::frontmatter::now_iso(),
        };
        let taken = intake::take(vault, stores, ledger, workspace_id, src, req)
            .map_err(AppError::storage)?;
        Ok(Added {
            file: row(vault, stores, &taken.manifest),
            warn_over_bytes: taken.warn_over,
            forced_local_only: taken.forced_local_only,
            delivery: taken.delivery,
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
    let path = resolved_export(&state, id, ExportPurpose::External)?;
    open_with_os(&app, &path)
}

/// resolver済みの内容を、ユーザーが保存ダイアログで選んだ場所へ複製する。
/// 保存先パスをWebViewから受け取らないため、invokeだけで任意ファイルを上書きできない。
#[tauri::command]
#[specta::specta]
pub async fn file_download(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> AppResult<bool> {
    use tauri_plugin_dialog::DialogExt;

    let source = resolved_export(&state, id, ExportPurpose::External)?;
    let name = source
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let Some(selected) = app.dialog().file().set_file_name(name).blocking_save_file() else {
        return Ok(false);
    };
    let destination = selected.into_path().map_err(AppError::unexpected)?;
    std::fs::copy(source, destination)?;
    Ok(true)
}

/// アプリ内プレビュー用。元の場所ではなく、resolver 済みの一時コピーだけを返す。
#[tauri::command]
#[specta::specta]
pub fn file_preview(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> AppResult<PreviewFile> {
    let path = resolved_export(&state, id, ExportPurpose::Preview)?;
    let text = if is_text_preview(&path) {
        read_text_preview(&path)?
    } else {
        None
    };
    app.asset_protocol_scope()
        .allow_file(&path)
        .map_err(AppError::unexpected)?;
    Ok(PreviewFile {
        path: path.to_string_lossy().into_owned(),
        text,
    })
}

fn resolved_export(
    state: &State<'_, AppState>,
    id: String,
    purpose: ExportPurpose,
) -> AppResult<PathBuf> {
    let id = artifact_id(&id)?;
    state.with_artifacts(|vault, stores, ledger, _| {
        let resolved = resolve::resolve(vault, stores, ledger, &resolve::Link::Fixed(id.clone()))
            .map_err(AppError::storage)?
            .ok_or_else(|| AppError::invalid_input(anyhow::anyhow!("台帳に無い: {id}")))?;
        // 手元に無い / 方針で閉じている場合、ここが None を返す
        let mut file = resolved
            .open(vault, stores)
            .map_err(AppError::storage)?
            .ok_or(AppError::FileNotHere)?;
        export(&resolved.manifest, &mut file, purpose)
    })
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
        let path = vault
            .legacy_attachment_path(&note_id, &name)
            .map_err(AppError::invalid_input)?;
        if !path.is_file() {
            return Err(AppError::FileNotHere);
        }
        Ok(path)
    })?;
    open_with_os(&app, &path)
}

/// 表示名を付けた複製を一時領域へ。内容は不変なので、同じものがあれば使い回す。
/// HTMLプレビューだけは、文字コード宣言がない場合にUTF-8 BOMを一時コピーへ補う。
fn export(m: &Manifest, src: &mut std::fs::File, purpose: ExportPurpose) -> AppResult<PathBuf> {
    // 表示名は直せる field なので、パスとして解釈させない
    let name = Path::new(&m.display_name)
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("file"));
    let root = match purpose {
        ExportPurpose::External => "kb-app-open",
        ExportPurpose::Preview => "kb-app-preview",
    };
    let dir = std::env::temp_dir().join(root).join(&m.hash.as_str()[..12]);
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(name);
    let add_utf8_bom = purpose == ExportPurpose::Preview
        && is_html(m)
        && html_needs_utf8_bom(src).map_err(AppError::unexpected)?;
    let expected_size = m.created.size + u64::from(add_utf8_bom) * UTF8_BOM.len() as u64;
    if std::fs::metadata(&dest).map(|meta| meta.len()).ok() != Some(expected_size) {
        let mut out = std::fs::File::create(&dest)?;
        if add_utf8_bom {
            out.write_all(UTF8_BOM)?;
        }
        // 固定サイズの塊で写す(全量をメモリに載せない — 決定8 と同じ理由)
        std::io::copy(src, &mut out)?;
    }
    Ok(dest)
}

fn is_html(m: &Manifest) -> bool {
    m.created.media_type.eq_ignore_ascii_case("text/html")
        || Path::new(&m.display_name)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("html") || extension.eq_ignore_ascii_case("htm")
            })
}

/// WebViewが文字コードを誤推測しないよう、宣言のないHTMLだけをUTF-8として示す。
/// 読み取り位置は必ず先頭へ戻し、後続のstreaming copyへ影響させない。
fn html_needs_utf8_bom(src: &mut std::fs::File) -> std::io::Result<bool> {
    let mut prefix = vec![0; HTML_ENCODING_SCAN_BYTES];
    let read = src.read(&mut prefix)?;
    src.rewind()?;
    prefix.truncate(read);

    let has_bom = prefix.starts_with(UTF8_BOM)
        || prefix.starts_with(&[0xff, 0xfe])
        || prefix.starts_with(&[0xfe, 0xff]);
    let declared = String::from_utf8_lossy(&prefix)
        .to_ascii_lowercase()
        .contains("charset");
    Ok(!has_bom && !declared)
}

fn is_text_preview(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "md" | "markdown"
                    | "txt"
                    | "csv"
                    | "tsv"
                    | "json"
                    | "yaml"
                    | "yml"
                    | "xml"
                    | "css"
                    | "js"
                    | "jsx"
                    | "ts"
                    | "tsx"
            )
        })
}

fn read_text_preview(path: &Path) -> AppResult<Option<String>> {
    if std::fs::metadata(path)?.len() > TEXT_PREVIEW_MAX_BYTES {
        return Ok(None);
    }
    let bytes = std::fs::read(path)?;
    Ok(Some(decode_text(&bytes)))
}

/// UTF-8を正常系にしつつ、表計算ソフト由来のCSVで多いUTF-16/Shift_JISも読む。
fn decode_text(bytes: &[u8]) -> String {
    if let Some(bytes) = bytes.strip_prefix(UTF8_BOM) {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    if let Some(bytes) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return encoding_rs::UTF_16LE.decode(bytes).0.into_owned();
    }
    if let Some(bytes) = bytes.strip_prefix(&[0xfe, 0xff]) {
        return encoding_rs::UTF_16BE.decode(bytes).0.into_owned();
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    encoding_rs::SHIFT_JIS.decode(bytes).0.into_owned()
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
    let note_id = kb_core::note_id::NoteId::parse(&note_id)
        .map_err(AppError::invalid_input)?
        .to_string();
    state.with_artifacts(|vault, _, ledger, _| {
        let mut manifest = ledger
            .get(&id)
            .map_err(AppError::storage)?
            .ok_or_else(|| AppError::invalid_input(anyhow::anyhow!("台帳に無い: {id}")))?;
        manifest.detach(expected_version, &note_id)?;
        manifest.record(
            &kb_core::frontmatter::now_iso(),
            "detached",
            &format!("{note_id} から外した"),
        );
        ledger.put(vault, &manifest).map_err(AppError::storage)
    })
}

/// どのノートからも外れたファイル。**外しただけでは実体が残る**ので、
/// ここで拾えないと置き場に溜まり続ける(ADR kb-app/artifact-deletion)。
#[tauri::command]
#[specta::specta]
pub fn files_orphans(state: State<'_, AppState>) -> AppResult<Vec<FileRow>> {
    state.with_artifacts(|vault, stores, ledger, _| {
        Ok(kb_core::purge::orphans(ledger)
            .iter()
            .map(|m| row(vault, stores, m))
            .collect())
    })
}

/// purge の下見。**まだ消さない。** 実体を道連れにするか、確認が要るかを返す。
#[tauri::command]
#[specta::specta]
pub fn file_purge_plan(
    state: State<'_, AppState>,
    id: String,
    reason: String,
) -> AppResult<kb_core::purge::PurgePlan> {
    let id = artifact_id(&id)?;
    state.with_purges(|_, _, ledger, purges| {
        purges
            .prepare(ledger, &id, &reason)
            .map_err(AppError::invalid_input)
    })
}

/// 下見どおりなら取り除く。
///
/// `confirmed` は**画面が本人へ訊いたときだけ** true。原本の無い実体(MCP 添付)を
/// 消すときに要る。履歴からは消えないので、画面は「完全に削除」と書かない。
#[tauri::command]
#[specta::specta]
pub fn file_purge_commit(
    state: State<'_, AppState>,
    id: String,
    token: String,
    confirmed: bool,
) -> AppResult<kb_core::purge::Purged> {
    let id = artifact_id(&id)?;
    state.with_purges(|vault, stores, ledger, purges| {
        purges
            .commit(
                kb_core::purge::Workspace {
                    vault,
                    stores,
                    ledger,
                },
                &id,
                &token,
                confirmed,
                &kb_core::frontmatter::now_iso(),
            )
            .map_err(AppError::invalid_input)
    })
}

/// 手元に無い実体を取り寄せる。戻り値は取り寄せた後の状態(都度算出)。
#[tauri::command]
#[specta::specta]
pub fn file_fetch(state: State<'_, AppState>, id: String) -> AppResult<Availability> {
    let id = artifact_id(&id)?;
    state.with_artifacts(|vault, stores, ledger, _| {
        let manifest = ledger
            .get(&id)
            .map_err(AppError::storage)?
            .ok_or_else(|| AppError::invalid_input(anyhow::anyhow!("台帳に無い: {id}")))?;
        kb_core::lfs::fetch(vault, &manifest.hash).map_err(AppError::backup)?;
        Ok(availability(vault, stores, &manifest))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_file(bytes: &[u8]) -> std::fs::File {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        file.rewind().unwrap();
        file
    }

    #[test]
    fn html_without_an_encoding_declaration_gets_a_utf8_hint() {
        let bytes = "<!doctype html><title>日本語</title>".as_bytes();
        let mut file = temporary_file(bytes);

        assert!(html_needs_utf8_bom(&mut file).unwrap());

        let mut untouched = Vec::new();
        file.read_to_end(&mut untouched).unwrap();
        assert_eq!(untouched, bytes);
    }

    #[test]
    fn declared_or_bom_prefixed_html_keeps_its_own_encoding() {
        let mut declared = temporary_file(b"<meta charset=\"shift_jis\"><title>page</title>");
        assert!(!html_needs_utf8_bom(&mut declared).unwrap());

        let mut bom = temporary_file(b"\xef\xbb\xbf<!doctype html><title>page</title>");
        assert!(!html_needs_utf8_bom(&mut bom).unwrap());
    }

    #[test]
    fn text_preview_decodes_utf8_utf16_and_shift_jis() {
        assert_eq!(decode_text("名前,値\n鬼,10".as_bytes()), "名前,値\n鬼,10");

        let utf16: Vec<u8> = [0xff, 0xfe]
            .into_iter()
            .chain("名前,値".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(decode_text(&utf16), "名前,値");

        let shift_jis = encoding_rs::SHIFT_JIS.encode("名前,値").0;
        assert_eq!(decode_text(&shift_jis), "名前,値");
    }
}
