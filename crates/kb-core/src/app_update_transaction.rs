//! macOS更新のbundle置換。署名・プロセス起動・KB操作は呼出側の責務。
//!
//! 2026-09-09: updaterの一時退避は次回起動まで残らないため、旧bundleを同じ
//! filesystemに保持する。journalのintentをrenameより先に永続化し、中断後は
//! 既知のidentityと配置形状だけから復元する。receiptの認証は呼出側で行う。

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::app_update_compatibility::{PackageCompatibilityMetadata, compiled_metadata};

const DIRECTORY: &str = ".kb-app-update";
const JOURNAL: &str = "journal.json";
const NEXT_JOURNAL: &str = "journal.pending";
const RESERVED: &[&str] = &[
    "startup.json",
    "supervisor.ready",
    "bootstrap.ok",
    "boot-failed.json",
];
const MAX_JOURNAL: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleIdentity {
    pub executable_sha256: String,
    pub plan_sha256: String,
    pub inventory_sha256: String,
}

impl BundleIdentity {
    fn validate(&self) -> Result<()> {
        ensure!(
            [
                &self.executable_sha256,
                &self.plan_sha256,
                &self.inventory_sha256
            ]
            .into_iter()
            .all(|value| digest(value)),
            "bundle identityが不正"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Prepared,
    OldMoved,
    NewPlaced,
    Accepted,
    RolledBack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    MoveOld,
    PlaceNew,
    MoveFailed,
    RestoreOld,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    pub format_version: u32,
    pub nonce: String,
    pub phase: Phase,
    pub intent: Option<Intent>,
    pub source: BundleIdentity,
    pub new: BundleIdentity,
    pub compatibility: PackageCompatibilityMetadata,
}

impl Journal {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.format_version == 1 && digest(&self.nonce),
            "更新journalの版またはnonceが不正"
        );
        self.source.validate()?;
        self.new.validate()?;
        ensure!(self.source != self.new, "更新前後のidentityが同じ");
        ensure!(
            self.compatibility == compiled_metadata(),
            "対応外の永続形式"
        );
        Ok(())
    }
}

/// nonceとidentityの一致だけを検査する。起動元・UI受入・署名の認証は呼出側で行う。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartupReceipt {
    pub nonce: String,
    pub identity: BundleIdentity,
}

pub struct Transaction {
    target: PathBuf,
    directory: PathBuf,
    nonce: String,
    journal: Option<Journal>,
    // unlinkせず保持し、次回更新も同じinodeをlockする。
    _lock: File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Prepared,
    OldMoved,
    NewPlaced,
    CandidateHeld,
    RestoredWithStage,
    RestoredWithFailed,
}

impl Transaction {
    /// 初回、または検証済み完了transactionだけを再利用する。
    pub fn create(target: &Path, nonce: &str) -> Result<Self> {
        ensure!(digest(nonce), "更新nonceは小文字64桁hexを指定する");
        let (target, directory) = locations(target)?;
        let mut transaction = if metadata(&directory)?.is_some() {
            require_private_directory(&directory)?;
            let lock = lock_file(&directory, false)?;
            let journal = if metadata(&directory.join(JOURNAL))?.is_some() {
                Some(read_journal(&directory)?)
            } else {
                None
            };
            let mut previous = Self {
                target,
                directory,
                nonce: journal
                    .as_ref()
                    .map_or_else(|| nonce.to_owned(), |value| value.nonce.clone()),
                journal,
                _lock: lock,
            };
            if previous.journal.is_some() {
                previous.clear_finished()?;
            } else {
                require_idle(&previous.directory)?;
            }
            previous.nonce = nonce.to_owned();
            previous
        } else {
            private_directory(&directory)?;
            sync_directory(target.parent().context("配置先の親がない")?)?;
            let lock = lock_file(&directory, true)?;
            Self {
                target,
                directory,
                nonce: nonce.to_owned(),
                journal: None,
                _lock: lock,
            }
        };
        ensure!(transaction.journal.is_none(), "未解決の更新がある");
        let new = transaction.staged_parent();
        if metadata(&new)?.is_none() {
            private_directory(&new)?;
            sync_directory(&transaction.directory)?;
        }
        transaction.validate_contents()?;
        // 次回prepareが完了するまでは旧targetへ何も変更しない。
        transaction.journal = None;
        Ok(transaction)
    }

    /// GUIのhandleをdropしてから、supervisorがこの入口で排他を引き継ぐ。
    pub fn resume(target: &Path) -> Result<Self> {
        let (target, directory) = locations(target)?;
        require_private_directory(&directory)?;
        let lock = lock_file(&directory, false)?;
        let journal = read_journal(&directory)?;
        let transaction = Self {
            target,
            directory,
            nonce: journal.nonce.clone(),
            journal: Some(journal),
            _lock: lock,
        };
        transaction.validate_contents()?;
        transaction.checked_shape()?;
        Ok(transaction)
    }

