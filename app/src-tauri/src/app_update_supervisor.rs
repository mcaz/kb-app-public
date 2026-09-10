//! GUIとは別の旧実行物が差し替えを監督する。MCPを終了する経路は持たない。
//! 起動受付はmainの最初、UI受入は初期query成功後。DB移行の復元には使わない。

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use kb_core::app_update_package::ValidatedPackage;
use kb_core::app_update_transaction::{
    self as transaction, BundleIdentity, Phase, StartupReceipt, Transaction,
};
use serde::{Deserialize, Serialize};

use crate::app_update_runtime::UpdateFailureKind;
use crate::error::{AppError, AppResult};

const SUPERVISOR_FLAG: &str = "--app-update-supervisor";
const BOOT_FLAG: &str = "--app-update-boot";
const RECOVER_FLAG: &str = "--recover-app-update";
const WAIT_FOR_GUI_EXIT: Duration = Duration::from_secs(30);
const WAIT_FOR_BOOT: Duration = Duration::from_secs(90);
const STABLE_BOOT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(200);
const RECEIPT_LIMIT: u64 = 4096;
static BOOT: OnceLock<BootContext> = OnceLock::new();

struct BootContext {
    directory: PathBuf,
    receipt: StartupReceipt,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SupervisorReady {
    nonce: String,
    gui_pid: u32,
    supervisor_pid: u32,
}

pub fn target_triple() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("aarch64-apple-darwin")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("x86_64-apple-darwin")
    } else {
        None
    }
}

fn bundle_for_executable(executable: &Path) -> Result<PathBuf> {
    ensure!(
        executable.file_name().is_some_and(|name| name == "kb-app"),
        "実行物の名前が違う"
    );
    let macos = executable.parent().context("実行物の親がない")?;
    ensure!(
        macos.file_name().is_some_and(|name| name == "MacOS"),
        "bundleの実行物ではない"
    );
    let contents = macos.parent().context("Contentsがない")?;
    ensure!(
        contents.file_name().is_some_and(|name| name == "Contents"),
        "bundleの実行物ではない"
    );
    let bundle = contents.parent().context("bundleがない")?;
    ensure!(
        bundle.file_name().is_some_and(|name| name == "kb-app.app"),
        "更新対象のbundleではない"
    );
    Ok(bundle.to_owned())
}

fn current_bundle() -> Result<PathBuf> {
    bundle_for_executable(&std::env::current_exe()?.canonicalize()?)
}

fn logged(error: anyhow::Error, kind: UpdateFailureKind) -> UpdateFailureKind {
    eprintln!("kb-app update: {error:#}");
    kind
}

pub fn current_identity() -> std::result::Result<BundleIdentity, UpdateFailureKind> {
    let app = current_bundle().map_err(|e| logged(e, UpdateFailureKind::Storage))?;
    verify_signature(&app).map_err(|e| logged(e, UpdateFailureKind::Signature))?;
    transaction::bundle_identity(&app).map_err(|e| logged(e, UpdateFailureKind::Storage))
}

// 証明書の更新ではTeam IDを変えず、更新署名とは別にmacOSの配布署名も保つ。
fn verify_signature(app: &Path) -> Result<String> {
    ensure!(target_triple().is_some(), "未対応OS");
    let status = Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(app)
        .output()?;
    ensure!(status.status.success(), "bundle署名を検証できない");
    let details = Command::new("/usr/bin/codesign")
        .args(["--display", "--verbose=4"])
        .arg(app)
        .output()?;
    ensure!(details.status.success(), "bundle署名を読めない");
    let details = String::from_utf8(details.stderr)?;
    ensure!(
        details.lines().any(|s| s == "Identifier=app.kb.desktop"),
        "署名のidentifierが違う"
    );
    ensure!(
        details
            .lines()
            .any(|s| s.starts_with("Authority=Developer ID Application:")),
        "配布署名がない"
    );
    ensure!(
        details
            .lines()
            .any(|s| s.starts_with("CodeDirectory ") && s.contains("runtime")),
        "hardened runtimeがない"
    );
    let team = details
        .lines()
        .find_map(|s| s.strip_prefix("TeamIdentifier="))
        .context("署名Teamがない")?;
    ensure!(
        team.len() == 10
            && team
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()),
        "署名Teamが不正"
    );
    let arch = Command::new("/usr/bin/lipo")
        .arg("-archs")
        .arg(app.join("Contents/MacOS/kb-app"))
        .output()?;
    ensure!(arch.status.success(), "実行物のCPUを確認できない");
    let arch = String::from_utf8(arch.stdout)?;
    let expected = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x86_64"
    };
    ensure!(arch.trim() == expected, "実行物のCPUが違う");
    Ok(team.to_owned())
}

