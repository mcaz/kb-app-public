//! 更新アーカイブの内容検査。署名の真正性や旧新writerの共存を認証するAPIではない。
//!
//! 呼出側は公式updaterで署名を検証した同じbytesを渡し、配置前に署名・稼働source・
//! DB診断を別途確認する。tarは単一kb-app.app、通常file/directory、明示した全親だけを
//! 許す。PAX/GNU長名・link・特殊fileを使う配布物は生成側で解消する。
//! 展開先は呼出側が排他的に所有する空directoryとし、実アプリの配置先には使わない。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use flate2::bufread::GzDecoder;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::app_update_compatibility::{PackageCompatibilityMetadata, compiled_metadata};

// 現行候補の圧縮約60MiB・展開約170MiBに余裕を持たせ、取得済みbytesと展開を制限する。
pub const MAX_COMPRESSED_BYTES: usize = 128 * 1024 * 1024;
const APP_ROOT: &str = "kb-app.app";
const EXECUTABLE: &str = "kb-app.app/Contents/MacOS/kb-app";
const INFO_PLIST: &str = "kb-app.app/Contents/Info.plist";
const PLAN: &str = "kb-app.app/Contents/Resources/release/plan.json";

#[derive(Clone, Copy)]
struct Limits {
    compressed: usize,
    decompressed: usize,
    entries: usize,
    entry: usize,
    plan: usize,
    plist: usize,
}

const LIMITS: Limits = Limits {
    compressed: MAX_COMPRESSED_BYTES,
    decompressed: 512 * 1024 * 1024,
    entries: 4096,
    entry: 256 * 1024 * 1024,
    plan: 256 * 1024,
    plist: 64 * 1024,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageValidationError {
    CompressedLimitExceeded,
    DecompressedLimitExceeded,
    EntryLimitExceeded,
    EntrySizeLimitExceeded,
    InvalidGzip,
    GzipTrailingData,
    InvalidTar,
    UnsupportedEntry,
    InvalidPath,
    DuplicatePath,
    PathCollision,
    InvalidMode,
    MissingRequiredEntry,
    InvalidPlan,
    UnsupportedPlan,
    InvalidVersion,
    VersionMismatch,
    VersionNotNewer,
    TargetMismatch,
    ProductMismatch,
    CompatibilityMismatch,
    InvalidCompatibleSources,
    InvalidPlist,
    PlistMismatch,
    UnsafeStaging,
    StagingNotEmpty,
    ExtractionFailed,
}

impl fmt::Display for PackageValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "更新packageの内容検査に失敗しました: {self:?}")
    }
}

impl std::error::Error for PackageValidationError {}

#[derive(Debug, Clone, Deserialize)]
struct CompatibleSource {
    plan_sha256: String,
    executable_sha256: String,
}

#[derive(Debug, Deserialize)]
struct ReleasePlan {
    schema: String,
    version: String,
    target: String,
    identifier: String,
    product_name: String,
    source_commit: String,
    compatibility: PackageCompatibilityMetadata,
    compatible_sources: Vec<CompatibleSource>,
}

#[derive(Deserialize)]
struct BundleInfo {
    #[serde(rename = "CFBundleIdentifier")]
    identifier: String,
    #[serde(rename = "CFBundleExecutable")]
    executable: String,
    #[serde(rename = "CFBundleShortVersionString")]
    version: String,
    #[serde(rename = "CFBundleVersion")]
    build_version: String,
}

#[derive(Debug)]
pub struct PackageEntry {
    path: String,
    directory: bool,
    mode: u32,
    size: usize,
    offset: usize,
    sha256: String,
}

impl PackageEntry {
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn is_directory(&self) -> bool {
        self.directory
    }

    pub fn mode(&self) -> u32 {
        self.mode
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}

/// 非公開bytesとinventoryを保持し、検査後に呼出側が別archiveへ差し替えられないようにする。
/// この型の取得は、署名検証・codesign・source共存試験の成功を意味しない。
#[derive(Debug)]
pub struct ValidatedPackage {
    archive: Vec<u8>,
    archive_sha256: String,
    plan_sha256: String,
    plan: ReleasePlan,
    entries: Vec<PackageEntry>,
}

impl ValidatedPackage {
    pub fn version(&self) -> &str {
        &self.plan.version
    }

