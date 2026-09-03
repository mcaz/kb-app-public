//! ログイン時の自動起動登録。
//!
//! kb-app は常駐してtrayから開く形にしたため、OSごとのログイン項目をアプリ自身が
//! 管理する。`tauri-plugin-autostart` を採らなかったのは、依存を1つ増やしても
//! 得られるのが3つのfile/registry操作だけで、生成物(plist・desktop entry・Run値)を
//! 単体テストできる形にした方が、OSごとの差を見落とさずに済むため
//! (ADR-0017 / `docs/coding-guidelines.md` §7「LLMが間違えたときに気づけるか」)。
//! OS別providerとして切る形は `ai_guard` と揃えてある。

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

/// 自動起動で立ち上げるときに渡す引数。画面を出さずtrayだけを出す。
pub const HIDDEN_FLAG: &str = "--hidden";

/// ログイン項目の識別子。bundle identifier と同じにして他アプリと衝突させない。
const LABEL: &str = "app.kb.desktop";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct AutostartState {
    pub enabled: bool,
    /// このOSでログイン項目を登録できるか。false のとき `enabled` は常に false。
    pub supported: bool,
}

/// ログイン項目の置き場。OSごとに実体が違うので、判定と読み書きをここへ集約する。
enum LoginItem {
    /// macOS: `~/Library/LaunchAgents/<label>.plist`
    LaunchAgent(PathBuf),
    /// Linux/BSD: `~/.config/autostart/kb-app.desktop`
    DesktopEntry(PathBuf),
    /// Windows: HKCU の Run キー(fileではなくregistry)
    RunKey,
    Unsupported,
}

pub fn state(exe: &Path) -> Result<AutostartState> {
    let item = login_item()?;
    Ok(AutostartState {
        // 別の場所を指す古い登録は「有効」と言わない。実行ファイルが移動しても
        // 画面のswitchが真を映すようにする(修復は initialize が行う)。
        enabled: registered_program(&item)?.is_some_and(|program| program == expected_program(exe)),
        supported: !matches!(item, LoginItem::Unsupported),
    })
}

pub fn set(exe: &Path, enabled: bool) -> Result<AutostartState> {
    let item = login_item()?;
    if enabled {
        write_login_item(&item, exe)?;
    } else {
        remove_login_item(&item)?;
    }
    state(exe)
}

/// 起動時に1回だけ通す初期化と自己修復。
///
/// 既定を有効にするのは初回の1回だけで、以後はユーザーの操作(アプリ内switchでも
/// OSのログイン項目設定でも)に従う。外した登録を毎回復活させない。
/// 登録が残っていて実行ファイルの場所だけが変わっている場合は書き直す
/// (`/Applications` と `~/Applications` の移動で無言のまま起動しなくなるため)。
pub fn initialize(exe: &Path) -> Result<AutostartState> {
    let item = login_item()?;
    if matches!(item, LoginItem::Unsupported) {
        return state(exe);
    }

    let registered = registered_program(&item)?;
    let settings = crate::settings::load()?;
    if !settings.launch_at_login_initialized {
        if registered.is_none() {
            write_login_item(&item, exe)?;
        }
        // 登録に成功してから印を付ける。先に付けると、失敗した初回が再試行されない。
        crate::settings::mark_launch_at_login_initialized()?;
    }

    if registered.is_some_and(|program| program != expected_program(exe)) {
        write_login_item(&item, exe)?;
    }
    state(exe)
}

fn login_item() -> Result<LoginItem> {
    if cfg!(target_os = "macos") {
        Ok(LoginItem::LaunchAgent(
            home()?.join("Library/LaunchAgents").join(plist_name()),
        ))
    } else if cfg!(target_os = "windows") {
        Ok(LoginItem::RunKey)
    } else if cfg!(unix) {
        Ok(LoginItem::DesktopEntry(
            config_dir()?.join(DESKTOP_ENTRY_PATH),
        ))
    } else {
        Ok(LoginItem::Unsupported)
    }
}