pub fn prepare(
    package: Arc<ValidatedPackage>,
    expected_source: &BundleIdentity,
) -> std::result::Result<(), UpdateFailureKind> {
    if target_triple().is_none() {
        return Err(UpdateFailureKind::UnsupportedPlatform);
    }
    let target = current_bundle().map_err(|e| logged(e, UpdateFailureKind::Storage))?;
    let source = current_identity()?;
    if source != *expected_source
        || !package.supports_source(&source.plan_sha256, &source.executable_sha256)
        || !kb_core::app_update_compatibility::inspect(package.compatibility()).checks_passed
    {
        return Err(UpdateFailureKind::Incompatible);
    }
    let result = (|| -> Result<()> {
        let mut nonce = [0_u8; 32];
        File::open("/dev/urandom")?.read_exact(&mut nonce)?;
        let nonce = nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let mut transaction = Transaction::create(&target, &nonce)?;
        let prepared = (|| -> Result<()> {
            let stage = package.extract_into(&transaction.staged_parent())?;
            ensure!(
                verify_signature(&stage)? == verify_signature(&target)?,
                "更新前後の署名Teamが違う"
            );
            let candidate = transaction::bundle_identity(&stage)?;
            ensure!(
                candidate.plan_sha256 == package.plan_sha256(),
                "展開前後のplanが違う"
            );
            // 展開と署名検査の間にKBが変わる場合も、この時点の前提を照合する。
            ensure!(
                kb_core::app_update_compatibility::inspect(package.compatibility()).checks_passed,
                "適用直前の互換性検査に失敗"
            );
            transaction.prepare(&source, &candidate, package.compatibility())
        })();
        if let Err(error) = prepared {
            if transaction.journal().is_some() {
                transaction.rollback()?;
            } else {
                transaction.abort_preparation()?;
            }
            return Err(error);
        }
        let ready_path = transaction.directory().join("supervisor.ready");
        drop(transaction);
        let helper_result = Command::new(std::env::current_exe()?)
            .arg(SUPERVISOR_FLAG)
            .arg(&nonce)
            .arg(std::process::id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let mut helper = match helper_result {
            Ok(helper) => helper,
            Err(error) => {
                Transaction::resume(&target)?.rollback()?;
                return Err(error.into());
            }
        };
        let handoff = (|| -> Result<()> {
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(5) {
                if let Some(ready) = read_record::<SupervisorReady>(&ready_path)?
                    && ready.nonce == nonce
                    && ready.gui_pid == std::process::id()
                    && ready.supervisor_pid == helper.id()
                {
                    return Ok(());
                }
                if helper.try_wait()?.is_some() {
                    break;
                }
                std::thread::sleep(POLL);
            }
            bail!("supervisorの準備を確認できない")
        })();
        if let Err(error) = handoff {
            if helper.try_wait()?.is_none() {
                helper.kill()?;
            }
            helper.wait()?;
            Transaction::resume(&target)?.rollback()?;
            return Err(error);
        }
        Ok(())
    })();
    result.map_err(|e| logged(e, UpdateFailureKind::InstallFailed))
}

/// どの起動面もDBを開く前に通す。trueなら専用処理または拒否を完了済み。
pub fn run_before_app() -> bool {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == SUPERVISOR_FLAG) {
        let result = (|| -> Result<()> {
            ensure!(args.len() == 3, "supervisor引数が不正");
            let pid: u32 = args[2].parse()?;
            ensure!(pid > 1 && pid != std::process::id(), "GUI pidが不正");
            supervise(current_bundle()?, &args[1], pid)
        })();
        if let Err(error) = result {
            eprintln!("kb-app update supervisor: {error:#}");
        }
        return true;
    }
    if args.first().is_some_and(|arg| arg == RECOVER_FLAG) {
        if let Err(error) = recover_from_executable(&args) {
            eprintln!("kb-app update recovery: {error:#}");
        }
        return true;
    }
    if std::env::current_exe().is_ok_and(|path| {
        path.components()
            .any(|part| part.as_os_str() == ".kb-app-update")
    }) {
        eprintln!("kb-app update startup: 退避実行物は明示的な更新復元にだけ使えます");
        return true;
    }
    let target = match current_bundle() {
        Ok(target) => target,
        Err(_)
            if !args
                .iter()
                .any(|s| s == BOOT_FLAG || s == SUPERVISOR_FLAG || s == RECOVER_FLAG) =>
        {
            return false;
        }
        Err(error) => {
            eprintln!("kb-app update startup: {error:#}");
            return true;
        }
    };
    match startup_gate(&target, &args) {
        Ok(continue_boot) => !continue_boot,
        Err(error) => {
            eprintln!("kb-app update startup: {error:#}");
            true
        }
    }
}

