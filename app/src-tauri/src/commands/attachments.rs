//! 添付(FR-C8)。
//!
//! WKWebView は DOM の paste にクリップボード画像を渡さず、ファイルの DND も
//! DOM の drop に来ない。どちらもブラウザでは動くため検証を素通りする類の罠で、
//! ここが Rust 側のフォールバック経路になっている。

use base64::Engine;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// 戻り値は (保存名, 警告)。警告は保存できたが注意が要る場合(名前の衝突など)。
type Saved = (String, Option<String>);

const MB: u64 = 1024 * 1024;

/// 添付の追加。データは base64。
#[tauri::command]
#[specta::specta]
pub fn attachment_add(
    state: State<'_, AppState>,
    id: String,
    name: String,
    data_base64: String,
) -> AppResult<Saved> {
    let data = base64::engine::general_purpose::STANDARD
        .decode(data_base64.as_bytes())
        .map_err(AppError::unexpected)?;
    state.with_vault(|vault| {
        vault
            .add_attachment(&id, &name, &data)
            .map_err(AppError::from)
    })
}

/// パス指定で添付(ドラッグ&ドロップ用)。Tauri はファイルドロップを DOM に渡さず
/// 自前イベントでパスをくれるので、Rust 側で直接読む(base64 経由より大きいファイルに強い)。
#[tauri::command]
#[specta::specta]
pub fn attachment_add_from_path(
    state: State<'_, AppState>,
    id: String,
    path: String,
) -> AppResult<Saved> {
    let p = std::path::Path::new(&path);
    let size = std::fs::metadata(p)?.len();
    if size > kb_core::vault::ATTACH_MAX_BYTES {
        return Err(AppError::AttachmentTooLarge {
            limit_mb: (kb_core::vault::ATTACH_MAX_BYTES / MB) as u32,
            actual_mb: (size / MB) as u32,
        });
    }
    let data = std::fs::read(p)?;
    let name = p.file_name().and_then(|f| f.to_str()).unwrap_or("file");
    state.with_vault(|vault| {
        vault
            .add_attachment(&id, name, &data)
            .map_err(AppError::from)
    })
}

/// クリップボードの画像を添付(ペーストのフォールバック)。
/// 画像が無ければ Ok(None) — テキストのペーストを邪魔しない。
#[tauri::command]
#[specta::specta]
pub fn attachment_paste(state: State<'_, AppState>, id: String) -> AppResult<Option<Saved>> {
    let mut clipboard = arboard::Clipboard::new().map_err(AppError::unexpected)?;
    let Ok(img) = clipboard.get_image() else {
        return Ok(None);
    };
    let rgba =
        image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned())
            .ok_or_else(|| AppError::unexpected("クリップボード画像の変換に失敗"))?;
    let mut png: Vec<u8> = Vec::new();
    image::DynamicImage::ImageRgba8(rgba)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(AppError::unexpected)?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    state.with_vault(|vault| {
        vault
            .add_attachment(&id, &format!("pasted-{ts}.png"), &png)
            .map(Some)
            .map_err(AppError::from)
    })
}

#[tauri::command]
#[specta::specta]
pub fn attachment_remove(state: State<'_, AppState>, id: String, name: String) -> AppResult<()> {
    state.with_vault(|vault| vault.remove_attachment(&id, &name).map_err(AppError::from))
}