fn write_login_item(item: &LoginItem, exe: &Path) -> Result<()> {
    match item {
        LoginItem::LaunchAgent(path) => write_file(path, &launch_agent_plist(exe)),
        LoginItem::DesktopEntry(path) => write_file(path, &desktop_entry(exe)),
        LoginItem::RunKey => write_run_key(&run_key_value(exe)),
        LoginItem::Unsupported => Err(CoreError::configuration(anyhow::anyhow!(
            "このOSでは自動起動を登録できない"
        ))),
    }
}

fn remove_login_item(item: &LoginItem) -> Result<()> {
    match item {
        LoginItem::LaunchAgent(path) | LoginItem::DesktopEntry(path) => {
            match std::fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(CoreError::configuration(error)),
            }
        }
        LoginItem::RunKey => remove_run_key(),
        LoginItem::Unsupported => Ok(()),
    }
}

/// 登録済みなら、そこに書かれている実行ファイルのpathを返す。
fn registered_program(item: &LoginItem) -> Result<Option<String>> {
    match item {
        LoginItem::LaunchAgent(path) => {
            Ok(read_file(path)?.as_deref().and_then(launch_agent_program))
        }
        LoginItem::DesktopEntry(path) => Ok(read_file(path)?
            .as_deref()
            .and_then(desktop_entry_program)
            .map(str::to_owned)),
        LoginItem::RunKey => read_run_key(),
        LoginItem::Unsupported => Ok(None),
    }
}

/// 登録に書く実行ファイルのpath。突き合わせと書き込みで同じ形を使う。
fn expected_program(exe: &Path) -> String {
    exe.to_string_lossy().into_owned()
}

fn write_file(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .context("ログイン項目に親ディレクトリがない")
        .map_err(CoreError::configuration)?;
    std::fs::create_dir_all(parent).map_err(CoreError::configuration)?;
    std::fs::write(path, contents).map_err(CoreError::configuration)
}

fn read_file(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(CoreError::configuration(error)),
    }
}

fn home() -> Result<PathBuf> {
    dirs::home_dir()
        .context("ホームディレクトリが特定できない")
        .map_err(CoreError::configuration)
}

fn config_dir() -> Result<PathBuf> {
    dirs::config_dir()
        .context("設定ディレクトリが特定できない")
        .map_err(CoreError::configuration)
}

fn plist_name() -> String {
    format!("{LABEL}.plist")
}

const DESKTOP_ENTRY_PATH: &str = "autostart/kb-app.desktop";

// ---- 生成物 ----
//
// 中身の生成はOSに依存しない純関数にして、`#[cfg]` の向こう側でしか読めない
// 文字列を作らないようにする(macOS以外のCIでも形を検査できる)。

fn launch_agent_plist(exe: &Path) -> String {
    // KeepAlive は置かない。trayの「終了」で切ったものを launchd が起こし直すと、
    // ユーザーの操作が無効になる。RunAtLoad だけで次回ログインから復帰する。
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{program}</string>
		<string>{HIDDEN_FLAG}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>ProcessType</key>
	<string>Interactive</string>
</dict>
</plist>
"#,
        program = xml_escape(&expected_program(exe)),
    )
}

fn launch_agent_program(plist: &str) -> Option<String> {
    let array = plist
        .split_once("<key>ProgramArguments</key>")?
        .1
        .split_once("<array>")?
        .1
        .split_once("</array>")?
        .0;
    let value = array.split_once("<string>")?.1.split_once("</string>")?.0;
    Some(xml_unescape(value.trim()))
}

fn desktop_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=kb-app\n\
         Exec=\"{program}\" {HIDDEN_FLAG}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n",
        program = expected_program(exe),
    )
}

fn desktop_entry_program(entry: &str) -> Option<&str> {
    entry
        .lines()
        .find_map(|line| line.strip_prefix("Exec="))
        .map(unquote_program)
}