fn startup_gate(target: &Path, args: &[String]) -> Result<bool> {
    let Some(journal) = transaction::inspect(target)? else {
        ensure!(
            !args.iter().any(|s| s == BOOT_FLAG),
            "更新起動のjournalがない"
        );
        return Ok(true);
    };
    if matches!(journal.phase, Phase::Accepted | Phase::RolledBack) {
        ensure!(!args.iter().any(|s| s == BOOT_FLAG), "更新起動は完了済み");
        return Ok(true);
    }
    if args.len() == 2 && args[0] == BOOT_FLAG && args[1] == journal.nonce {
        ensure!(
            journal.phase == Phase::NewPlaced && journal.intent.is_none(),
            "配置が未完了"
        );
        ensure!(
            transaction::bundle_identity(target)? == journal.new,
            "起動物のidentityが違う"
        );
        verify_signature(target)?;
        ensure!(
            journal.compatibility == kb_core::app_update_compatibility::compiled_metadata(),
            "起動版の永続形式が違う"
        );
        ensure!(
            kb_core::app_update_compatibility::inspect(&journal.compatibility).checks_passed,
            "通常DB open前の互換性検査に失敗"
        );
        let directory = target
            .parent()
            .context("bundleの親がない")?
            .join(".kb-app-update");
        let receipt = StartupReceipt {
            nonce: journal.nonce,
            identity: journal.new,
        };
        write_record(&directory.join("bootstrap.ok"), &receipt)?;
        BOOT.set(BootContext { directory, receipt })
            .map_err(|_| anyhow::anyhow!("更新起動済み"))?;
        return Ok(true);
    }
    // helperが生きていればnonblocking lockで拒否する。旧MCPは稼働を続け、
    // 新たなMCP/hook/recoveryは未受入版で永続形式を開かない。
    ensure!(args.is_empty(), "更新が完了するまで新しいAI接続は待つ");
    let mut pending = Transaction::resume(target)?;
    ensure_no_pending_gui(target, &journal.nonce)?;
    pending.rollback()?;
    launch(target, None)?;
    Ok(false)
}

pub fn boot_ready() -> AppResult<()> {
    let Some(context) = BOOT.get() else {
        return Ok(());
    };
    write_record(&context.directory.join("startup.json"), &context.receipt)
        .map_err(AppError::unexpected)
}

fn supervise(target: PathBuf, nonce: &str, gui_pid: u32) -> Result<()> {
    let mut pending = Transaction::resume(&target)?;
    let journal = pending.journal().context("準備済みjournalがない")?.clone();
    ensure!(
        journal.nonce == nonce && journal.phase == Phase::Prepared,
        "準備とsupervisorが一致しない"
    );
    write_record(
        &pending.directory().join("supervisor.ready"),
        &SupervisorReady {
            nonce: nonce.to_owned(),
            gui_pid,
            supervisor_pid: std::process::id(),
        },
    )?;
    let start = Instant::now();
    while process_exists(gui_pid)? {
        if start.elapsed() >= WAIT_FOR_GUI_EXIT {
            pending.rollback()?;
            bail!("GUIの終了を確認できない");
        }
        std::thread::sleep(POLL);
    }
    // GUI終了後に最後の検査を行い、完了前は全bundleを保持する。
    if !kb_core::app_update_compatibility::inspect(&journal.compatibility).checks_passed {
        pending.rollback()?;
        launch(&target, None)?;
        bail!("GUI終了後の互換性検査に失敗");
    }
    if let Err(error) = pending.replace() {
        // replace自身が復元できた場合だけ旧版を起動する。
        if pending
            .journal()
            .is_some_and(|j| j.phase == Phase::RolledBack)
        {
            launch(&target, None)?;
        }
        return Err(error);
    }
    let mut child = match launch(&target, Some(nonce)) {
        Ok(child) => child,
        Err(error) => {
            pending.rollback()?;
            launch(&target, None)?;
            return Err(error);
        }
    };
    let acceptance = wait_for_acceptance(&mut child, pending.directory(), &journal)
        .and_then(|receipt| pending.accept(&receipt));
    match acceptance {
        Ok(()) => Ok(()),
        Err(error) => {
            // journal rename後のfsync失敗では、受入済みの実物を巻き戻さない。
            if transaction::inspect(&target)?.is_some_and(|j| j.phase == Phase::Accepted)
                && transaction::bundle_identity(&target)? == journal.new
            {
                return Err(error);
            }
            if child.try_wait()?.is_none() {
                child.kill()?;
            }
            child.wait()?;
            pending.rollback()?;
            write_record(
                &pending.directory().join("boot-failed.json"),
                &serde_json::json!({"nonce":nonce,"restored":true}),
            )?;
            launch(&target, None)?;
            Err(error)
        }
    }
}