    pub fn staged_parent(&self) -> PathBuf {
        self.directory.join("new")
    }
    pub fn staged_app(&self) -> PathBuf {
        self.staged_parent().join("kb-app.app")
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    pub fn target(&self) -> &Path {
        &self.target
    }
    pub fn journal(&self) -> Option<&Journal> {
        self.journal.as_ref()
    }

    /// 呼出側が署名・package構造・互換性を検査した後、同じ実物を固定する。
    pub fn prepare(
        &mut self,
        source: &BundleIdentity,
        candidate: &BundleIdentity,
        compatibility: &PackageCompatibilityMetadata,
    ) -> Result<()> {
        ensure!(self.journal.is_none(), "更新は既に準備済み");
        self.validate_contents()?;
        ensure!(
            metadata(&self.directory.join(JOURNAL))?.is_none(),
            "既存journalがある"
        );
        ensure!(
            metadata(&self.previous())?.is_none() && metadata(&self.failed())?.is_none(),
            "既存退避物がある"
        );
        verify_bundle(&self.target, source)?;
        verify_bundle(&self.staged_app(), candidate)?;
        // 展開済みfileの内容をrenameだけでdurableになったとは扱わない。
        sync_tree(&self.staged_app())?;
        sync_directory(&self.staged_parent())?;
        let journal = Journal {
            format_version: 1,
            nonce: self.nonce.clone(),
            phase: Phase::Prepared,
            intent: None,
            source: source.clone(),
            new: candidate.clone(),
            compatibility: compatibility.clone(),
        };
        journal.validate()?;
        self.save(journal)
    }

    /// 2026-09-09: 展開・署名検査の失敗で、未journalの準備物を永久に残さない。
    /// このhandleが排他を保持し、まだ旧bundleを動かしていない場合だけ使う。
    pub fn abort_preparation(&mut self) -> Result<()> {
        ensure!(self.journal.is_none(), "準備済み更新はrollbackが必要");
        self.validate_contents()?;
        ensure!(
            metadata(&self.directory.join(JOURNAL))?.is_none(),
            "保存済みjournalは復元対象"
        );
        ensure!(
            metadata(&self.previous())?.is_none() && metadata(&self.failed())?.is_none(),
            "退避物があるため準備を削除しない"
        );
        for name in RESERVED {
            ensure!(
                metadata(&self.directory.join(name))?.is_none(),
                "起動処理の証拠がある"
            );
        }
        let stage = self.staged_app();
        if metadata(&stage)?.is_some() {
            validate_regular_tree(&stage)?;
            fs::remove_dir_all(stage)?;
            sync_directory(&self.staged_parent())?;
        }
        let pending = self.directory.join(NEXT_JOURNAL);
        if metadata(&pending)?.is_some() {
            require_file(&pending)?;
            fs::remove_file(pending)?;
        }
        sync_directory(&self.directory)?;
        require_idle(&self.directory)
    }

    /// rename失敗時も旧版を消さず、可能ならその場で復元する。
    pub fn replace(&mut self) -> Result<()> {
        self.replace_observed(|_| Ok(()))
    }

    fn replace_observed(&mut self, mut observe: impl FnMut(Shape) -> Result<()>) -> Result<()> {
        self.reload()?;
        ensure!(
            self.checked_shape()? == Shape::Prepared,
            "置換開始時の配置が不正"
        );
        ensure!(
            self.current()?.phase == Phase::Prepared && self.current()?.intent.is_none(),
            "置換済みまたは中断中"
        );
        let result: Result<()> = (|| {
            self.transition(Phase::Prepared, Some(Intent::MoveOld))?;
            fs::rename(&self.target, self.previous()).context("旧bundleの退避に失敗")?;
            self.sync_locations()?;
            observe(Shape::OldMoved)?;
            self.transition(Phase::OldMoved, None)?;
            self.transition(Phase::OldMoved, Some(Intent::PlaceNew))?;
            fs::rename(self.staged_app(), &self.target).context("新bundleの配置に失敗")?;
            self.sync_locations()?;
            observe(Shape::NewPlaced)?;
            self.transition(Phase::NewPlaced, None)?;
            self.checked_shape()?;
            Ok(())
        })();
        if let Err(error) = result {
            return match self.rollback() {
                Ok(()) => Err(error.context("配置失敗後に旧bundleへ復元した")),
                Err(rollback) => {
                    Err(error.context(format!("復元未完了。退避物を保持: {rollback:#}")))
                }
            };
        }
        Ok(())
    }

    /// 認証済みUI受入を呼出側で確認した後だけ呼ぶ。旧bundleは次回更新まで保持する。
    pub fn accept(&mut self, receipt: &StartupReceipt) -> Result<()> {
        self.reload()?;
        let journal = self.current()?;
        ensure!(
            journal.phase == Phase::NewPlaced && journal.intent.is_none(),
            "起動受入待ちではない"
        );
        ensure!(
            receipt.nonce == journal.nonce && receipt.identity == journal.new,
            "起動receiptが一致しない"
        );
        ensure!(
            self.checked_shape()? == Shape::NewPlaced,
            "受入対象が変わった"
        );
        self.transition(Phase::Accepted, None)
    }

    /// 中断したrenameの前後も識別して戻す。未知のtargetを上書きしない。
    pub fn rollback(&mut self) -> Result<()> {
        self.reload()?;
        ensure!(
            self.current()?.phase != Phase::Accepted,
            "受入済み更新を自動で巻き戻さない"
        );
        let mut shape = self.checked_shape()?;
        match shape {
            Shape::Prepared | Shape::RestoredWithStage | Shape::RestoredWithFailed => {
                return self.transition(Phase::RolledBack, None);
            }
            Shape::NewPlaced => {
                self.transition(Phase::NewPlaced, Some(Intent::MoveFailed))?;
                fs::rename(&self.target, self.failed()).context("新bundleの退避に失敗")?;
                self.sync_locations()?;
                shape = Shape::CandidateHeld;
            }
            Shape::OldMoved | Shape::CandidateHeld => {}
        }
        let phase = if shape == Shape::OldMoved {
            Phase::OldMoved
        } else {
            Phase::NewPlaced
        };
        self.transition(phase, Some(Intent::RestoreOld))?;
        ensure!(
            metadata(&self.target)?.is_none(),
            "復元先に未知のbundleがある"
        );
        fs::rename(self.previous(), &self.target).context("旧bundleの復元に失敗")?;
        self.sync_locations()?;
        self.transition(Phase::RolledBack, None)?;
        self.checked_shape()?;
        Ok(())
    }

    /// 次の明示的更新でのみ呼ぶ。現在版が既知でなければ最後の退避物を消さない。
    pub fn clear_finished(&mut self) -> Result<()> {
        self.reload()?;
        let phase = self.current()?.phase;
        ensure!(
            matches!(phase, Phase::Accepted | Phase::RolledBack),
            "未解決のtransactionは削除しない"
        );
        self.checked_shape()?;
        self.validate_contents()?;
        for path in [self.previous(), self.failed(), self.staged_app()] {
            if metadata(&path)?.is_some() {
                fs::remove_dir_all(&path).context("完了した退避物を削除できない")?;
            }
        }
        for name in RESERVED.iter().copied().chain([NEXT_JOURNAL]) {
            let path = self.directory.join(name);
            if metadata(&path)?.is_some() {
                fs::remove_file(path)?;
            }
        }
        sync_directory(&self.staged_parent())?;
        sync_directory(&self.directory)?;
        fs::remove_file(self.directory.join(JOURNAL))?;
        sync_directory(&self.directory)?;
        self.journal = None;
        Ok(())
    }

    fn previous(&self) -> PathBuf {
        self.directory.join("previous.app")
    }
    fn failed(&self) -> PathBuf {
        self.directory.join("failed.app")
    }
    fn current(&self) -> Result<&Journal> {
        self.journal.as_ref().context("更新journalがない")
    }
    fn reload(&mut self) -> Result<()> {
        self.validate_contents()?;
        let journal = read_journal(&self.directory)?;
        ensure!(journal.nonce == self.nonce, "別transactionへ差し替わった");
        self.journal = Some(journal);
        Ok(())
    }

    fn transition(&mut self, phase: Phase, intent: Option<Intent>) -> Result<()> {
        let mut next = self.current()?.clone();
        next.phase = phase;
        next.intent = intent;
        self.save(next)
    }

    fn save(&mut self, journal: Journal) -> Result<()> {
        journal.validate()?;
        require_private_directory(&self.directory)?;
        let next = self.directory.join(NEXT_JOURNAL);
        if metadata(&next)?.is_some() {
            require_file(&next)?;
            fs::remove_file(&next)?;
            sync_directory(&self.directory)?;
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&next)?;
        file.write_all(&serde_json::to_vec(&journal)?)?;
        file.sync_all()?;
        fs::rename(&next, self.directory.join(JOURNAL))?;
        sync_directory(&self.directory)?;
        self.journal = Some(journal);
        Ok(())
    }

    fn sync_locations(&self) -> Result<()> {
        sync_directory(self.target.parent().context("配置先の親がない")?)?;
        sync_directory(&self.directory)?;
        sync_directory(&self.staged_parent())
    }

    fn checked_shape(&self) -> Result<Shape> {
        let journal = self.current()?;
        let source_at_target = matches_bundle(&self.target, &journal.source)?;
        let candidate_at_target = if source_at_target {
            false
        } else {
            matches_bundle(&self.target, &journal.new)?
        };
        let target_absent = metadata(&self.target)?.is_none();
        ensure!(
            source_at_target || candidate_at_target || target_absent,
            "配置先bundleが未知または改変された"
        );
        let previous = optional_verified(&self.previous(), &journal.source)?;
        let staged = optional_verified(&self.staged_app(), &journal.new)?;
        let failed = optional_verified(&self.failed(), &journal.new)?;
        let shape = match (
            source_at_target,
            candidate_at_target,
            target_absent,
            previous,
            staged,
            failed,
        ) {
            (true, false, false, false, true, false) => Shape::Prepared,
            (false, false, true, true, true, false) => Shape::OldMoved,
            (false, true, false, true, false, false) => Shape::NewPlaced,
            (false, false, true, true, false, true) => Shape::CandidateHeld,
            (true, false, false, false, false, true) => Shape::RestoredWithFailed,
            _ => bail!("更新配置の組み合わせが不明。退避物を保持する"),
        };
        let allowed = match (journal.phase, journal.intent) {
            (Phase::Prepared, None) => shape == Shape::Prepared,
            (Phase::Prepared, Some(Intent::MoveOld)) => {
                matches!(shape, Shape::Prepared | Shape::OldMoved)
            }
            (Phase::OldMoved, None) => shape == Shape::OldMoved,
            (Phase::OldMoved, Some(Intent::PlaceNew)) => {
                matches!(shape, Shape::OldMoved | Shape::NewPlaced)
            }
            (Phase::NewPlaced, None) | (Phase::Accepted, None) => shape == Shape::NewPlaced,
            (Phase::NewPlaced, Some(Intent::MoveFailed)) => {
                matches!(shape, Shape::NewPlaced | Shape::CandidateHeld)
            }
            (Phase::OldMoved, Some(Intent::RestoreOld)) => {
                matches!(shape, Shape::OldMoved | Shape::Prepared)
            }
            (Phase::NewPlaced, Some(Intent::RestoreOld)) => {
                matches!(shape, Shape::CandidateHeld | Shape::RestoredWithFailed)
            }
            (Phase::RolledBack, None) => {
                matches!(shape, Shape::Prepared | Shape::RestoredWithFailed)
            }
            _ => false,
        };
        ensure!(allowed, "journalと配置の形状が一致しない");
        Ok(
            if shape == Shape::Prepared && journal.phase == Phase::RolledBack {
                Shape::RestoredWithStage
            } else {
                shape
            },
        )
    }

    fn validate_contents(&self) -> Result<()> {
        require_private_directory(&self.directory)?;
        require_same_filesystem(
            self.target.parent().context("配置先の親がない")?,
            &self.directory,
        )?;
        for entry in fs::read_dir(&self.directory)? {
            let path = entry?.path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .context("未知のtransaction entry")?;
            if matches!(name, "new" | "previous.app" | "failed.app") {
                require_directory(&path)?;
            } else {
                ensure!(
                    matches!(name, "lock" | JOURNAL | NEXT_JOURNAL) || RESERVED.contains(&name),
                    "未知のtransaction file"
                );
                require_file(&path)?;
            }
        }
        let new = self.staged_parent();
        if metadata(&new)?.is_some() {
            require_private_directory(&new)?;
            require_same_filesystem(&self.directory, &new)?;
            for entry in fs::read_dir(new)? {
                let path = entry?.path();
                ensure!(
                    path.file_name() == Some(std::ffi::OsStr::new("kb-app.app")),
                    "未知の展開物"
                );
                require_directory(&path)?;
            }
        }
        Ok(())
    }
}

/// 起動時の観測だけを行う。排他・署名認証・起動許可は発行しない。
pub fn inspect(target: &Path) -> Result<Option<Journal>> {
    let (_, directory) = locations(target)?;
    if metadata(&directory)?.is_none() {
        return Ok(None);
    }
    require_private_directory(&directory)?;
    if metadata(&directory.join(JOURNAL))?.is_none() {
        require_idle(&directory)?;
        return Ok(None);
    }
    Ok(Some(read_journal(&directory)?))
}

fn require_idle(directory: &Path) -> Result<()> {
    require_private_directory(directory)?;
    require_file(&directory.join("lock"))?;
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        match path.file_name().and_then(|value| value.to_str()) {
            Some("lock") => {
                require_file(&path)?;
            }
            Some("new") => {
                require_private_directory(&path)?;
                ensure!(
                    fs::read_dir(&path)?.next().is_none(),
                    "中断した展開物があるため明示的な復旧が必要"
                );
            }
            _ => bail!("未journalの未知の内容を削除しない"),
        }
    }
    Ok(())
}