/// Run値・desktop entry の `"path" --hidden` から実行ファイル部分だけを取り出す。
fn unquote_program(command: &str) -> &str {
    let command = command.trim();
    match command.strip_prefix('"') {
        Some(rest) => rest.split_once('"').map_or(rest, |(program, _)| program),
        None => command
            .split_once(' ')
            .map_or(command, |(program, _)| program),
    }
}

/// Windows の Run 値。空白を含む path を1引数として渡すため常に引用する。
fn run_key_value(exe: &Path) -> String {
    format!("\"{}\" {HIDDEN_FLAG}", expected_program(exe))
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

// ---- Windows registry ----
//
// `winreg` を足さずに `reg.exe` を通す。GUIから起動するのでconsole窓が出ないよう
// CREATE_NO_WINDOW を付ける。実機検証は未了(KB「macOSとWindowsを並行検証」)。

#[cfg(target_os = "windows")]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

#[cfg(target_os = "windows")]
fn reg_command() -> std::process::Command {
    use std::os::windows::process::CommandExt;

    let mut command = std::process::Command::new("reg");
    command.creation_flags(0x0800_0000);
    command
}

#[cfg(target_os = "windows")]
fn read_run_key() -> Result<Option<String>> {
    let output = reg_command()
        .args(["query", RUN_KEY, "/v", LABEL])
        .output()
        .map_err(CoreError::configuration)?;
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .find_map(|line| line.split_once("REG_SZ"))
        .map(|(_, value)| unquote_program(value).to_owned()))
}

#[cfg(target_os = "windows")]
fn write_run_key(value: &str) -> Result<()> {
    let status = reg_command()
        .args([
            "add", RUN_KEY, "/v", LABEL, "/t", "REG_SZ", "/d", value, "/f",
        ])
        .status()
        .map_err(CoreError::configuration)?;
    if status.success() {
        return Ok(());
    }
    Err(CoreError::configuration(anyhow::anyhow!(
        "ログイン項目を登録できなかった"
    )))
}

#[cfg(target_os = "windows")]
fn remove_run_key() -> Result<()> {
    // 値が無いときも失敗終了するため、消えている状態は成功として扱う。
    reg_command()
        .args(["delete", RUN_KEY, "/v", LABEL, "/f"])
        .status()
        .map_err(CoreError::configuration)?;
    if read_run_key()?.is_none() {
        return Ok(());
    }
    Err(CoreError::configuration(anyhow::anyhow!(
        "ログイン項目を解除できなかった"
    )))
}

#[cfg(not(target_os = "windows"))]
fn read_run_key() -> Result<Option<String>> {
    Ok(None)
}

// Windows以外で `LoginItem::RunKey` は作られない。到達したら黙って成功と言わない。
#[cfg(not(target_os = "windows"))]
fn write_run_key(_value: &str) -> Result<()> {
    Err(unavailable_run_key())
}

#[cfg(not(target_os = "windows"))]
fn remove_run_key() -> Result<()> {
    Err(unavailable_run_key())
}