fn wait_for_acceptance(
    child: &mut Child,
    directory: &Path,
    journal: &transaction::Journal,
) -> Result<StartupReceipt> {
    wait_for_acceptance_with_timing(child, directory, journal, WAIT_FOR_BOOT, STABLE_BOOT, POLL)
}

fn wait_for_acceptance_with_timing(
    child: &mut Child,
    directory: &Path,
    journal: &transaction::Journal,
    timeout: Duration,
    stable: Duration,
    poll: Duration,
) -> Result<StartupReceipt> {
    let start = Instant::now();
    let mut received_at = None;
    loop {
        ensure!(child.try_wait()?.is_none(), "更新版が起動中に終了した");
        // 2026-09-09: 長いschedule停止後も、安定期間より先に期限を検査する。
        ensure!(
            start.elapsed() < timeout,
            "初期画面の起動受入が期限内に届かない"
        );
        let bootstrap = read_record::<StartupReceipt>(&directory.join("bootstrap.ok"))?;
        let receipt = read_record::<StartupReceipt>(&directory.join("startup.json"))?;
        if let (Some(bootstrap), Some(receipt)) = (bootstrap, receipt)
            && receipt.nonce == journal.nonce
            && receipt.identity == journal.new
            && bootstrap.nonce == journal.nonce
            && bootstrap.identity == journal.new
        {
            let received_at = received_at.get_or_insert_with(Instant::now);
            if received_at.elapsed() >= stable {
                return Ok(receipt);
            }
        } else {
            received_at = None;
        }
        std::thread::sleep(poll);
    }
}