fn require_same_filesystem(left: &Path, right: &Path) -> Result<()> {
    let left = require_directory(left)?;
    let right = require_directory(right)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            left.dev() == right.dev(),
            "配置先と更新物は同じfilesystemが必要"
        );
    }
    #[cfg(not(unix))]
    {
        let _ = (left, right);
    }
    Ok(())
}

fn validate_regular_tree(path: &Path) -> Result<()> {
    require_directory(path)?;
    for entry in fs::read_dir(path)? {
        let child = entry?.path();
        let info = metadata(&child)?.context("準備物が消えた")?;
        if info.is_dir() {
            validate_regular_tree(&child)?;
        } else {
            require_file(&child)?;
        }
    }
    Ok(())
}

fn read_journal(directory: &Path) -> Result<Journal> {
    let path = directory.join(JOURNAL);
    let info = require_file(&path)?;
    ensure!(info.len() <= MAX_JOURNAL, "更新journalが大きすぎる");
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_JOURNAL + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_JOURNAL, "更新journalが大きすぎる");
    let journal: Journal = serde_json::from_slice(&bytes).context("更新journalを読めない")?;
    journal.validate()?;
    Ok(journal)
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn locations(target: &Path) -> Result<(PathBuf, PathBuf)> {
    ensure!(
        target.is_absolute() && target.file_name() == Some(std::ffi::OsStr::new("kb-app.app")),
        "固定kb-app.appだけを扱う"
    );
    ensure!(
        !target
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir)),
        "非正規path"
    );
    let parent = target.parent().context("配置先の親がない")?;
    require_directory(parent)?;
    ensure!(
        parent.canonicalize()? == parent,
        "配置先の親がcanonicalではない"
    );
    if metadata(target)?.is_some() {
        require_directory(target)?;
    }
    Ok((target.to_path_buf(), parent.join(DIRECTORY)))
}