    pub fn target(&self) -> &str {
        &self.plan.target
    }

    pub fn source_commit(&self) -> &str {
        &self.plan.source_commit
    }

    pub fn compatibility(&self) -> &PackageCompatibilityMetadata {
        &self.plan.compatibility
    }

    pub fn archive_sha256(&self) -> &str {
        &self.archive_sha256
    }

    pub fn plan_sha256(&self) -> &str {
        &self.plan_sha256
    }

    pub fn entries(&self) -> &[PackageEntry] {
        &self.entries
    }

    /// 配布者が記載したsource pairとの一致だけを返す。pairの試験証拠は配布工程が担う。
    pub fn supports_source(&self, plan_sha256: &str, executable_sha256: &str) -> bool {
        lowercase_hash(plan_sha256, 64)
            && lowercase_hash(executable_sha256, 64)
            && self.plan.compatible_sources.iter().any(|source| {
                source.plan_sha256 == plan_sha256 && source.executable_sha256 == executable_sha256
            })
    }

    /// 呼出側が排他的に所有する空stagingへ展開する。既存appの上書きや配置は行わない。
    /// 失敗時の部分stagingは呼出側がTempDir単位で破棄する。任意pathを再帰削除しない。
    pub fn extract_into(&self, staging: &Path) -> Result<PathBuf, PackageValidationError> {
        if !cfg!(unix) {
            return Err(PackageValidationError::UnsafeStaging);
        }
        if !staging.is_absolute()
            || !fs::symlink_metadata(staging)
                .map_err(|_| PackageValidationError::UnsafeStaging)?
                .is_dir()
        {
            return Err(PackageValidationError::UnsafeStaging);
        }
        // macOSの/var aliasなどは入口で一度だけ解決し、以後は解決済みrootから操作する。
        let root = staging
            .canonicalize()
            .map_err(|_| PackageValidationError::UnsafeStaging)?;
        ensure_directories(&root)?;
        if fs::read_dir(&root)
            .map_err(|_| PackageValidationError::UnsafeStaging)?
            .next()
            .is_some()
        {
            return Err(PackageValidationError::StagingNotEmpty);
        }
        let tar = decompress(&self.archive, LIMITS)?;
        // 親directoryは全て検査済み。辞書順なら親が先になり、file-parent衝突もない。
        for entry in &self.entries {
            let path = root.join(&entry.path);
            ensure_directories(path.parent().ok_or(PackageValidationError::UnsafeStaging)?)?;
            if entry.directory {
                fs::create_dir(&path).map_err(|_| PackageValidationError::ExtractionFailed)?;
                set_mode(&path, 0o700)?;
            } else {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .map_err(|_| PackageValidationError::ExtractionFailed)?;
                let content = tar
                    .get(entry.offset..entry.offset + entry.size)
                    .ok_or(PackageValidationError::InvalidTar)?;
                file.write_all(content)
                    .map_err(|_| PackageValidationError::ExtractionFailed)?;
                file.sync_all()
                    .map_err(|_| PackageValidationError::ExtractionFailed)?;
                set_mode(&path, entry.mode)?;
            }
        }
        // 読取専用directoryのmodeは子を書いた後に適用する。
        for entry in self.entries.iter().rev().filter(|entry| entry.directory) {
            set_mode(&root.join(&entry.path), entry.mode)?;
        }
        Ok(root.join(APP_ROOT))
    }
}

fn ensure_directories(path: &Path) -> Result<(), PackageValidationError> {
    for ancestor in path.ancestors() {
        if !fs::symlink_metadata(ancestor)
            .map_err(|_| PackageValidationError::UnsafeStaging)?
            .is_dir()
        {
            return Err(PackageValidationError::UnsafeStaging);
        }
    }
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> Result<(), PackageValidationError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|_| PackageValidationError::ExtractionFailed)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Err(PackageValidationError::UnsafeStaging)
    }
}

