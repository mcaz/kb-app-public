//! 「繋ぐ」(FR-A1 の代行+FR-A6 の最小)。
//! - Claude Desktop 接続: claude_desktop_config.json への追記をアプリが代行
//!   (バックアップ作成・既存サーバー不侵害。M1 で手作業だった手順の機構化)
//! - バックアップ: 紐付け済み remote(origin 固定)への明示 push と滞留表示。
//!   宛先は origin のみ(FR-A6: push 先固定)。GitHub は各 upload の直前に private +
//!   push 権限を認証済み API で再確認する(契約7)

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::backup::{BackupFailureKind, failure, failure_kind, git_failure};
use crate::frontmatter::today;
use crate::vault::Vault;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum DesktopStatus {
    /// Claude Desktop の設定ファイルが見つからない(未インストールか未起動)
    NotFound,
    NotConnected,
    Connected,
}

pub fn claude_desktop_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("Claude").join("claude_desktop_config.json"))
}

pub fn desktop_status_at(config: &Path) -> DesktopStatus {
    let Ok(text) = fs::read_to_string(config) else {
        return DesktopStatus::NotFound;
    };
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(v) if v.get("mcpServers").and_then(|s| s.get("kb-app")).is_some() => {
            DesktopStatus::Connected
        }
        Ok(_) => DesktopStatus::NotConnected,
        Err(_) => DesktopStatus::NotConnected,
    }
}