fn metadata(path: &Path) -> Result<Option<Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(info) => {
            ensure!(!info.file_type().is_symlink(), "symlinkは扱わない");
            Ok(Some(info))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn require_directory(path: &Path) -> Result<Metadata> {
    let info = metadata(path)?.context("directoryがない")?;
    ensure!(info.is_dir(), "通常directoryではない");
    Ok(info)
}

fn require_private_directory(path: &Path) -> Result<()> {
    let info = require_directory(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(info.mode() & 0o777 == 0o700, "更新directoryは0700が必要");
    }
    #[cfg(not(unix))]
    {
        let _ = info;
    }
    Ok(())
}

fn require_file(path: &Path) -> Result<Metadata> {
    let info = metadata(path)?.context("fileがない")?;
    ensure!(info.is_file(), "通常fileではない");
    Ok(info)
}

fn private_directory(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    require_private_directory(path)
}

fn lock_file(directory: &Path, create: bool) -> Result<File> {
    let path = directory.join("lock");
    if !create {
        require_file(&path)?;
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path)?;
    ensure!(file.try_lock_exclusive()?, "別の更新処理が実行中");
    if create {
        file.sync_all()?;
        sync_directory(directory)?;
    }
    Ok(file)
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all().map_err(Into::into)
}

fn hash_file(path: &Path) -> Result<String> {
    let before = require_file(path)?;
    let mut file = File::open(path)?;
    let opened = file.metadata()?;
    ensure!(
        opened.is_file() && same_file(&before, &opened),
        "fileが検査中に変わった"
    );
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    ensure!(
        same_file(&opened, &file.metadata()?),
        "fileが読取中に変わった"
    );
    Ok(format!("{:x}", digest.finalize()))
}

fn same_file(left: &Metadata, right: &Metadata) -> bool {
    let base = left.len() == right.len() && left.modified().ok() == right.modified().ok();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        base && left.dev() == right.dev()
            && left.ino() == right.ino()
            && left.mode() == right.mode()
    }
    #[cfg(not(unix))]
    {
        base
    }
}