#[cfg(not(target_os = "windows"))]
fn unavailable_run_key() -> CoreError {
    CoreError::configuration(anyhow::anyhow!("Run キーはWindowsにしかない"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe() -> PathBuf {
        PathBuf::from("/Applications/kb-app.app/Contents/MacOS/kb-app")
    }

    #[test]
    fn launch_agent_starts_the_installed_binary_without_a_window() {
        let plist = launch_agent_plist(&exe());
        assert!(plist.contains("<string>app.kb.desktop</string>"));
        assert!(plist.contains("<string>/Applications/kb-app.app/Contents/MacOS/kb-app</string>"));
        assert!(plist.contains("<string>--hidden</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
        // trayの「終了」を launchd が打ち消さないこと。
        assert!(!plist.contains("KeepAlive"));
    }

    #[test]
    fn launch_agent_program_round_trips() {
        assert_eq!(
            launch_agent_program(&launch_agent_plist(&exe())),
            Some(expected_program(&exe()))
        );
        assert_eq!(launch_agent_program("<plist></plist>"), None);
    }

    /// path に `&` を含む利用者がいると、plist ごと壊れて自動起動が黙って止まる。
    /// 読み戻しでescapeを解かないと、毎回「別のpath」と判定して書き直し続ける。
    #[test]
    fn launch_agent_escapes_xml_significant_characters() {
        let moved = Path::new("/Users/a&b/kb-app");
        let plist = launch_agent_plist(moved);
        assert!(plist.contains("<string>/Users/a&amp;b/kb-app</string>"));
        assert_eq!(launch_agent_program(&plist), Some(expected_program(moved)));
    }

    #[test]
    fn desktop_entry_program_round_trips() {
        let entry = desktop_entry(Path::new("/opt/kb app/kb-app"));
        assert!(entry.contains("Exec=\"/opt/kb app/kb-app\" --hidden"));
        assert_eq!(desktop_entry_program(&entry), Some("/opt/kb app/kb-app"));
    }

    /// 空白入りのpathを引用しないと、Windowsは前半だけを実行ファイルとして扱う。
    #[test]
    fn run_key_value_quotes_the_program() {
        let value = run_key_value(Path::new(r"C:\Program Files\kb-app\kb-app.exe"));
        assert_eq!(value, r#""C:\Program Files\kb-app\kb-app.exe" --hidden"#);
        assert_eq!(
            unquote_program(&value),
            r"C:\Program Files\kb-app\kb-app.exe"
        );
    }

    #[test]
    fn unquote_program_handles_unquoted_values() {
        assert_eq!(
            unquote_program("/usr/bin/kb-app --hidden"),
            "/usr/bin/kb-app"
        );
        assert_eq!(unquote_program("  /usr/bin/kb-app  "), "/usr/bin/kb-app");
    }

    #[test]
    fn entry_targets_this_platform() {
        let item = login_item().unwrap();
        if cfg!(target_os = "macos") {
            assert!(matches!(item, LoginItem::LaunchAgent(_)));
        } else if cfg!(target_os = "windows") {
            assert!(matches!(item, LoginItem::RunKey));
        } else if cfg!(unix) {
            assert!(matches!(item, LoginItem::DesktopEntry(_)));
        }
    }

    #[cfg(unix)]
    #[test]
    fn writing_and_removing_a_file_entry_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let item = if cfg!(target_os = "macos") {
            LoginItem::LaunchAgent(dir.path().join("LaunchAgents").join(plist_name()))
        } else {
            LoginItem::DesktopEntry(dir.path().join(DESKTOP_ENTRY_PATH))
        };

        assert_eq!(registered_program(&item).unwrap(), None);
        write_login_item(&item, &exe()).unwrap();
        assert_eq!(
            registered_program(&item).unwrap().as_deref(),
            Some("/Applications/kb-app.app/Contents/MacOS/kb-app")
        );

        // 消えている状態からの解除も成功にする(二重解除で失敗させない)。
        remove_login_item(&item).unwrap();
        remove_login_item(&item).unwrap();
        assert_eq!(registered_program(&item).unwrap(), None);
    }

    /// アプリを `~/Applications` から `/Applications` へ移すと、登録は残ったまま
    /// 存在しない実行ファイルを指す。switchは「有効」と言ってはいけない。
    #[cfg(unix)]
    #[test]
    fn a_stale_program_path_is_not_reported_as_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let item = LoginItem::DesktopEntry(dir.path().join(DESKTOP_ENTRY_PATH));
        write_login_item(&item, Path::new("/old/kb-app")).unwrap();

        let registered = registered_program(&item).unwrap();
        assert_eq!(registered.as_deref(), Some("/old/kb-app"));
        assert_ne!(registered, Some(expected_program(&exe())));
    }
}