/// kb-app サーバーを Desktop 設定へ追記。既存キーは触らない。バックアップを残す。
/// `exe` は MCP を起動する実行ファイル(アプリ自身+`--mcp`)。
pub fn connect_desktop_at(config: &Path, exe: &Path, vault_name: &str) -> Result<()> {
    let text = fs::read_to_string(config)
        .with_context(|| format!("Claude Desktop の設定が見つからない: {}", config.display()))?;
    let mut v: serde_json::Value = serde_json::from_str(&text).context("設定の parse")?;
    let servers = v
        .as_object_mut()
        .context("設定がオブジェクトでない")?
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    let servers = servers
        .as_object_mut()
        .context("mcpServers がオブジェクトでない")?;
    let backup = config.with_file_name(format!("claude_desktop_config.json.bak-kbapp-{}", today()));
    fs::copy(config, &backup).context("バックアップ作成")?;
    servers.insert(
        "kb-app".to_string(),
        serde_json::json!({
            "command": exe.to_string_lossy(),
            "args": ["--mcp", "--vault", vault_name, "--client", "claude-desktop/claude"],
        }),
    );
    fs::write(config, serde_json::to_string_pretty(&v)?).context("設定の書き込み")?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct BackupStatus {
    /// origin の URL(未設定なら None = 未接続)
    pub remote: Option<String>,
    /// 未バックアップのコミット数(remote 未設定時は全コミット数)
    pub pending: usize,
}

pub fn backup_status(vault: &Vault) -> Result<BackupStatus> {
    let repo = git2::Repository::open(&vault.root)?;
    let remote = repo
        .find_remote("origin")
        .ok()
        .and_then(|r| r.url().map(String::from));
    let head = match repo.head().ok().and_then(|h| h.peel_to_commit().ok()) {
        Some(c) => c,
        None => return Ok(BackupStatus { remote, pending: 0 }),
    };
    let tracking = repo
        .branch_upstream_name(repo.head()?.name().unwrap_or("refs/heads/main"))
        .ok()
        .and_then(|b| b.as_str().map(String::from))
        .and_then(|name| repo.find_reference(&name).ok())
        .and_then(|r| r.peel_to_commit().ok());
    let pending = match tracking {
        Some(t) => repo.graph_ahead_behind(head.id(), t.id())?.0,
        None => {
            let mut walk = repo.revwalk()?;
            walk.push(head.id())?;
            walk.count()
        }
    };
    Ok(BackupStatus { remote, pending })
}

/// 同期(FR-A6 改定 2026-08-10): 随時 push・メッセージ時 pull。
/// GitHub の remote 操作には OAuth token を process 環境だけで渡す。
/// git は非対話モード強制(資格情報プロンプトで GUI/MCP をハングさせない)。
fn git(vault: &Vault, args: &[&str]) -> Result<std::process::Output> {
    let mut command = std::process::Command::new("git");
    command
        .args(args)
        .current_dir(&vault.root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes")
        .env("GCM_INTERACTIVE", "Never");
    let accesses_remote = args
        .iter()
        .any(|arg| matches!(*arg, "push" | "pull" | "fetch"))
        || args.starts_with(&["remote", "set-head"]);
    if accesses_remote && let Ok(url) = origin_url(vault) {
        crate::github_auth::configure_git_auth(&mut command, &url)?;
    }
    command.output().context("git 実行")
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).trim().to_string()
}

/// 生成ファイルを競合させない設定(複数デバイス同期の前提)。
/// index.md は派生物 — 競合したら相手側を取り、pull 後に再生成で自己修復。
/// log.md は追記ログ — union merge で両側の行を残す。
/// .kb-workspace は保管庫の ID — 2台が同時に初回起動すると別々に発行されるので、
/// union で両方を残し、読むときに古い方へ寄せる(crate::workspace)。
/// merge driver の設定はリポジトリローカルなので、clone した側でも毎回冪等に張り直す。
/// 保管庫の設定を一括で張り直す(merge 属性 + LFS)。
///
/// **実体を書く前に必ずこれを通すこと。** `.gitattributes` の LFS 行が無いまま
/// `git add` すると、実体がそのまま Git に入る(決定が却下した形)。
/// 冪等なので何度呼んでもよい。
pub fn ensure_vault_config(vault: &Vault) -> Result<()> {
    ensure_merge_config(vault)
}

fn ensure_merge_config(vault: &Vault) -> Result<()> {
    let lfs = lfs_available();
    let attrs = vault.root.join(".gitattributes");
    let current = fs::read_to_string(&attrs).unwrap_or_default();
    let want = merged_attributes(&current, lfs);
    if current != want {
        fs::write(&attrs, &want)?;
        vault.commit(&[".gitattributes"], "vault: 同期用の merge 属性")?;
    }
    let repo = git2::Repository::open(&vault.root)?;
    repo.config()?.set_str("merge.ours.driver", "true")?;
    if lfs {
        let _ = ensure_lfs_config(vault);
    }
    Ok(())
}

/// `.gitattributes` の中身。**ここが唯一の書き手**
/// (2箇所から書くと、LFS の有無で毎回互いに上書きし合う)。
///
/// eol=lf は Windows(autocrlf)混在でも差分が全行化しないため。
/// LFS の行は git-lfs がある時だけ足す — フィルタが無い環境でこの属性を張ると、
/// 実体がそのまま Git に入ってしまう(決定が却下した形)。
/// アプリが存在を保証する行。**これ以外は知らない行として残す。**
fn required_attributes(lfs: bool) -> Vec<String> {
    let mut want = vec![
        "* text=auto eol=lf".to_string(),
        "index.md merge=ours".to_string(),
        "log.md merge=union".to_string(),
        ".kb-workspace merge=union".to_string(),
    ];
    if lfs {
        want.push(format!(
            "{}/lfs/** filter=lfs diff=lfs merge=lfs -text",
            crate::ledger::DIR
        ));
    }
    want
}

/// 必要な行の存在**だけ**を保証し、知らない行はそのまま残す。
///
/// 2026-08-14 の事故: 以前は全文を作り直して上書きしていた。これは
/// 「自分がこのファイルの唯一の書き手」という仮定に立っていて、**古い版のバイナリが
/// 同じ保管庫を触ると崩れる**。実際、MCP が実行していた2日前のビルドが、
/// 自分の知らない LFS の追跡行を消してコミットし続けていた(手元は無症状で、
/// 別の端末で clone したとき初めて「取り寄せ」が空振りする形で出た)。
///
/// 足すだけにしておけば、版が違っても互いの設定を消さない。
fn merged_attributes(current: &str, lfs: bool) -> String {
    let mut lines: Vec<String> = current
        .lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    for want in required_attributes(lfs) {
        if !lines.iter().any(|l| l.trim() == want) {
            lines.push(want);
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn lfs_available() -> bool {
    std::process::Command::new("git")
        .args(["lfs", "version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// LFS の下ごしらえ(PoC `poc/lfs-transport` の実測を反映)。
///
/// 3つとも**順序と場所が効く**:
///
/// 1. `lfs.storage` は**最初の実体を作る前**に張る。後から張っても既にある実体は
///    移らないので、保管庫の中に取り残される
/// 2. 素の clone には LFS のフィルタが無い。取得を担うなら `install --local` が要る
/// 3. `.lfsconfig` の `fetchexclude` は**追跡ファイル**にする。これが他の端末へ
///    運ばれることで、手で clone しても実体が落ちてこない
///
/// push / pull のたびに呼ばれる(clone した側でも冪等に張り直すため)。
/// 失敗しても同期自体は続ける — 同期は派生(契約4)。
pub fn ensure_lfs_config(vault: &Vault) -> Result<()> {
    let workspace_id = crate::workspace::workspace_id(vault)?;
    let storage = crate::app_data_dir()?
        .join("artifacts")
        .join(&workspace_id)
        .join("full-lfs");
    fs::create_dir_all(&storage)?;

    // フィルタを張る(冪等)
    let out = git(vault, &["lfs", "install", "--local"])?;
    if !out.status.success() {
        bail!("git lfs install: {}", stderr_of(&out));
    }

    // 置き場を保管庫の外へ。実体を作る前でなければ効かない
    let repo = git2::Repository::open(&vault.root)?;
    let want_storage = storage.to_string_lossy().to_string();
    let current = repo.config()?.get_string("lfs.storage").unwrap_or_default();
    if current != want_storage {
        repo.config()?.set_str("lfs.storage", &want_storage)?;
    }

    // 既定では実体を取らない。**追跡ファイル**にして他の端末へも運ぶ
    let cfg = vault.root.join(".lfsconfig");
    let want_cfg = "[lfs]\n\tfetchexclude = *\n";
    if fs::read_to_string(&cfg).unwrap_or_default() != want_cfg {
        fs::write(&cfg, want_cfg)?;
        vault.commit(&[".lfsconfig"], "vault: 既定では実体を取らない")?;
    }
    Ok(())
}

enum RemoteContents {
    Empty,
    Vault { workspace_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum RestorePhase {
    Checking,
    Cloning,
    RestoringFiles,
    Finalizing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RestoreProgress {
    pub phase: RestorePhase,
    pub completed: usize,
    pub total: usize,
    pub fetched: usize,
    pub reused: usize,
}

impl RestoreProgress {
    fn at(phase: RestorePhase) -> Self {
        Self {
            phase,
            completed: 0,
            total: 0,
            fetched: 0,
            reused: 0,
        }
    }
}

fn is_local_test_remote(url: &str) -> bool {
    cfg!(test) && (url.starts_with("file://") || Path::new(url).is_absolute())
}

fn origin_url(vault: &Vault) -> Result<String> {
    let repo = git2::Repository::open(&vault.root)?;
    repo.find_remote("origin")?
        .url()
        .map(String::from)
        .context("origin の URL が無い")
}

/// upload の直前に必ず通す。ローカル remote は unit test だけの経路。
fn ensure_origin_upload_allowed(vault: &Vault) -> Result<()> {
    let url = origin_url(vault)?;
    if is_local_test_remote(&url) {
        return Ok(());
    }
    crate::github::verify_private_repository(&url)?;
    Ok(())
}

/// remote を一時 clone して、現在の Vault を変更せずに正本と workspace ID を調べる。
fn inspect_remote(url: &str) -> Result<RemoteContents> {
    let temp = tempfile::tempdir().context("既存 Vault の検査場所を作れない")?;
    let clone_root = temp.path().join("vault");
    let mut command = std::process::Command::new("git");
    command
        .args([
            "clone",
            "--no-tags",
            "--",
            url,
            clone_root.to_str().unwrap_or_default(),
        ])
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes")
        .env("GCM_INTERACTIVE", "Never");
    crate::github_auth::configure_git_auth(&mut command, url)?;
    let out = command
        .output()
        .context("既存 Vault の検査用 clone を開始できない")?;
    if !out.status.success() {
        return Err(git_failure(
            "既存 Vault を確認できない",
            &stderr_of(&out),
            BackupFailureKind::GitPull,
        ));
    }
    let repo = git2::Repository::open(&clone_root)?;
    if repo.head().is_err() {
        return Ok(RemoteContents::Empty);
    }
    let candidate = Vault::open(&clone_root)?;
    if let Err(error) = crate::storage_contract::verify(&candidate) {
        return Err(failure(
            BackupFailureKind::InvalidVault,
            format!("接続先は kb-app の Storage Contract を満たさない: {error}"),
        ));
    }
    Ok(RemoteContents::Vault {
        workspace_id: crate::workspace::stored_workspace_id(&candidate).map_err(|error| {
            failure(
                BackupFailureKind::InvalidVault,
                format!("接続先の workspace ID を確認できない: {error}"),
            )
        })?,
    })
}

/// 新しい端末で既存 Vault に参加する。検査と Full 復元が全部終わるまで `destination` は作らない。
/// 既存 path への overlay / overwrite は行わない。
pub fn clone_existing_vault(url: &str, destination: &Path) -> Result<crate::lfs::RestoreReport> {
    clone_existing_vault_with_progress(url, destination, |_| {})
}

/// [`clone_existing_vault`] の進捗通知付き経路。失敗時に一時cloneは消すが、hash照合済みの
/// LFS objectはworkspace ID単位の端末storeへ残るため、同じURLの再実行で再利用する。
pub fn clone_existing_vault_with_progress(
    url: &str,
    destination: &Path,
    mut progress: impl FnMut(RestoreProgress),
) -> Result<crate::lfs::RestoreReport> {
    progress(RestoreProgress::at(RestorePhase::Checking));
    let url = url.trim();
    let is_github = url.starts_with("git@github.com:") || url.starts_with("https://github.com/");
    if !is_github && !is_local_test_remote(url) {
        return Err(failure(
            BackupFailureKind::InvalidRepository,
            "既存 Vault は GitHub repository の URL で指定する",
        ));
    }
    if destination.exists() {
        return Err(failure(
            BackupFailureKind::DestinationExists,
            format!(
                "復元先が既に存在するため上書きしない: {}",
                destination.display()
            ),
        ));
    }
    let clone_url = if is_github {
        crate::github::verify_private_repository(url)?.clone_url
    } else {
        url.to_string()
    };
    let parent = destination.parent().context("復元先の親が無い")?;
    fs::create_dir_all(parent)?;
    let temp = tempfile::Builder::new()
        .prefix(".kb-restore-")
        .tempdir_in(parent)
        .context("既存 Vault の一時復元先を作れない")?;
    let clone_root = temp.path().join("vault");
    progress(RestoreProgress::at(RestorePhase::Cloning));
    let mut command = std::process::Command::new("git");
    command
        .arg("clone")
        .arg("--no-tags")
        .arg("--")
        .arg(&clone_url)
        .arg(&clone_root)
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes")
        .env("GCM_INTERACTIVE", "Never");
    crate::github_auth::configure_git_auth(&mut command, &clone_url)?;
    let out = command
        .output()
        .context("既存 Vault の clone を開始できない")?;
    if !out.status.success() {
        return Err(git_failure(
            "既存 Vault を clone できない",
            &stderr_of(&out),
            BackupFailureKind::GitPull,
        ));
    }
    let repo = git2::Repository::open(&clone_root)?;
    if repo.head().is_err() {
        return Err(failure(
            BackupFailureKind::InvalidVault,
            "選んだ repository は空で、復元できる Vault が無い",
        ));
    }
    let restored = Vault::open(&clone_root)?;
    if let Err(error) = crate::storage_contract::verify(&restored) {
        return Err(failure(
            BackupFailureKind::InvalidVault,
            format!("接続先は kb-app の Storage Contract を満たさない: {error}"),
        ));
    }
    let report = crate::lfs::restore_all_with_progress(&restored, |state| {
        progress(RestoreProgress {
            phase: RestorePhase::RestoringFiles,
            completed: state.completed,
            total: state.total,
            fetched: state.fetched,
            reused: state.reused,
        });
    })?;
    progress(RestoreProgress {
        phase: RestorePhase::Finalizing,
        completed: report.total,
        total: report.total,
        fetched: report.fetched,
        reused: report.reused,
    });
    drop(restored);
    fs::rename(&clone_root, destination).with_context(|| {
        format!(
            "検査済み Vault を復元先へ移せない: {}",
            destination.display()
        )
    })?;
    Ok(report)
}

/// 既存 remote へ参加するとき、初回 pull より先に upstream を張る。
/// 通常の clone には既にあるが、既存ローカル Vault へ origin を後付けする経路には無い。
fn configure_existing_upstream(vault: &Vault) -> Result<()> {
    let fetch = git(vault, &["fetch", "origin"])?;
    if !fetch.status.success() {
        return Err(git_failure(
            "既存 Vault の履歴を取得できない",
            &stderr_of(&fetch),
            BackupFailureKind::GitPull,
        ));
    }
    let _ = git(vault, &["remote", "set-head", "origin", "--auto"]);
    let repo = git2::Repository::open(&vault.root)?;
    let local = repo
        .head()?
        .shorthand()
        .map(String::from)
        .context("現在の branch 名を確認できない")?;
    let same = format!("refs/remotes/origin/{local}");
    let upstream = if repo.find_reference(&same).is_ok() {
        format!("origin/{local}")
    } else {
        repo.find_reference("refs/remotes/origin/HEAD")?
            .symbolic_target()
            .and_then(|target| target.strip_prefix("refs/remotes/"))
            .map(String::from)
            .context("既存 Vault の既定 branch を確認できない")?
    };
    let out = git(vault, &["branch", "--set-upstream-to", &upstream, &local])?;
    if !out.status.success() {
        return Err(git_failure(
            "既存 Vault の追跡設定に失敗",
            &stderr_of(&out),
            BackupFailureKind::GitPull,
        ));
    }
    Ok(())
}

/// バックアップ先の設定(origin 固定)。
///
/// GitHub は private + push 権限を確認してから remote の内容を一時 clone する。空なら
/// 「新規作成済みの保管場所」、既存 Vault なら `.kb-workspace` が一致する場合だけ接続する。
/// 別 ID や壊れた repository へ現在の Vault を push しない。
pub fn set_backup_remote(vault: &Vault, url: &str) -> Result<()> {
    let url = url.trim();
    let is_github = url.starts_with("git@github.com:") || url.starts_with("https://github.com/");
    let is_local = is_local_test_remote(url);
    if !is_github && !is_local {
        return Err(failure(
            BackupFailureKind::InvalidRepository,
            "バックアップ先は GitHub repository の URL を指定する",
        ));
    }
    let remote_url = if is_github {
        crate::github::verify_private_repository(url)?.clone_url
    } else {
        url.to_string()
    };
    let remote = inspect_remote(&remote_url)?;
    let local_workspace_id = crate::workspace::stored_workspace_id(vault).map_err(|error| {
        failure(
            BackupFailureKind::InvalidVault,
            format!("現在の Vault の workspace ID を確認できない: {error}"),
        )
    })?;
    if let RemoteContents::Vault {
        workspace_id: remote_workspace_id,
    } = &remote
        && remote_workspace_id != &local_workspace_id
    {
        return Err(failure(
            BackupFailureKind::WorkspaceMismatch,
            format!(
                "別の Vault なので接続しない(local workspace {local_workspace_id}, remote workspace {remote_workspace_id})"
            ),
        ));
    }
    ensure_merge_config(vault)?;
    let repo = git2::Repository::open(&vault.root)?;
    match repo.find_remote("origin") {
        Ok(_) => repo.remote_set_url("origin", &remote_url)?,
        Err(_) => {
            repo.remote("origin", &remote_url)?;
        }
    }
    match remote {
        RemoteContents::Empty => push_now(vault),
        RemoteContents::Vault { .. } => {
            configure_existing_upstream(vault)?;
            pull_now(vault)?;
            push_now(vault)?;
            crate::lfs::restore_all(vault)?;
            Ok(())
        }
    }
}

/// いま push(随時 push の実体)。非 fast-forward なら pull --rebase して1回だけ再試行。
pub fn push_now(vault: &Vault) -> Result<()> {
    let _lock = sync_lock(vault)?;
    push_now_locked(vault)
}

/// 呼び出し側が `sync_lock` を保持しているときの push 本体。
fn push_now_locked(vault: &Vault) -> Result<()> {
    if let Err(error) = ensure_origin_upload_allowed(vault) {
        let message = error.to_string();
        record_sync(
            vault,
            Some(&message),
            Some(failure_kind(&error).unwrap_or(BackupFailureKind::PrivacyCheck)),
        );
        return Err(error);
    }
    let _ = ensure_merge_config(vault);
    let out = git(vault, &["push", "-u", "origin", "HEAD"])?;
    if out.status.success() {
        record_sync(vault, None, None);
        return Ok(());
    }
    let first_error = git_failure("push 失敗", &stderr_of(&out), BackupFailureKind::GitPush);
    if failure_kind(&first_error) != Some(BackupFailureKind::GitConflict) {
        record_sync(
            vault,
            Some(&first_error.to_string()),
            failure_kind(&first_error),
        );
        return Err(first_error);
    }
    let pull = git(vault, &["pull", "--rebase", "--autostash"])?;
    if pull.status.success() {
        let retry = git(vault, &["push", "-u", "origin", "HEAD"])?;
        if retry.status.success() {
            record_sync(vault, None, None);
            return Ok(());
        }
        let error = git_failure("push 失敗", &stderr_of(&retry), BackupFailureKind::GitPush);
        record_sync(vault, Some(&error.to_string()), failure_kind(&error));
        return Err(error);
    }
    let error = git_failure(
        "push 失敗(pull --rebase も失敗)",
        &stderr_of(&pull),
        BackupFailureKind::GitConflict,
    );
    record_sync(vault, Some(&error.to_string()), failure_kind(&error));
    Err(error)
}

/// 同期操作(pull/push)のプロセス間ロック。GUI・MCP・CLI が同時に git を叩くと
/// FETCH_HEAD の競合で「Cannot rebase onto multiple branches」等の一過性エラーになる
/// (実機で観測)。flock で直列化する — git 自体は取らない advisory lock なので、
/// この3者(自アプリ群)の間でだけ効けばよい。
pub(crate) fn sync_lock(vault: &Vault) -> Result<fs::File> {
    use fs4::fs_std::FileExt;
    let dir = vault.root.join(".kb");
    fs::create_dir_all(&dir)?;
    let f = fs::File::create(dir.join("sync.lock"))?;
    f.lock_exclusive().context("同期ロック取得")?;
    Ok(f)
}

fn has_origin(vault: &Vault) -> bool {
    match git2::Repository::open(&vault.root) {
        Ok(repo) => repo.find_remote("origin").is_ok(),
        Err(_) => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullDelivery {
    RemoteNotConfigured,
    Confirmed,
}

/// `full` Artifact の実体と台帳を origin へ到達させる。
///
/// 実体を先に LFS へ upload し、その成功後に pointer・manifest・ref を含む Git ref を
/// push する。remote 未設定はローカル利用の正常状態なのでエラーにせず区別して返す。
pub fn deliver_full(vault: &Vault, hash: &crate::artifact::ContentHash) -> Result<FullDelivery> {
    let _lock = sync_lock(vault)?;
    deliver_full_locked(vault, hash)
}

/// 取り込み全体が `sync_lock` を保持しているときの Full 配送。
/// manifest commit と LFS upload の間へ別プロセスの push が割り込むことを防ぐ。
pub(crate) fn deliver_full_locked(
    vault: &Vault,
    hash: &crate::artifact::ContentHash,
) -> Result<FullDelivery> {
    if !has_origin(vault) {
        return Ok(FullDelivery::RemoteNotConfigured);
    }
    if let Err(error) = ensure_origin_upload_allowed(vault) {
        let message = error.to_string();
        record_sync(
            vault,
            Some(&message),
            Some(failure_kind(&error).unwrap_or(BackupFailureKind::PrivacyCheck)),
        );
        return Err(error);
    }
    if let Err(error) = crate::lfs::push_object(vault, hash) {
        let message = error.to_string();
        record_sync(
            vault,
            Some(&message),
            Some(failure_kind(&error).unwrap_or(BackupFailureKind::LfsUpload)),
        );
        return Err(error);
    }
    push_now_locked(vault)?;
    Ok(FullDelivery::Confirmed)
}

/// Git commit まで届かなかった同期対象の書き込みを、画面で見える劣化状態へ残す。
pub fn record_sync_degradation(vault: &Vault, message: &str) {
    record_sync(vault, Some(message), Some(BackupFailureKind::Commit));
}

/// ノート操作の後に呼ぶ随時 push。remote 未設定なら何もしない。
/// **失敗してもノート操作は成功のまま**(fail-open)— 失敗は sync 状態に記録され画面に出る。
pub fn auto_push(vault: &Vault) {
    if has_origin(vault) {
        let _ = push_now(vault);
    }
}

const PULL_THROTTLE_SECS: u64 = 60;

fn sync_state_path(vault: &Vault) -> PathBuf {
    vault.root.join(".kb").join("sync.json")
}

#[derive(Debug, Default, Clone, Serialize, serde::Deserialize)]
pub struct SyncState {
    pub last_pull_epoch: u64,
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_error_kind: Option<BackupFailureKind>,
}

pub fn sync_state(vault: &Vault) -> SyncState {
    fs::read_to_string(sync_state_path(vault))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn record_sync(vault: &Vault, error: Option<&str>, kind: Option<BackupFailureKind>) {
    let mut st = sync_state(vault);
    st.last_error = error.map(String::from);
    st.last_error_kind = kind;
    if error.is_none() {
        st.last_pull_epoch = epoch_now();
    }
    let path = sync_state_path(vault);
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(path, serde_json::to_string(&st).unwrap_or_default());
}

fn epoch_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// メッセージのやり取り・画面更新の際の pull(複数デバイス同期)。
/// 時間スロットリング付き — 全呼び出しで同期待ちしない(旧 KB のレイテンシ教訓)。
/// 戻り値: 劣化情報(None = 正常またはスキップ)。
pub fn pull_if_stale(vault: &Vault) -> Option<String> {
    if !has_origin(vault) {
        return None;
    }
    if epoch_now().saturating_sub(sync_state(vault).last_pull_epoch) < PULL_THROTTLE_SECS {
        return sync_state(vault)
            .last_error
            .map(|e| format!("同期エラー(前回): {e}"));
    }
    pull_now(vault).err().map(|e| e.to_string())
}

/// いま pull(スロットリング無視)。成功後は index.md を再生成して自己修復
/// (merge=ours で相手側が勝った場合や、他デバイス追加分の反映)。
pub fn pull_now(vault: &Vault) -> Result<()> {
    let _lock = sync_lock(vault)?;
    let _ = ensure_merge_config(vault);
    let out = git(vault, &["pull", "--rebase", "--autostash"])?;
    if out.status.success() {
        record_sync(vault, None, None);
        let _ = vault.write_index_md();
        Ok(())
    } else {
        let error = git_failure("pull 失敗", &stderr_of(&out), BackupFailureKind::GitPull);
        record_sync(vault, Some(&error.to_string()), failure_kind(&error));
        Err(error)
    }
}

/// 明示同期(pull → push)。「今すぐバックアップ」ボタンの実体。
pub fn backup_push(vault: &Vault) -> Result<String> {
    let status = backup_status(vault)?;
    if status.remote.is_none() {
        return Err(failure(
            BackupFailureKind::InvalidRepository,
            "バックアップ先が未設定(繋ぐ画面で GitHub リポジトリの URL を設定)",
        ));
    }
    pull_now(vault)?;
    push_now(vault)?;
    Ok(format!("同期完了({} 件を送信)", status.pending))
}

/// FR-A5 最小: 「いま見ているノート」をコアの状態として記録(将来 MCP 側から参照)。
pub fn set_current_note(vault: &Vault, id: &str) -> Result<()> {
    let dir = vault.root.join(".kb");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("current-note"), id)?;
    Ok(())
}

pub fn current_note(vault: &Vault) -> Option<String> {
    fs::read_to_string(vault.root.join(".kb").join("current-note"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lfs_attribute_is_added_only_when_git_lfs_exists() {
        // フィルタの無い環境でこの属性を張ると、実体がそのまま Git に入る
        // (決定が却下した形)。だから git-lfs がある時だけ足す
        let without = merged_attributes("", false);
        assert!(!without.contains("filter=lfs"));
        assert!(without.contains("index.md merge=ours"));
        assert!(without.contains(".kb-workspace merge=union"));

        let with = merged_attributes("", true);
        assert!(with.contains(".kb-artifacts/lfs/** filter=lfs diff=lfs merge=lfs -text"));
        assert!(with.contains("index.md merge=ours"));
        assert!(with.contains("log.md merge=union"));
        assert!(with.contains(".kb-workspace merge=union"));
    }

    /// 2026-08-14 の事故の再現。MCP が実行していた2日前のビルドが、自分の知らない
    /// LFS の追跡行を消して `.gitattributes` を上書きし続けていた。手元では無症状で、
    /// 別の端末で clone したとき「取り寄せ」が空振りする形で初めて出た。
    /// **知らない行は消さない**ことをここで固定する。
    #[test]
    fn an_older_build_must_not_strip_what_it_does_not_know() {
        // 新しい版が張った状態
        let newer = merged_attributes("", true);
        assert!(newer.contains("filter=lfs"));

        // 古い版(LFS を知らない)が同じファイルを触っても、その行は残る
        let after_old = merged_attributes(&newer, false);
        assert!(
            after_old.contains(".kb-artifacts/lfs/** filter=lfs diff=lfs merge=lfs -text"),
            "古い版が新しい版の設定を消した: {after_old}"
        );

        // ユーザーが手で足した行も残す(アプリはこのファイルの唯一の書き手ではない)
        let hand_written = format!("{newer}*.psd binary\n");
        let after = merged_attributes(&hand_written, true);
        assert!(
            after.contains("*.psd binary"),
            "手書きの行が消えた: {after}"
        );
    }

    #[test]
    fn merging_the_same_content_changes_nothing() {
        let once = merged_attributes("", true);
        assert_eq!(
            merged_attributes(&once, true),
            once,
            "冪等でないと毎回コミットが増える"
        );
        // 空行や末尾の空白が混じっても増殖しない
        let messy = once.replace('\n', "  \n") + "\n\n";
        assert_eq!(merged_attributes(&messy, true), once);
    }

    #[test]
    fn lfs_setup_is_idempotent_and_points_outside_the_vault() {
        if !lfs_available() {
            eprintln!("git-lfs が無いので飛ばす");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();

        ensure_lfs_config(&vault).unwrap();
        let repo = git2::Repository::open(&vault.root).unwrap();
        let storage = repo.config().unwrap().get_string("lfs.storage").unwrap();

        // 保管庫の外を指していること。中だと clone・履歴が肥大する
        assert!(
            !std::path::Path::new(&storage).starts_with(&vault.root),
            "置き場が保管庫の中にある: {storage}"
        );
        // 実体を取らない設定が**追跡ファイル**として置かれる
        // (これが他の端末へ運ばれるから、手で clone しても実体が落ちてこない)
        let cfg = vault.root.join(".lfsconfig");
        assert!(fs::read_to_string(&cfg).unwrap().contains("fetchexclude"));
        let ignore = fs::read_to_string(vault.root.join(".gitignore")).unwrap();
        assert!(!ignore.contains(".lfsconfig"));

        // 2回目は何も壊さない(push / pull のたびに呼ばれる)
        ensure_lfs_config(&vault).unwrap();
        assert_eq!(
            repo.config().unwrap().get_string("lfs.storage").unwrap(),
            storage
        );
    }

    #[test]
    fn desktop_connect_preserves_existing_servers() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("claude_desktop_config.json");
        fs::write(&cfg, r#"{"mcpServers": {"vault": {"command": "x"}}}"#).unwrap();
        assert_eq!(desktop_status_at(&cfg), DesktopStatus::NotConnected);
        connect_desktop_at(&cfg, Path::new("/usr/bin/true"), "try").unwrap();
        assert_eq!(desktop_status_at(&cfg), DesktopStatus::Connected);
        let v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&cfg).unwrap()).unwrap();
        assert!(
            v["mcpServers"]["vault"].is_object(),
            "既存サーバーが保持される"
        );
        assert_eq!(v["mcpServers"]["kb-app"]["args"][2], "try");
        // バックアップが残る
        assert!(fs::read_dir(dir.path()).unwrap().count() >= 2);
    }

    #[test]
    fn backup_status_without_remote() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        vault.new_human_note("メモ", "本文", "human:o").unwrap();
        let st = backup_status(&vault).unwrap();
        assert!(st.remote.is_none());
        assert!(st.pending >= 2); // initialize + note
    }

    /// 複数デバイス同期の一周: A が書く → 随時 push → B がメッセージ時 pull で受け取る
    #[test]
    fn multi_device_sync_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("backup.git");
        let run = |cwd: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(dir.path(), &["init", "--bare", bare.to_str().unwrap()]);

        // デバイス A: vault 作成 → バックアップ先設定(初回 push)→ ノート追加(随時 push)
        let a = Vault::create(dir.path().join("a")).unwrap();
        set_backup_remote(&a, bare.to_str().unwrap()).unwrap();
        a.new_human_note("同期テスト", "デバイス A で書いた。", "human:o")
            .unwrap();
        assert_eq!(
            backup_status(&a).unwrap().pending,
            0,
            "随時 push 済みなら滞留ゼロ"
        );

        // bare の HEAD を A のブランチ名に合わせる(git2 と system git の
        // 既定ブランチ名差で clone が空チェックアウトになるのを防ぐ)
        let branch = git2::Repository::open(&a.root)
            .unwrap()
            .head()
            .unwrap()
            .shorthand()
            .unwrap()
            .to_string();
        run(
            &bare,
            &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")],
        );

        // デバイス B: clone で復元 → 会話(pull)で A の変化を受け取る
        run(dir.path(), &["clone", bare.to_str().unwrap(), "b"]);
        let b = Vault::open(dir.path().join("b")).unwrap();
        assert_eq!(b.list_note_files().len(), 1);
        a.new_human_note("追加分", "A の2本目。", "human:o")
            .unwrap();
        pull_now(&b).unwrap();
        assert_eq!(
            b.list_note_files().len(),
            2,
            "B が pull で A の追加分を受け取る"
        );

        // B 側で書いても push が通る(非 fast-forward 時の rebase 再試行経路)
        a.new_human_note("三本目", "A の3本目(B の pull 後)。", "human:o")
            .unwrap();
        b.new_human_note("B のメモ", "デバイス B で書いた。", "human:o")
            .unwrap();
        assert_eq!(
            backup_status(&b).unwrap().pending,
            0,
            "rebase 再試行で push が通る"
        );
    }

    /// 既存 repository を URL だけで上書きしない。同じ名前の Vault でも
    /// `.kb-workspace` が違えば別物で、origin を設定する前に止める。
    #[test]
    fn existing_remote_with_a_different_workspace_is_never_attached() {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("backup.git");
        let run = |cwd: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(dir.path(), &["init", "--bare", bare.to_str().unwrap()]);
        let first = Vault::create(dir.path().join("first")).unwrap();
        set_backup_remote(&first, bare.to_str().unwrap()).unwrap();
        let branch = git2::Repository::open(&first.root)
            .unwrap()
            .head()
            .unwrap()
            .shorthand()
            .unwrap()
            .to_string();
        run(
            &bare,
            &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")],
        );

        let other = Vault::create(dir.path().join("other")).unwrap();
        let error = set_backup_remote(&other, bare.to_str().unwrap()).unwrap_err();
        assert!(error.to_string().contains("別の Vault"));
        assert_eq!(
            failure_kind(&error),
            Some(BackupFailureKind::WorkspaceMismatch)
        );
        assert!(
            git2::Repository::open(&other.root)
                .unwrap()
                .find_remote("origin")
                .is_err(),
            "検査に落ちた接続先を origin に残してはいけない"
        );
    }

    #[test]
    fn a_clone_of_the_same_workspace_can_rejoin_the_existing_remote() {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("backup.git");
        let run = |cwd: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_LFS_SKIP_SMUDGE", "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(dir.path(), &["init", "--bare", bare.to_str().unwrap()]);
        let first = Vault::create(dir.path().join("first")).unwrap();
        set_backup_remote(&first, bare.to_str().unwrap()).unwrap();
        let branch = git2::Repository::open(&first.root)
            .unwrap()
            .head()
            .unwrap()
            .shorthand()
            .unwrap()
            .to_string();
        run(
            &bare,
            &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")],
        );
        run(
            dir.path(),
            &["clone", bare.to_str().unwrap(), "same-workspace"],
        );
        let same = Vault::open(dir.path().join("same-workspace")).unwrap();
        git2::Repository::open(&same.root)
            .unwrap()
            .remote_delete("origin")
            .unwrap();

        set_backup_remote(&same, bare.to_str().unwrap()).unwrap();
        assert_eq!(
            crate::workspace::stored_workspace_id(&same).unwrap(),
            crate::workspace::stored_workspace_id(&first).unwrap()
        );
        assert!(backup_status(&same).unwrap().remote.is_some());
    }

    #[test]
    fn onboarding_can_join_an_existing_vault_without_overwriting_a_path() {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("backup.git");
        let run = |cwd: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(dir.path(), &["init", "--bare", bare.to_str().unwrap()]);
        let source = Vault::create(dir.path().join("source")).unwrap();
        source
            .new_human_note("別端末", "既存 Vault から来た。", "human:test")
            .unwrap();
        set_backup_remote(&source, bare.to_str().unwrap()).unwrap();
        let branch = git2::Repository::open(&source.root)
            .unwrap()
            .head()
            .unwrap()
            .shorthand()
            .unwrap()
            .to_string();
        run(
            &bare,
            &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")],
        );

        let destination = dir.path().join("joined");
        let mut progress = Vec::new();
        let report =
            clone_existing_vault_with_progress(bare.to_str().unwrap(), &destination, |state| {
                progress.push(state)
            })
            .unwrap();
        assert_eq!(report.total, 0);
        assert_eq!(progress.first().unwrap().phase, RestorePhase::Checking);
        assert!(
            progress
                .iter()
                .any(|state| state.phase == RestorePhase::Cloning)
        );
        assert!(
            progress
                .iter()
                .any(|state| state.phase == RestorePhase::RestoringFiles)
        );
        assert_eq!(progress.last().unwrap().phase, RestorePhase::Finalizing);
        let joined = Vault::open(&destination).unwrap();
        assert_eq!(
            crate::workspace::stored_workspace_id(&joined).unwrap(),
            crate::workspace::stored_workspace_id(&source).unwrap()
        );
        assert_eq!(joined.list_note_files().len(), 1);

        let occupied = dir.path().join("occupied");
        fs::create_dir(&occupied).unwrap();
        let error = clone_existing_vault(bare.to_str().unwrap(), &occupied).unwrap_err();
        assert!(error.to_string().contains("上書きしない"));
        assert_eq!(
            failure_kind(&error),
            Some(BackupFailureKind::DestinationExists)
        );
    }

    #[test]
    fn current_note_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        assert!(current_note(&vault).is_none());
        set_current_note(&vault, "notes/foo").unwrap();
        assert_eq!(current_note(&vault).as_deref(), Some("notes/foo"));
    }
}