fn mode(info: &Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        info.mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        u32::from(info.permissions().readonly())
    }
}

/// 署名そのものの検証ではない。symlinkを含むbundleは初版の対象外にする。
pub fn bundle_identity(app: &Path) -> Result<BundleIdentity> {
    require_directory(app)?;
    ensure!(app.canonicalize()? == app, "bundle pathがcanonicalではない");
    let mut inventory = Sha256::new();
    fn walk(root: &Path, path: &Path, inventory: &mut Sha256) -> Result<()> {
        let info = require_directory(path)?;
        let relative = path
            .strip_prefix(root)?
            .to_str()
            .context("非UTF-8 bundle entry")?;
        record(inventory, b'd', relative, mode(&info), "");
        let mut entries = fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        entries.sort();
        for child in entries {
            let info = metadata(&child)?.context("bundle entryが消えた")?;
            if info.is_dir() {
                walk(root, &child, inventory)?;
            } else {
                ensure!(info.is_file(), "bundleの非通常entry");
                let relative = child
                    .strip_prefix(root)?
                    .to_str()
                    .context("非UTF-8 bundle entry")?;
                record(inventory, b'f', relative, mode(&info), &hash_file(&child)?);
            }
        }
        Ok(())
    }
    walk(app, app, &mut inventory)?;
    Ok(BundleIdentity {
        executable_sha256: hash_file(&app.join("Contents/MacOS/kb-app"))?,
        plan_sha256: hash_file(&app.join("Contents/Resources/release/plan.json"))?,
        inventory_sha256: format!("{:x}", inventory.finalize()),
    })
}