fn launch(target: &Path, nonce: Option<&str>) -> Result<Child> {
    let _ = Command::new("/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister")
        .arg("-f").arg(target).stdout(Stdio::null()).stderr(Stdio::null()).status();
    let mut command = Command::new(target.join("Contents/MacOS/kb-app"));
    if let Some(nonce) = nonce {
        command.arg(BOOT_FLAG).arg(nonce);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("GUIを起動できない")
}

fn process_exists(pid: u32) -> Result<bool> {
    let status = Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(status.success())
}

fn recover_from_executable(args: &[String]) -> Result<()> {
    ensure!(args.len() == 1, "復元に任意pathは指定できない");
    let executable = std::env::current_exe()?.canonicalize()?;
    let target = match bundle_for_executable(&executable) {
        Ok(target) => target,
        Err(_) => {
            let previous = executable
                .parent()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .context("退避bundleがない")?;
            ensure!(
                previous.file_name().is_some_and(|n| n == "previous.app"),
                "退避実行物ではない"
            );
            let directory = previous.parent().context("復元領域がない")?;
            ensure!(
                directory.file_name().is_some_and(|n| n == ".kb-app-update"),
                "復元領域が違う"
            );
            directory
                .parent()
                .context("配置先がない")?
                .join("kb-app.app")
        }
    };
    let mut pending = Transaction::resume(&target)?;
    let nonce = pending
        .journal()
        .context("復元journalがない")?
        .nonce
        .clone();
    ensure_no_pending_gui(&target, &nonce)?;
    pending.rollback()?;
    launch(&target, None)?;
    Ok(())
}

// supervisor自体のcrash後は、生きている新GUIを推測で終了しない。
// nonceを含む自分の起動行が残る間、明示的復元も待たせる。
fn ensure_no_pending_gui(target: &Path, nonce: &str) -> Result<()> {
    let output = Command::new("/bin/ps")
        .args(["-ww", "-axo", "pid=,command="])
        .output()?;
    ensure!(output.status.success(), "更新GUIの終了を確認できない");
    let listing = String::from_utf8(output.stdout)?;
    let command = format!(
        "{} {BOOT_FLAG} {nonce}",
        target.join("Contents/MacOS/kb-app").display()
    );
    ensure!(
        !listing.lines().any(|line| line
            .trim()
            .split_once(char::is_whitespace)
            .is_some_and(|(_, args)| args.trim() == command)),
        "未受入の更新GUIを終了してから復元する必要がある"
    );
    Ok(())
}

pub fn previous_boot_was_restored() -> bool {
    (|| -> Result<bool> {
        let target = current_bundle()?;
        let Some(journal) = transaction::inspect(&target)? else {
            return Ok(false);
        };
        if journal.phase != Phase::RolledBack {
            return Ok(false);
        }
        let path = target
            .parent()
            .context("bundleの親がない")?
            .join(".kb-app-update/boot-failed.json");
        let value = read_record::<serde_json::Value>(&path)?;
        Ok(value.is_some_and(|v| v["nonce"] == journal.nonce && v["restored"] == true))
    })()
    .unwrap_or(false)
}

fn write_record(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(
        bytes.len() as u64 <= RECEIPT_LIMIT,
        "起動receiptが大きすぎる"
    );
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure!(
                fs::symlink_metadata(path)?.is_file() && fs::read(path)? == bytes,
                "別の起動receiptがある"
            );
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    file.write_all(&bytes)?;
    file.sync_all()?;
    File::open(path.parent().context("receiptの親がない")?)?.sync_all()?;
    Ok(())
}

fn read_record<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        metadata.is_file() && metadata.len() <= RECEIPT_LIMIT,
        "起動receiptの形式が不正"
    );
    // 書込側がsyncを完了する前の短い観測は次のpollへ回す。期限は監督側が持つ。
    Ok(serde_json::from_slice(&fs::read(path)?).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updater_flags_cannot_choose_an_arbitrary_installation() {
        for path in [
            "/tmp/kb-app",
            "/tmp/other.app/Contents/MacOS/kb-app",
            "/tmp/kb-app.app/Contents/MacOS/other",
        ] {
            assert!(bundle_for_executable(Path::new(path)).is_err());
        }
        assert_eq!(
            bundle_for_executable(Path::new("/tmp/kb-app.app/Contents/MacOS/kb-app")).unwrap(),
            Path::new("/tmp/kb-app.app")
        );
    }

    #[test]
    fn receipt_is_bounded_idempotent_and_never_overwrites_other_data() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("startup.json");
        let receipt = serde_json::json!({"nonce":"a", "identity":"b"});
        assert!(read_record::<serde_json::Value>(&path).unwrap().is_none());
        write_record(&path, &receipt).unwrap();
        write_record(&path, &receipt).unwrap();
        assert!(write_record(&path, &serde_json::json!({"nonce":"changed"})).is_err());
        assert_eq!(
            read_record::<serde_json::Value>(&path).unwrap(),
            Some(receipt)
        );
        fs::write(&path, vec![0_u8; RECEIPT_LIMIT as usize + 1]).unwrap();
        assert!(read_record::<serde_json::Value>(&path).is_err());
    }

    #[cfg(unix)]
    struct OwnedChild(Child);

    #[cfg(unix)]
    impl OwnedChild {
        fn start(script: &str) -> Result<Self> {
            Ok(Self(
                Command::new("/bin/sh")
                    .args(["-c", script])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()?,
            ))
        }
    }

    #[cfg(unix)]
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            // テスト自身のchildだけを終了し、assert失敗時もprocessを残さない。
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[cfg(unix)]
    fn acceptance_fixture() -> (transaction::Journal, StartupReceipt) {
        let identity = |byte: &str| BundleIdentity {
            executable_sha256: byte.repeat(64),
            plan_sha256: byte.repeat(64),
            inventory_sha256: byte.repeat(64),
        };
        let journal = transaction::Journal {
            format_version: 1,
            nonce: "a".repeat(64),
            phase: Phase::NewPlaced,
            intent: None,
            source: identity("0"),
            new: identity("1"),
            compatibility: kb_core::app_update_compatibility::compiled_metadata(),
        };
        let receipt = StartupReceipt {
            nonce: journal.nonce.clone(),
            identity: journal.new.clone(),
        };
        (journal, receipt)
    }

    /// 2026-09-09: bootstrapだけで受入せず、UI receiptと生存安定期間も要求する。
    #[cfg(unix)]
    #[test]
    fn acceptance_requires_matching_receipts_and_a_live_stable_period() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let (journal, receipt) = acceptance_fixture();
        let mut child = OwnedChild::start("exec /bin/sleep 30")?;
        write_record(&temporary.path().join("bootstrap.ok"), &receipt)?;
        write_record(&temporary.path().join("startup.json"), &receipt)?;
        let stable = Duration::from_millis(20);
        let start = Instant::now();
        let accepted = wait_for_acceptance_with_timing(
            &mut child.0,
            temporary.path(),
            &journal,
            Duration::from_secs(2),
            stable,
            Duration::from_millis(1),
        )?;
        assert!(start.elapsed() >= stable);
        assert!(child.0.try_wait()?.is_none());
        assert_eq!(accepted.nonce, receipt.nonce);
        assert_eq!(accepted.identity, receipt.identity);
        Ok(())
    }

    /// 2026-09-09: 正しいreceiptが残っていても終了済みchildを起動成功にしない。
    #[cfg(unix)]
    #[test]
    fn acceptance_rejects_an_exited_child_even_with_matching_receipts() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let (journal, receipt) = acceptance_fixture();
        let mut child = OwnedChild::start("exit 0")?;
        child.0.wait()?;
        write_record(&temporary.path().join("bootstrap.ok"), &receipt)?;
        write_record(&temporary.path().join("startup.json"), &receipt)?;
        let error = wait_for_acceptance_with_timing(
            &mut child.0,
            temporary.path(),
            &journal,
            Duration::from_millis(30),
            Duration::from_millis(5),
            Duration::from_millis(1),
        )
        .unwrap_err();
        assert!(error.to_string().contains("終了した"));
        Ok(())
    }

    /// 2026-09-09: receipt後すぐ落ちた新版を、安定期間の経過だけで受入しない。
    #[cfg(unix)]
    #[test]
    fn acceptance_rejects_a_child_that_exits_during_the_stable_period() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let (journal, receipt) = acceptance_fixture();
        let mut child = OwnedChild::start("exec /bin/sleep 0.01")?;
        write_record(&temporary.path().join("bootstrap.ok"), &receipt)?;
        write_record(&temporary.path().join("startup.json"), &receipt)?;
        let error = wait_for_acceptance_with_timing(
            &mut child.0,
            temporary.path(),
            &journal,
            Duration::from_secs(2),
            Duration::from_millis(100),
            Duration::from_millis(1),
        )
        .unwrap_err();
        assert!(error.to_string().contains("終了した"));
        Ok(())
    }

    /// 2026-09-09: bootstrap/UIそれぞれの欠落・別nonce・別実物・途中JSONは期限切れになる。
    #[cfg(unix)]
    #[test]
    fn incomplete_or_mismatching_receipts_never_accept_a_live_child() -> Result<()> {
        let mut child = OwnedChild::start("exec /bin/sleep 30")?;
        for name in ["bootstrap.ok", "startup.json"] {
            for defect in ["missing", "nonce", "identity", "partial"] {
                let temporary = tempfile::tempdir()?;
                let (journal, receipt) = acceptance_fixture();
                for entry in ["bootstrap.ok", "startup.json"] {
                    let path = temporary.path().join(entry);
                    if entry != name {
                        write_record(&path, &receipt)?;
                        continue;
                    }
                    match defect {
                        "missing" => {}
                        "nonce" => {
                            let mut changed = receipt.clone();
                            changed.nonce = "b".repeat(64);
                            write_record(&path, &changed)?;
                        }
                        "identity" => {
                            let mut changed = receipt.clone();
                            changed.identity.inventory_sha256 = "c".repeat(64);
                            write_record(&path, &changed)?;
                        }
                        "partial" => fs::write(path, b"{\"nonce\":")?,
                        _ => unreachable!(),
                    }
                }
                let error = wait_for_acceptance_with_timing(
                    &mut child.0,
                    temporary.path(),
                    &journal,
                    Duration::from_millis(20),
                    Duration::from_millis(5),
                    Duration::from_millis(1),
                )
                .unwrap_err();
                assert!(
                    error.to_string().contains("期限内"),
                    "{name}/{defect}: {error}"
                );
                assert!(child.0.try_wait()?.is_none());
            }
        }
        Ok(())
    }
}