pub fn validate_macos_package(
    archive: Vec<u8>,
    current_version: &str,
    announced_version: &str,
    expected_target: &str,
) -> Result<ValidatedPackage, PackageValidationError> {
    validate_with_limits(
        archive,
        current_version,
        announced_version,
        expected_target,
        LIMITS,
    )
}

fn validate_with_limits(
    archive: Vec<u8>,
    current_version: &str,
    announced_version: &str,
    expected_target: &str,
    limits: Limits,
) -> Result<ValidatedPackage, PackageValidationError> {
    let current =
        Version::parse(current_version).map_err(|_| PackageValidationError::InvalidVersion)?;
    let announced =
        Version::parse(announced_version).map_err(|_| PackageValidationError::InvalidVersion)?;
    if !announced.cmp_precedence(&current).is_gt() {
        return Err(PackageValidationError::VersionNotNewer);
    }
    if !matches!(
        expected_target,
        "aarch64-apple-darwin" | "x86_64-apple-darwin"
    ) {
        return Err(PackageValidationError::TargetMismatch);
    }
    let tar = decompress(&archive, limits)?;
    let entries = inventory(&tar, limits)?;
    let required = |name: &str, maximum: usize| -> Result<&PackageEntry, PackageValidationError> {
        let entry = entries
            .iter()
            .find(|entry| entry.path == name && !entry.directory)
            .ok_or(PackageValidationError::MissingRequiredEntry)?;
        if entry.size == 0 || entry.size > maximum {
            return Err(PackageValidationError::EntrySizeLimitExceeded);
        }
        Ok(entry)
    };
    let executable = required(EXECUTABLE, limits.entry)?;
    if executable.mode & 0o111 == 0 {
        return Err(PackageValidationError::InvalidMode);
    }
    let plan_entry = required(PLAN, limits.plan)?;
    let plan_bytes = &tar[plan_entry.offset..plan_entry.offset + plan_entry.size];
    let plan: ReleasePlan =
        serde_json::from_slice(plan_bytes).map_err(|_| PackageValidationError::InvalidPlan)?;
    if plan.schema != "kb-app.macos-release-plan/v1" {
        return Err(PackageValidationError::UnsupportedPlan);
    }
    if !lowercase_hash(&plan.source_commit, 40) {
        return Err(PackageValidationError::InvalidPlan);
    }
    if plan.version != announced_version {
        return Err(PackageValidationError::VersionMismatch);
    }
    if plan.target != expected_target {
        return Err(PackageValidationError::TargetMismatch);
    }
    if plan.identifier != "app.kb.desktop" || plan.product_name != "kb-app" {
        return Err(PackageValidationError::ProductMismatch);
    }
    if plan.compatibility != compiled_metadata() {
        return Err(PackageValidationError::CompatibilityMismatch);
    }
    let mut sources = BTreeSet::new();
    for source in &plan.compatible_sources {
        if !lowercase_hash(&source.plan_sha256, 64)
            || !lowercase_hash(&source.executable_sha256, 64)
            || !sources.insert((&source.plan_sha256, &source.executable_sha256))
        {
            return Err(PackageValidationError::InvalidCompatibleSources);
        }
    }
    let plist_entry = required(INFO_PLIST, limits.plist)?;
    let plist_bytes = &tar[plist_entry.offset..plist_entry.offset + plist_entry.size];
    // plistのserdeは未知のnested値も再帰する。byte上限内の極端な入れ子も先に拒否する。
    if plist_bytes.iter().filter(|byte| **byte == b'<').count() > 512 {
        return Err(PackageValidationError::InvalidPlist);
    }
    // serdeのstructで既知keyの重複も拒否し、Valueへ変換してlast-key-winsにしない。
    let info: BundleInfo =
        plist::from_reader_xml(plist_bytes).map_err(|_| PackageValidationError::InvalidPlist)?;
    if info.identifier != plan.identifier
        || info.executable != "kb-app"
        || info.version != plan.version
        || info.build_version != plan.version
    {
        return Err(PackageValidationError::PlistMismatch);
    }
    Ok(ValidatedPackage {
        archive_sha256: hash(&archive),
        archive,
        plan_sha256: hash(plan_bytes),
        plan,
        entries,
    })
}