fn record(digest: &mut Sha256, kind: u8, path: &str, mode: u32, content: &str) {
    digest.update([kind]);
    digest.update((path.len() as u64).to_be_bytes());
    digest.update(path.as_bytes());
    digest.update(mode.to_be_bytes());
    digest.update(content.as_bytes());
}

fn verify_bundle(path: &Path, expected: &BundleIdentity) -> Result<()> {
    ensure!(
        bundle_identity(path)? == *expected,
        "bundle identityが一致しない"
    );
    Ok(())
}
fn matches_bundle(path: &Path, expected: &BundleIdentity) -> Result<bool> {
    if metadata(path)?.is_none() {
        return Ok(false);
    }
    Ok(bundle_identity(path)? == *expected)
}
fn optional_verified(path: &Path, expected: &BundleIdentity) -> Result<bool> {
    if metadata(path)?.is_none() {
        return Ok(false);
    }
    verify_bundle(path, expected)?;
    Ok(true)
}
fn sync_tree(path: &Path) -> Result<()> {
    require_directory(path)?;
    for entry in fs::read_dir(path)? {
        let child = entry?.path();
        let info = metadata(&child)?.context("展開物が消えた")?;
        if info.is_dir() {
            sync_tree(&child)?;
        } else {
            require_file(&child)?;
            File::open(child)?.sync_all()?;
        }
    }
    sync_directory(path)
}
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{TempDir, tempdir};

    fn fake_bundle(path: &Path, value: &str) -> Result<()> {
        fs::create_dir_all(path.join("Contents/MacOS"))?;
        fs::create_dir_all(path.join("Contents/Resources/release"))?;
        fs::write(path.join("Contents/MacOS/kb-app"), value)?;
        fs::write(path.join("Contents/Resources/release/plan.json"), value)?;
        fs::write(path.join("Contents/Resources/asset.txt"), value)?;
        Ok(())
    }

    fn unprepared() -> Result<(TempDir, Transaction, BundleIdentity)> {
        let temp = tempdir()?;
        let target = temp.path().canonicalize()?.join("kb-app.app");
        fake_bundle(&target, "old")?;
        let source = bundle_identity(&target)?;
        let transaction = Transaction::create(&target, &"a".repeat(64))?;
        Ok((temp, transaction, source))
    }

    fn setup() -> Result<(TempDir, Transaction, BundleIdentity, BundleIdentity)> {
        let (temp, mut transaction, source) = unprepared()?;
        fake_bundle(&transaction.staged_app(), "new")?;
        let new = bundle_identity(&transaction.staged_app())?;
        transaction.prepare(&source, &new, &compiled_metadata())?;
        Ok((temp, transaction, source, new))
    }

    /// 2026-09-09: UI受入まで旧版を残し、次回更新は同じlock inodeで再利用する。
    #[test]
    fn accept_keeps_backup_and_next_create_prunes_only_finished() -> Result<()> {
        let (_temp, mut transaction, source, new) = setup()?;
        transaction.replace()?;
        assert_eq!(bundle_identity(&transaction.previous())?, source);
        assert!(
            transaction
                .accept(&StartupReceipt {
                    nonce: "b".repeat(64),
                    identity: new.clone()
                })
                .is_err()
        );
        let lock_info = fs::metadata(transaction.directory.join("lock"))?;
        transaction.accept(&StartupReceipt {
            nonce: "a".repeat(64),
            identity: new.clone(),
        })?;
        for name in RESERVED {
            fs::write(transaction.directory.join(name), "fixture")?;
        }
        let target = transaction.target.clone();
        drop(transaction);
        let next = Transaction::create(&target, &"b".repeat(64))?;
        assert_eq!(bundle_identity(&target)?, new);
        assert!(!next.previous().exists());
        assert!(next.journal().is_none());
        assert!(inspect(&target)?.is_none());
        for name in RESERVED {
            assert!(!next.directory.join(name).exists());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                lock_info.ino(),
                fs::metadata(next.directory.join("lock"))?.ino()
            );
        }
        #[cfg(not(unix))]
        {
            let _ = lock_info;
        }
        Ok(())
    }

    /// 2026-09-09: rename直後の失敗を、戻す前のjournalでも復元できること。
    #[test]
    fn replacement_failure_at_each_rename_restores_old_bundle() -> Result<()> {
        for failure in [Shape::OldMoved, Shape::NewPlaced] {
            let (_temp, mut transaction, source, new) = setup()?;
            assert!(
                transaction
                    .replace_observed(|shape| {
                        ensure!(shape != failure, "合成rename後障害");
                        Ok(())
                    })
                    .is_err()
            );
            assert_eq!(bundle_identity(transaction.target())?, source);
            assert_eq!(transaction.current()?.phase, Phase::RolledBack);
            let retained_new = if failure == Shape::OldMoved {
                transaction.staged_app()
            } else {
                transaction.failed()
            };
            assert_eq!(bundle_identity(&retained_new)?, new);
        }
        Ok(())
    }

    /// 2026-09-09: 各intent保存直後・rename直後で終了しても、未知の配置へ進まない。
    #[test]
    fn resume_recovers_each_persisted_intent_window() -> Result<()> {
        for window in 0..8 {
            let (_temp, mut transaction, source, _new) = setup()?;
            if window < 2 {
                transaction.transition(Phase::Prepared, Some(Intent::MoveOld))?;
                if window == 1 {
                    fs::rename(&transaction.target, transaction.previous())?;
                }
            } else if window < 4 {
                transaction.transition(Phase::Prepared, Some(Intent::MoveOld))?;
                fs::rename(&transaction.target, transaction.previous())?;
                transaction.transition(Phase::OldMoved, Some(Intent::PlaceNew))?;
                if window == 3 {
                    fs::rename(transaction.staged_app(), &transaction.target)?;
                }
            } else if window < 6 {
                transaction.replace()?;
                transaction.transition(Phase::NewPlaced, Some(Intent::MoveFailed))?;
                if window == 5 {
                    fs::rename(&transaction.target, transaction.failed())?;
                }
            } else {
                transaction.replace()?;
                transaction.transition(Phase::NewPlaced, Some(Intent::MoveFailed))?;
                fs::rename(&transaction.target, transaction.failed())?;
                transaction.transition(Phase::NewPlaced, Some(Intent::RestoreOld))?;
                if window == 7 {
                    fs::rename(transaction.previous(), &transaction.target)?;
                }
            }
            let target = transaction.target.clone();
            drop(transaction);
            let mut resumed = Transaction::resume(&target)?;
            resumed.rollback()?;
            assert_eq!(bundle_identity(&target)?, source, "window {window}");
            assert_eq!(resumed.current()?.phase, Phase::RolledBack);
        }
        Ok(())
    }

    /// 2026-09-09: 起動済み新bundleのresource改変をreceiptやrollbackで見逃さない。
    #[test]
    fn changed_new_resource_refuses_accept_and_preserves_previous() -> Result<()> {
        let (_temp, mut transaction, source, new) = setup()?;
        transaction.replace()?;
        fs::write(
            transaction.target.join("Contents/Resources/asset.txt"),
            "changed",
        )?;
        assert!(
            transaction
                .accept(&StartupReceipt {
                    nonce: "a".repeat(64),
                    identity: new
                })
                .is_err()
        );
        assert!(transaction.rollback().is_err());
        assert_eq!(bundle_identity(&transaction.previous())?, source);
        assert_eq!(
            fs::read_to_string(transaction.target.join("Contents/Resources/asset.txt"))?,
            "changed"
        );
        Ok(())
    }

    /// 2026-09-09: 準備後のsource改変を旧版として退避しない。
    #[test]
    fn changed_source_refuses_replace_without_moving_either_bundle() -> Result<()> {
        let (_temp, mut transaction, _source, new) = setup()?;
        fs::write(
            transaction.target.join("Contents/Resources/asset.txt"),
            "changed",
        )?;
        assert!(transaction.replace().is_err());
        assert!(transaction.target.exists());
        assert!(!transaction.previous().exists());
        assert_eq!(bundle_identity(&transaction.staged_app())?, new);
        Ok(())
    }

    /// 2026-09-09: supervisorへlockを渡す前の二重resumeは待ち続けず拒否する。
    #[test]
    fn held_lock_and_unfinished_transaction_block_next_update() -> Result<()> {
        let (_temp, transaction, _source, _new) = setup()?;
        let target = transaction.target.clone();
        assert!(Transaction::resume(&target).is_err());
        assert!(Transaction::create(&target, &"b".repeat(64)).is_err());
        drop(transaction);
        assert!(Transaction::create(&target, &"b".repeat(64)).is_err());
        Ok(())
    }

    /// 2026-09-09: journal破損や未知fileを理由に旧bundleを自動削除しない。
    #[test]
    fn invalid_journal_and_unknown_entry_preserve_accepted_backup() -> Result<()> {
        let (_temp, mut transaction, source, new) = setup()?;
        transaction.replace()?;
        transaction.accept(&StartupReceipt {
            nonce: "a".repeat(64),
            identity: new,
        })?;
        let target = transaction.target.clone();
        let previous = transaction.previous();
        fs::write(transaction.directory.join("unknown"), "keep")?;
        assert!(transaction.clear_finished().is_err());
        assert_eq!(bundle_identity(&previous)?, source);
        fs::remove_file(transaction.directory.join("unknown"))?;
        fs::write(transaction.directory.join(JOURNAL), "{}")?;
        drop(transaction);
        assert!(Transaction::resume(&target).is_err());
        assert_eq!(bundle_identity(&previous)?, source);
        Ok(())
    }

    /// 2026-09-09: 未展開・途中展開の失敗をabortした後、同じ場所で再準備できる。
    #[test]
    fn preparation_abort_recycles_empty_locked_directory() -> Result<()> {
        let (_temp, mut transaction, source) = unprepared()?;
        fs::create_dir_all(transaction.staged_app().join("partial"))?;
        fs::write(transaction.staged_app().join("partial/asset"), "partial")?;
        fs::write(transaction.directory.join(NEXT_JOURNAL), "partial json")?;
        transaction.abort_preparation()?;
        assert_eq!(bundle_identity(transaction.target())?, source);
        assert!(inspect(transaction.target())?.is_none());
        let target = transaction.target.clone();
        drop(transaction);
        let next = Transaction::create(&target, &"b".repeat(64))?;
        assert!(!next.staged_app().exists());
        Ok(())
    }

    /// 2026-09-09: journal保存後の失敗を未準備abortで消すと復元根拠を失う。
    #[test]
    fn abort_never_removes_saved_preparation_or_unknown_stage() -> Result<()> {
        let (_temp, mut transaction, source, new) = setup()?;
        assert!(transaction.abort_preparation().is_err());
        assert_eq!(bundle_identity(transaction.target())?, source);
        assert_eq!(bundle_identity(&transaction.staged_app())?, new);
        transaction.rollback()?;
        let target = transaction.target.clone();
        drop(transaction);
        let next = Transaction::create(&target, &"b".repeat(64))?;
        assert!(!next.staged_app().exists());
        assert_eq!(bundle_identity(&target)?, source);
        Ok(())
    }

    /// 2026-09-09: 固定temp名がdirectoryならwrite失敗前に拒否し、旧版を動かさない。
    #[test]
    fn invalid_journal_temp_fails_before_first_rename() -> Result<()> {
        let (_temp, mut transaction, source, new) = setup()?;
        fs::create_dir(transaction.directory.join(NEXT_JOURNAL))?;
        assert!(transaction.replace().is_err());
        assert_eq!(bundle_identity(transaction.target())?, source);
        assert_eq!(bundle_identity(&transaction.staged_app())?, new);
        assert!(!transaction.previous().exists());
        Ok(())
    }

    /// 2026-09-09: symlinkを含む途中展開を再帰削除せず、外側のfileも保持する。
    #[cfg(unix)]
    #[test]
    fn preparation_symlink_is_rejected_without_cleanup() -> Result<()> {
        use std::os::unix::fs::symlink;
        let (temp, mut transaction, source) = unprepared()?;
        let outside = temp.path().canonicalize()?.join("outside");
        fs::write(&outside, "keep")?;
        fs::create_dir(transaction.staged_app())?;
        symlink(&outside, transaction.staged_app().join("link"))?;
        assert!(transaction.abort_preparation().is_err());
        assert!(transaction.staged_app().exists());
        assert_eq!(fs::read_to_string(outside)?, "keep");
        assert_eq!(bundle_identity(transaction.target())?, source);
        Ok(())
    }
}