fn lowercase_hash(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn decompress(archive: &[u8], limits: Limits) -> Result<Vec<u8>, PackageValidationError> {
    if archive.len() > limits.compressed {
        return Err(PackageValidationError::CompressedLimitExceeded);
    }
    let mut decoder = GzDecoder::new(Cursor::new(archive));
    let mut tar = Vec::new();
    decoder
        .by_ref()
        .take(limits.decompressed as u64 + 1)
        .read_to_end(&mut tar)
        .map_err(|_| PackageValidationError::InvalidGzip)?;
    if tar.len() > limits.decompressed {
        return Err(PackageValidationError::DecompressedLimitExceeded);
    }
    // bufread decoderは最初のmemberでEOFになる。残りは別member/garbageとも拒否する。
    if decoder.into_inner().position() != archive.len() as u64 {
        return Err(PackageValidationError::GzipTrailingData);
    }
    Ok(tar)
}

fn inventory(
    tar_bytes: &[u8],
    limits: Limits,
) -> Result<Vec<PackageEntry>, PackageValidationError> {
    let mut archive = tar::Archive::new(Cursor::new(tar_bytes));
    let mut entries = BTreeMap::new();
    let mut folded_paths = BTreeSet::new();
    let mut next_header = 0;
    for entry in archive
        .entries()
        .map_err(|_| PackageValidationError::InvalidTar)?
        .raw(true)
    {
        let entry = entry.map_err(|_| PackageValidationError::InvalidTar)?;
        if entries.len() >= limits.entries {
            return Err(PackageValidationError::EntryLimitExceeded);
        }
        let header = entry.header();
        let directory = match header.entry_type().as_byte() {
            0 | b'0' => false,
            b'5' => true,
            _ => return Err(PackageValidationError::UnsupportedEntry),
        };
        if (header.as_ustar().is_none() && header.as_gnu().is_none())
            || header.link_name_bytes().is_some()
            || !canonical_c_string(&header.as_bytes()[..100])
            || (header.as_ustar().is_some() && !canonical_c_string(&header.as_bytes()[345..500]))
        {
            return Err(PackageValidationError::UnsupportedEntry);
        }
        let raw_path = header.path_bytes();
        let path = canonical_path(&raw_path, directory)?;
        if entries.contains_key(&path) || !folded_paths.insert(path.to_lowercase()) {
            return Err(PackageValidationError::DuplicatePath);
        }
        let mode = header
            .mode()
            .map_err(|_| PackageValidationError::InvalidMode)?;
        // 検証後に署名検査などが失敗しても、呼出し元が自身のstagingを安全に
        // 検査・片付けできる通常権限だけを受け付ける。
        if mode & !0o777 != 0
            || (directory && mode & 0o700 != 0o700)
            || (!directory && mode & 0o400 == 0)
        {
            return Err(PackageValidationError::InvalidMode);
        }
        let size = usize::try_from(entry.size())
            .map_err(|_| PackageValidationError::EntrySizeLimitExceeded)?;
        if size > limits.entry || (directory && size != 0) {
            return Err(PackageValidationError::EntrySizeLimitExceeded);
        }
        let offset = usize::try_from(entry.raw_file_position())
            .map_err(|_| PackageValidationError::InvalidTar)?;
        let end = offset
            .checked_add(size)
            .ok_or(PackageValidationError::InvalidTar)?;
        let padded_end = offset
            .checked_add(
                size.checked_add(511)
                    .ok_or(PackageValidationError::InvalidTar)?
                    & !511,
            )
            .ok_or(PackageValidationError::InvalidTar)?;
        if entry.raw_header_position() != next_header as u64
            || offset != next_header + 512
            || padded_end > tar_bytes.len()
            || tar_bytes[end..padded_end].iter().any(|byte| *byte != 0)
        {
            return Err(PackageValidationError::InvalidTar);
        }
        next_header = padded_end;
        entries.insert(
            path.clone(),
            PackageEntry {
                path,
                directory,
                mode,
                size,
                offset,
                sha256: hash(&tar_bytes[offset..end]),
            },
        );
    }
    // tar crateはEOFや最初のzero blockでも終了するため、終端2blockを独立に検査する。
    let tail = tar_bytes
        .get(next_header..)
        .ok_or(PackageValidationError::InvalidTar)?;
    if !tar_bytes.len().is_multiple_of(512)
        || tail.len() < 1024
        || tail.iter().any(|byte| *byte != 0)
    {
        return Err(PackageValidationError::InvalidTar);
    }
    if !entries.get(APP_ROOT).is_some_and(|entry| entry.directory) {
        return Err(PackageValidationError::MissingRequiredEntry);
    }
    for entry in entries.values() {
        let mut child = entry.path.as_str();
        while let Some((parent, _)) = child.rsplit_once('/') {
            if !entries.get(parent).is_some_and(|entry| entry.directory) {
                return Err(PackageValidationError::PathCollision);
            }
            child = parent;
        }
    }
    Ok(entries.into_values().collect())
}

fn canonical_c_string(field: &[u8]) -> bool {
    field
        .iter()
        .position(|byte| *byte == 0)
        .is_none_or(|end| field[end..].iter().all(|byte| *byte == 0))
}

fn canonical_path(bytes: &[u8], directory: bool) -> Result<String, PackageValidationError> {
    let path = std::str::from_utf8(bytes).map_err(|_| PackageValidationError::InvalidPath)?;
    if path.len() > 1024 || path.contains('\\') || path.chars().any(char::is_control) {
        return Err(PackageValidationError::InvalidPath);
    }
    let path = if directory {
        path.strip_suffix('/').unwrap_or(path)
    } else {
        path
    };
    if path.is_empty()
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
        || (path != APP_ROOT && !path.starts_with("kb-app.app/"))
    {
        return Err(PackageValidationError::InvalidPath);
    }
    Ok(path.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;

    const CURRENT: &str = "0.0.1";
    const NEXT: &str = "0.0.2";
    const TARGET: &str = "aarch64-apple-darwin";

    fn plan() -> serde_json::Value {
        serde_json::json!({
            "schema":"kb-app.macos-release-plan/v1", "version":NEXT, "target":TARGET,
            "identifier":"app.kb.desktop", "product_name":"kb-app", "source_commit":"a".repeat(40),
            "compatibility":compiled_metadata(), "compatible_sources":[], "additional_existing_field":true
        })
    }

    fn plist() -> Vec<u8> {
        format!("<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>app.kb.desktop</string><key>CFBundleExecutable</key><string>kb-app</string><key>CFBundleShortVersionString</key><string>{NEXT}</string><key>CFBundleVersion</key><string>{NEXT}</string></dict></plist>").into_bytes()
    }

    fn append(
        builder: &mut tar::Builder<Vec<u8>>,
        path: &str,
        kind: u8,
        mode: u32,
        content: &[u8],
    ) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::new(kind));
        header.set_mode(mode);
        header.set_size(content.len() as u64);
        // 不正path fixtureも、Builderのpath正規化を通さず実headerへ入れる。
        let field = &mut header.as_mut_bytes()[..100];
        assert!(path.len() < field.len());
        field[..path.len()].copy_from_slice(path.as_bytes());
        header.set_cksum();
        builder.append(&header, content).unwrap();
    }

    fn fixture(
        plan: &serde_json::Value,
        info: &[u8],
        extra: impl FnOnce(&mut tar::Builder<Vec<u8>>),
    ) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for path in [
            "kb-app.app/",
            "kb-app.app/Contents/",
            "kb-app.app/Contents/MacOS/",
            "kb-app.app/Contents/Resources/",
            "kb-app.app/Contents/Resources/release/",
        ] {
            append(&mut builder, path, b'5', 0o755, &[]);
        }
        append(
            &mut builder,
            EXECUTABLE,
            b'0',
            0o755,
            b"synthetic executable",
        );
        append(&mut builder, INFO_PLIST, b'0', 0o644, info);
        append(
            &mut builder,
            PLAN,
            b'0',
            0o644,
            &serde_json::to_vec(plan).unwrap(),
        );
        extra(&mut builder);
        builder.into_inner().unwrap()
    }

    fn gzip(tar: &[u8]) -> Vec<u8> {
        let mut writer = GzEncoder::new(Vec::new(), Compression::fast());
        writer.write_all(tar).unwrap();
        writer.finish().unwrap()
    }

    fn check(tar: &[u8]) -> Result<ValidatedPackage, PackageValidationError> {
        validate_macos_package(gzip(tar), CURRENT, NEXT, TARGET)
    }

    #[test]
    #[cfg(unix)]
    fn valid_owned_package_extracts_only_into_empty_staging_and_preserves_modes() {
        let source = fixture(&plan(), &plist(), |_| {});
        let package = check(&source).unwrap();
        assert_eq!(package.version(), NEXT);
        assert_eq!(package.target(), TARGET);
        assert_eq!(package.source_commit(), "a".repeat(40));
        assert_eq!(package.archive_sha256(), hash(&gzip(&source)));
        assert!(!package.supports_source(&"a".repeat(64), &"b".repeat(64)));
        let temp = tempfile::tempdir().unwrap();
        let app = package.extract_into(temp.path()).unwrap();
        assert_eq!(
            fs::read(app.join("Contents/MacOS/kb-app")).unwrap(),
            b"synthetic executable"
        );
        assert_eq!(
            hash(&fs::read(app.join("Contents/Resources/release/plan.json")).unwrap()),
            package.plan_sha256()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(app.join("Contents/MacOS/kb-app"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
        }
        assert_eq!(
            package.extract_into(temp.path()).unwrap_err(),
            PackageValidationError::StagingNotEmpty
        );
    }

    #[test]
    fn traversal_absolute_multiple_root_and_ambiguous_paths_are_rejected() {
        for path in [
            "/tmp/out",
            "../out",
            "kb-app.app/../out",
            "other.app/file",
            "kb-app.app//file",
            "kb-app.app/./file",
            "kb-app.app/Contents\\outside",
            "kb-app.app/Contents/file/",
        ] {
            let tar = fixture(&plan(), &plist(), |builder| {
                append(builder, path, b'0', 0o644, b"x")
            });
            assert_eq!(
                check(&tar).unwrap_err(),
                PackageValidationError::InvalidPath,
                "{path}"
            );
        }
    }

    #[test]
    fn links_pax_gnu_extensions_devices_and_privileged_modes_are_rejected() {
        for kind in *b"123467xgLKS" {
            let tar = fixture(&plan(), &plist(), |builder| {
                append(builder, "kb-app.app/extra", kind, 0o644, &[])
            });
            assert_eq!(
                check(&tar).unwrap_err(),
                PackageValidationError::UnsupportedEntry
            );
        }
        for mode in [0o4755, 0o2755, 0o1755, 0o000, 0o311] {
            let tar = fixture(&plan(), &plist(), |builder| {
                append(builder, "kb-app.app/extra", b'0', mode, b"x")
            });
            assert_eq!(
                check(&tar).unwrap_err(),
                PackageValidationError::InvalidMode
            );
        }
        // 2026-09-09: 作成後にownerが走査・削除できないdirectoryを残さない。
        for mode in [0o000, 0o500, 0o555] {
            let tar = fixture(&plan(), &plist(), |builder| {
                append(builder, "kb-app.app/extra", b'5', mode, &[])
            });
            assert_eq!(
                check(&tar).unwrap_err(),
                PackageValidationError::InvalidMode
            );
        }
    }

    #[test]
    fn duplicate_and_file_ancestor_collisions_are_rejected_in_both_orders() {
        for path in [
            EXECUTABLE,
            "kb-app.app/Contents/MacOS/KB-APP",
            "kb-app.app/Contents",
        ] {
            let tar = fixture(&plan(), &plist(), |builder| {
                append(builder, path, b'0', 0o644, b"x")
            });
            assert_eq!(
                check(&tar).unwrap_err(),
                PackageValidationError::DuplicatePath
            );
        }
        for reverse in [false, true] {
            let tar = fixture(&plan(), &plist(), |builder| {
                let paths = if reverse {
                    ["kb-app.app/collision/file", "kb-app.app/collision"]
                } else {
                    ["kb-app.app/collision", "kb-app.app/collision/file"]
                };
                for path in paths {
                    append(builder, path, b'0', 0o644, b"x");
                }
            });
            assert_eq!(
                check(&tar).unwrap_err(),
                PackageValidationError::PathCollision
            );
        }
    }

    #[test]
    fn truncated_corrupt_tar_and_missing_end_blocks_are_rejected() {
        let valid = fixture(&plan(), &plist(), |_| {});
        let mut corrupt = valid.clone();
        corrupt[0] ^= 1;
        assert_eq!(
            check(&corrupt).unwrap_err(),
            PackageValidationError::InvalidTar
        );
        for length in [100, valid.len() - 1024, valid.len() - 1] {
            assert_eq!(
                check(&valid[..length]).unwrap_err(),
                PackageValidationError::InvalidTar
            );
        }
        let mut trailing = valid.clone();
        trailing.extend_from_slice(&valid);
        assert_eq!(
            check(&trailing).unwrap_err(),
            PackageValidationError::InvalidTar
        );
        let compressed = gzip(&valid);
        assert_eq!(
            validate_macos_package(
                compressed[..compressed.len() - 4].to_vec(),
                CURRENT,
                NEXT,
                TARGET
            )
            .unwrap_err(),
            PackageValidationError::InvalidGzip
        );
        for suffix in [vec![0], gzip(b"second member")] {
            let mut multiple = compressed.clone();
            multiple.extend(suffix);
            assert_eq!(
                validate_macos_package(multiple, CURRENT, NEXT, TARGET).unwrap_err(),
                PackageValidationError::GzipTrailingData
            );
        }
    }

    #[test]
    fn package_dimensions_and_source_allowlist_fail_closed() {
        for (key, value, error) in [
            (
                "schema",
                serde_json::json!("future"),
                PackageValidationError::UnsupportedPlan,
            ),
            (
                "version",
                serde_json::json!("0.0.3"),
                PackageValidationError::VersionMismatch,
            ),
            (
                "target",
                serde_json::json!("x86_64-apple-darwin"),
                PackageValidationError::TargetMismatch,
            ),
            (
                "identifier",
                serde_json::json!("another.app"),
                PackageValidationError::ProductMismatch,
            ),
            (
                "product_name",
                serde_json::json!("other"),
                PackageValidationError::ProductMismatch,
            ),
            (
                "compatible_sources",
                serde_json::json!([{"plan_sha256":"bad", "executable_sha256":"b".repeat(64)}]),
                PackageValidationError::InvalidCompatibleSources,
            ),
        ] {
            let mut changed = plan();
            changed[key] = value;
            assert_eq!(
                check(&fixture(&changed, &plist(), |_| {})).unwrap_err(),
                error
            );
        }
        for key in ["compatibility", "compatible_sources", "source_commit"] {
            let mut missing = plan();
            missing.as_object_mut().unwrap().remove(key);
            assert_eq!(
                check(&fixture(&missing, &plist(), |_| {})).unwrap_err(),
                PackageValidationError::InvalidPlan
            );
        }
        for key in ["database_schema", "persistent_compatibility_epoch"] {
            let mut changed = plan();
            changed["compatibility"][key] = serde_json::json!(999);
            assert_eq!(
                check(&fixture(&changed, &plist(), |_| {})).unwrap_err(),
                PackageValidationError::CompatibilityMismatch
            );
        }
        let mut supported = plan();
        let source =
            serde_json::json!({"plan_sha256":"a".repeat(64),"executable_sha256":"b".repeat(64)});
        supported["compatible_sources"] = serde_json::json!([source]);
        let package = check(&fixture(&supported, &plist(), |_| {})).unwrap();
        assert!(package.supports_source(&"a".repeat(64), &"b".repeat(64)));
        assert!(!package.supports_source(&"a".repeat(64), &"c".repeat(64)));
        supported["compatible_sources"] = serde_json::json!([source, source]);
        assert_eq!(
            check(&fixture(&supported, &plist(), |_| {})).unwrap_err(),
            PackageValidationError::InvalidCompatibleSources
        );
    }

    #[test]
    fn version_is_newer_by_semver_precedence_not_only_build_metadata() {
        let archive = gzip(&fixture(&plan(), &plist(), |_| {}));
        for current in [NEXT, "0.0.3"] {
            assert_eq!(
                validate_macos_package(archive.clone(), current, NEXT, TARGET).unwrap_err(),
                PackageValidationError::VersionNotNewer
            );
        }
        assert_eq!(
            validate_macos_package(archive, "0.0.2+one", "0.0.2+two", TARGET).unwrap_err(),
            PackageValidationError::VersionNotNewer
        );
    }

    #[test]
    fn plist_identity_and_duplicate_fields_are_not_accepted() {
        let wrong = String::from_utf8(plist())
            .unwrap()
            .replace("app.kb.desktop", "another.app");
        assert_eq!(
            check(&fixture(&plan(), wrong.as_bytes(), |_| {})).unwrap_err(),
            PackageValidationError::PlistMismatch
        );
        let duplicate = String::from_utf8(plist()).unwrap().replace(
            "</dict>",
            "<key>CFBundleExecutable</key><string>kb-app</string></dict>",
        );
        assert_eq!(
            check(&fixture(&plan(), duplicate.as_bytes(), |_| {})).unwrap_err(),
            PackageValidationError::InvalidPlist
        );
        assert_eq!(
            check(&fixture(&plan(), b"not a plist", |_| {})).unwrap_err(),
            PackageValidationError::InvalidPlist
        );
        let nested = String::from_utf8(plist()).unwrap().replace(
            "</dict>",
            &format!(
                "<key>unknown</key>{}<string>x</string>{}</dict>",
                "<array>".repeat(300),
                "</array>".repeat(300)
            ),
        );
        assert_eq!(
            check(&fixture(&plan(), nested.as_bytes(), |_| {})).unwrap_err(),
            PackageValidationError::InvalidPlist
        );
    }

    #[test]
    fn every_allocation_and_entry_count_has_a_finite_limit() {
        let tar = fixture(&plan(), &plist(), |_| {});
        let archive = gzip(&tar);
        for (limits, expected) in [
            (
                Limits {
                    compressed: archive.len() - 1,
                    ..LIMITS
                },
                PackageValidationError::CompressedLimitExceeded,
            ),
            (
                Limits {
                    decompressed: tar.len() - 1,
                    ..LIMITS
                },
                PackageValidationError::DecompressedLimitExceeded,
            ),
            (
                Limits {
                    entries: 7,
                    ..LIMITS
                },
                PackageValidationError::EntryLimitExceeded,
            ),
            (
                Limits {
                    entry: 10,
                    ..LIMITS
                },
                PackageValidationError::EntrySizeLimitExceeded,
            ),
            (
                Limits { plan: 10, ..LIMITS },
                PackageValidationError::EntrySizeLimitExceeded,
            ),
            (
                Limits {
                    plist: 10,
                    ..LIMITS
                },
                PackageValidationError::EntrySizeLimitExceeded,
            ),
        ] {
            assert_eq!(
                validate_with_limits(archive.clone(), CURRENT, NEXT, TARGET, limits).unwrap_err(),
                expected
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_staging_and_preexisting_content_are_preserved() {
        use std::os::unix::fs::symlink;
        let package = check(&fixture(&plan(), &plist(), |_| {})).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        fs::create_dir(&target).unwrap();
        let alias = temp.path().join("alias");
        symlink(&target, &alias).unwrap();
        assert_eq!(
            package.extract_into(&alias).unwrap_err(),
            PackageValidationError::UnsafeStaging
        );
        fs::write(target.join("existing"), b"keep").unwrap();
        assert_eq!(
            package.extract_into(&target).unwrap_err(),
            PackageValidationError::StagingNotEmpty
        );
        assert_eq!(fs::read(target.join("existing")).unwrap(), b"keep");
    }
}
