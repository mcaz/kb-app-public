//! Storage Contract backend 比較 PoC。
//!
//! 同じ論理 Note を file-per-note、SQLite 正本、append-only event log + 派生 snapshotへ
//! 保存し、Git commit / fresh clone / cold rebuild / point read / export を同条件で測る。
//!
//! 実行: `cargo run --release -- 1000 10000`
//! 反復数: `KB_STORAGE_POC_RUNS=3`（既定3）

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::hint::black_box;
use std::io::{BufRead, BufReader, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const POINT_READS: usize = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Note {
    id: String,
    title: String,
    tags: Vec<String>,
    body: String,
    version: u64,
    provenance: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredFront {
    id: String,
    title: String,
    tags: Vec<String>,
    version: u64,
    provenance: String,
}

impl From<&Note> for StoredFront {
    fn from(note: &Note) -> Self {
        Self {
            id: note.id.clone(),
            title: note.title.clone(),
            tags: note.tags.clone(),
            version: note.version,
            provenance: note.provenance.clone(),
        }
    }
}

impl StoredFront {
    fn with_body(self, body: String) -> Note {
        Note {
            id: self.id,
            title: self.title,
            tags: self.tags,
            body,
            version: self.version,
            provenance: self.provenance,
        }
    }
}

trait Backend: Sized {
    const NAME: &'static str;

    fn create(root: &Path) -> Result<Self>;
    fn open(root: &Path) -> Result<Self>;
    fn put_many(&mut self, notes: &[Note]) -> Result<()>;
    fn get(&self, id: &str) -> Result<Option<Note>>;
    fn export(&self) -> Result<Vec<Note>>;
    fn authority_stats(&self) -> Result<(u64, usize)>;
    fn derived_bytes(&self) -> Result<u64> {
        Ok(0)
    }
    fn integrity_check(&self) -> Result<()>;
    fn inject_corruption(root: &Path) -> Result<()>;
    fn set_writer(&mut self, _writer: &str) -> Result<()> {
        Ok(())
    }
}

struct MarkdownBackend {
    root: PathBuf,
}

impl MarkdownBackend {
    fn path(&self, id: &str) -> PathBuf {
        self.root.join("notes").join(format!("{id}.md"))
    }

    fn encode(note: &Note) -> Result<String> {
        let front = serde_yaml::to_string(&StoredFront::from(note))?;
        Ok(format!("---\n{front}---\n\n{}", note.body))
    }

    fn decode(text: &str) -> Result<Note> {
        let rest = text.strip_prefix("---\n").context("frontmatter start")?;
        let (yaml, body) = rest.split_once("---\n\n").context("frontmatter end")?;
        let front: StoredFront = serde_yaml::from_str(yaml)?;
        Ok(front.with_body(body.to_string()))
    }
}

impl Backend for MarkdownBackend {
    const NAME: &'static str = "file-per-note";

    fn create(root: &Path) -> Result<Self> {
        fs::create_dir_all(root.join("notes"))?;
        Ok(Self { root: root.into() })
    }

    fn open(root: &Path) -> Result<Self> {
        Ok(Self { root: root.into() })
    }

    fn put_many(&mut self, notes: &[Note]) -> Result<()> {
        for note in notes {
            fs::write(self.path(&note.id), Self::encode(note)?)?;
        }
        Ok(())
    }

    fn get(&self, id: &str) -> Result<Option<Note>> {
        match fs::read_to_string(self.path(id)) {
            Ok(text) => Ok(Some(Self::decode(&text)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn export(&self) -> Result<Vec<Note>> {
        let mut paths: Vec<PathBuf> = fs::read_dir(self.root.join("notes"))?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<std::io::Result<_>>()?;
        paths.sort();
        paths
            .into_iter()
            .map(|path| Self::decode(&fs::read_to_string(path)?))
            .collect()
    }

    fn authority_stats(&self) -> Result<(u64, usize)> {
        dir_stats(&self.root.join("notes"))
    }

    fn integrity_check(&self) -> Result<()> {
        let exported = self.export()?;
        if exported.windows(2).any(|pair| pair[0].id >= pair[1].id) {
            bail!("note ID order/uniqueness failed");
        }
        Ok(())
    }

    fn inject_corruption(root: &Path) -> Result<()> {
        let first = fs::read_dir(root.join("notes"))?
            .next()
            .context("note fixture missing")??
            .path();
        fs::write(first, "broken note")?;
        Ok(())
    }
}

struct SqliteBackend {
    root: PathBuf,
    conn: Connection,
}

impl SqliteBackend {
    fn db_path(root: &Path) -> PathBuf {
        root.join("knowledge.db")
    }

    fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Note> {
        let tags_json: String = row.get(2)?;
        Ok(Note {
            id: row.get(0)?,
            title: row.get(1)?,
            tags: serde_json::from_str(&tags_json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    tags_json.len(),
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
            body: row.get(3)?,
            version: row.get::<_, i64>(4)? as u64,
            provenance: row.get(5)?,
        })
    }
}

impl Backend for SqliteBackend {
    const NAME: &'static str = "sqlite-canonical";

    fn create(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)?;
        let conn = Connection::open(Self::db_path(root))?;
        conn.execute_batch(
            "PRAGMA journal_mode=DELETE;
             PRAGMA synchronous=FULL;
             CREATE TABLE notes(
               id TEXT PRIMARY KEY,
               title TEXT NOT NULL,
               tags_json TEXT NOT NULL,
               body TEXT NOT NULL,
               version INTEGER NOT NULL,
               provenance TEXT NOT NULL
             ) WITHOUT ROWID;",
        )?;
        Ok(Self {
            root: root.into(),
            conn,
        })
    }

    fn open(root: &Path) -> Result<Self> {
        Ok(Self {
            root: root.into(),
            conn: Connection::open(Self::db_path(root))?,
        })
    }

    fn put_many(&mut self, notes: &[Note]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO notes(id,title,tags_json,body,version,provenance)
                 VALUES(?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(id) DO UPDATE SET
                   title=excluded.title,
                   tags_json=excluded.tags_json,
                   body=excluded.body,
                   version=excluded.version,
                   provenance=excluded.provenance",
            )?;
            for note in notes {
                stmt.execute(params![
                    note.id,
                    note.title,
                    serde_json::to_string(&note.tags)?,
                    note.body,
                    note.version as i64,
                    note.provenance
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn get(&self, id: &str) -> Result<Option<Note>> {
        let result = self.conn.query_row(
            "SELECT id,title,tags_json,body,version,provenance FROM notes WHERE id=?1",
            [id],
            Self::read_row,
        );
        match result {
            Ok(note) => Ok(Some(note)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn export(&self) -> Result<Vec<Note>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id,title,tags_json,body,version,provenance FROM notes ORDER BY id")?;
        Ok(stmt
            .query_map([], Self::read_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    fn authority_stats(&self) -> Result<(u64, usize)> {
        Ok((fs::metadata(Self::db_path(&self.root))?.len(), 1))
    }

    fn integrity_check(&self) -> Result<()> {
        let result: String = self
            .conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if result != "ok" {
            bail!("SQLite integrity_check: {result}");
        }
        Ok(())
    }

    fn inject_corruption(root: &Path) -> Result<()> {
        let mut file = OpenOptions::new().write(true).open(Self::db_path(root))?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(b"BROKEN!!")?;
        file.flush()?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    Put { note: Note },
}

#[derive(Debug)]
struct EventLogBackend {
    root: PathBuf,
    notes: BTreeMap<String, Note>,
    writer: String,
}

impl EventLogBackend {
    fn events_dir(root: &Path) -> PathBuf {
        root.join("events")
    }

    fn log_path(&self) -> PathBuf {
        Self::events_dir(&self.root).join(format!("{}.ndjson", self.writer))
    }

    fn snapshot_path(&self) -> PathBuf {
        self.root.join("snapshot.json")
    }

    fn write_derived_snapshot(&self) -> Result<()> {
        fs::write(
            self.snapshot_path(),
            serde_json::to_vec(&self.notes.values().collect::<Vec<_>>())?,
        )?;
        Ok(())
    }
}

impl Backend for EventLogBackend {
    const NAME: &'static str = "segmented-event-log";

    fn create(root: &Path) -> Result<Self> {
        fs::create_dir_all(Self::events_dir(root))?;
        fs::write(root.join(".gitignore"), "snapshot.json\n")?;
        let backend = Self {
            root: root.into(),
            notes: BTreeMap::new(),
            writer: "000000-bootstrap".into(),
        };
        File::create(backend.log_path())?;
        Ok(backend)
    }

    fn open(root: &Path) -> Result<Self> {
        let mut paths: Vec<PathBuf> = fs::read_dir(Self::events_dir(root))?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<std::io::Result<_>>()?;
        paths.sort();
        let mut notes = BTreeMap::new();
        for path in paths {
            if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("ndjson") {
                bail!("unknown event segment: {}", path.display());
            }
            let file = File::open(&path)?;
            for (index, line) in BufReader::new(file).lines().enumerate() {
                let line = line?;
                let event: Event = serde_json::from_str(&line)
                    .with_context(|| format!("event {}:{} is broken", path.display(), index + 1))?;
                match event {
                    Event::Put { note } => {
                        validate_next_version(&notes, &note)?;
                        notes.insert(note.id.clone(), note);
                    }
                }
            }
        }
        let backend = Self {
            root: root.into(),
            notes,
            writer: "100000-local".into(),
        };
        backend.write_derived_snapshot()?;
        Ok(backend)
    }

    fn put_many(&mut self, notes: &[Note]) -> Result<()> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path())?;
        let mut writer = BufWriter::new(file);
        for note in notes {
            validate_next_version(&self.notes, note)?;
            serde_json::to_writer(&mut writer, &Event::Put { note: note.clone() })?;
            writer.write_all(b"\n")?;
            self.notes.insert(note.id.clone(), note.clone());
        }
        writer.flush()?;
        self.write_derived_snapshot()?;
        Ok(())
    }

    fn get(&self, id: &str) -> Result<Option<Note>> {
        Ok(self.notes.get(id).cloned())
    }

    fn export(&self) -> Result<Vec<Note>> {
        Ok(self.notes.values().cloned().collect())
    }

    fn authority_stats(&self) -> Result<(u64, usize)> {
        dir_stats(&Self::events_dir(&self.root))
    }

    fn derived_bytes(&self) -> Result<u64> {
        Ok(fs::metadata(self.snapshot_path())?.len())
    }

    fn integrity_check(&self) -> Result<()> {
        let replayed = Self::open(&self.root)?;
        if replayed.notes != self.notes {
            bail!("event replay mismatch");
        }
        Ok(())
    }

    fn inject_corruption(root: &Path) -> Result<()> {
        let mut paths: Vec<PathBuf> = fs::read_dir(Self::events_dir(root))?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<std::io::Result<_>>()?;
        paths.sort();
        let path = paths.into_iter().next().context("event fixture missing")?;
        let mut file = OpenOptions::new().append(true).open(path)?;
        file.write_all(b"{broken event\n")?;
        file.flush()?;
        Ok(())
    }

    fn set_writer(&mut self, writer: &str) -> Result<()> {
        if writer.is_empty()
            || !writer
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            bail!("invalid event writer: {writer}");
        }
        self.writer = writer.to_string();
        Ok(())
    }
}

fn validate_next_version(notes: &BTreeMap<String, Note>, next: &Note) -> Result<()> {
    let expected = notes.get(&next.id).map_or(1, |current| current.version + 1);
    if next.version != expected {
        bail!(
            "causal conflict: {} expected version {}, got {}",
            next.id,
            expected,
            next.version
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
struct Metrics {
    backend: String,
    notes: usize,
    updates: usize,
    initial_write_ms: u128,
    initial_commit_ms: u128,
    update_ms: u128,
    update_commit_ms: u128,
    point_reads_ms: u128,
    export_ms: u128,
    clone_ms: u128,
    clone_cold_rebuild_ms: u128,
    authority_bytes: u64,
    authority_files: usize,
    derived_bytes: u64,
    git_bytes: u64,
    clone_digest_match: bool,
    corruption_detected: bool,
    disjoint_git_merge_preserved: bool,
}

#[derive(Debug, Serialize)]
struct Report {
    schema: &'static str,
    environment: String,
    runs: usize,
    point_reads: usize,
    results: Vec<Metrics>,
}

fn benchmark<B: Backend>(parent: &Path, notes: &[Note], run: usize) -> Result<Metrics> {
    let root = parent.join(format!("{}-{run}", B::NAME));
    let clone_root = parent.join(format!("{}-{run}-clone", B::NAME));
    let mut backend = B::create(&root)?;
    git(&root, &["init", "-q"])?;

    let started = Instant::now();
    backend.put_many(notes)?;
    let initial_write_ms = started.elapsed().as_millis();
    let started = Instant::now();
    git_commit(&root, "initial")?;
    let initial_commit_ms = started.elapsed().as_millis();

    let mut updates: Vec<Note> = notes.iter().step_by(10).cloned().collect();
    for note in &mut updates {
        note.version += 1;
        note.body
            .push_str("\n追記イベント: Storage Contract の更新を反映。");
    }
    let started = Instant::now();
    backend.put_many(&updates)?;
    let update_ms = started.elapsed().as_millis();
    let started = Instant::now();
    git_commit(&root, "updates")?;
    let update_commit_ms = started.elapsed().as_millis();

    backend.integrity_check()?;
    let expected = logical_digest(&backend.export()?)?;
    let started = Instant::now();
    for index in 0..POINT_READS {
        let id = &notes[(index * 7919) % notes.len()].id;
        black_box(backend.get(id)?.context("point read missed")?);
    }
    let point_reads_ms = started.elapsed().as_millis();
    let started = Instant::now();
    black_box(backend.export()?);
    let export_ms = started.elapsed().as_millis();
    let (authority_bytes, authority_files) = backend.authority_stats()?;
    let derived_bytes = backend.derived_bytes()?;
    drop(backend);

    let started = Instant::now();
    git_clone(&root, &clone_root)?;
    let clone_ms = started.elapsed().as_millis();
    let started = Instant::now();
    let clone = B::open(&clone_root)?;
    clone.integrity_check()?;
    let cloned_digest = logical_digest(&clone.export()?)?;
    let clone_cold_rebuild_ms = started.elapsed().as_millis();
    drop(clone);
    B::inject_corruption(&clone_root)?;
    let corruption_detected = match B::open(&clone_root) {
        Ok(corrupted) => corrupted.integrity_check().is_err(),
        Err(_) => true,
    };
    let git_bytes = dir_stats(&root.join(".git"))?.0;
    let disjoint_git_merge_preserved = concurrency_acceptance::<B>(parent, notes, run)?;

    Ok(Metrics {
        backend: B::NAME.into(),
        notes: notes.len(),
        updates: updates.len(),
        initial_write_ms,
        initial_commit_ms,
        update_ms,
        update_commit_ms,
        point_reads_ms,
        export_ms,
        clone_ms,
        clone_cold_rebuild_ms,
        authority_bytes,
        authority_files,
        derived_bytes,
        git_bytes,
        clone_digest_match: cloned_digest == expected,
        corruption_detected,
        disjoint_git_merge_preserved,
    })
}

fn concurrency_acceptance<B: Backend>(parent: &Path, notes: &[Note], run: usize) -> Result<bool> {
    let source_root = parent.join(format!("{}-{run}-concurrency", B::NAME));
    let left_root = parent.join(format!("{}-{run}-left", B::NAME));
    let right_root = parent.join(format!("{}-{run}-right", B::NAME));
    let mut source = B::create(&source_root)?;
    git(&source_root, &["init", "-q"])?;
    source.put_many(&notes[..notes.len().min(100)])?;
    git_commit(&source_root, "base")?;
    drop(source);

    git_clone(&source_root, &left_root)?;
    git_clone(&source_root, &right_root)?;

    let mut left_note = notes[0].clone();
    left_note.version += 1;
    left_note.body.push_str("\nleft device update");
    let mut left = B::open(&left_root)?;
    left.set_writer("100000-left")?;
    left.put_many(&[left_note])?;
    drop(left);
    git_commit(&left_root, "left")?;

    let mut right_note = notes[1].clone();
    right_note.version += 1;
    right_note.body.push_str("\nright device update");
    let mut right_backend = B::open(&right_root)?;
    right_backend.set_writer("100000-right")?;
    right_backend.put_many(&[right_note])?;
    drop(right_backend);
    git_commit(&right_root, "right")?;

    let right = right_root.to_string_lossy();
    git(
        &left_root,
        &["fetch", "-q", &right, "HEAD:refs/remotes/right/head"],
    )?;
    let merge = Command::new("git")
        .args([
            "-c",
            "user.name=kb-app-poc",
            "-c",
            "user.email=kb-app@localhost",
            "merge",
            "-q",
            "--no-edit",
            "refs/remotes/right/head",
        ])
        .current_dir(&left_root)
        .output()?;
    if !merge.status.success() {
        return Ok(false);
    }
    let merged = B::open(&left_root)?;
    let left_version = merged.get(&notes[0].id)?.map(|note| note.version);
    let right_version = merged.get(&notes[1].id)?.map(|note| note.version);
    Ok(left_version == Some(2) && right_version == Some(2))
}

fn fixture(count: usize) -> Vec<Note> {
    const WORDS: &[&str] = &[
        "知識",
        "検索",
        "設計",
        "復元",
        "監査",
        "出典",
        "関連",
        "判断",
        "実装",
        "検証",
        "migration",
        "storage",
        "contract",
        "artifact",
        "repository",
        "context",
    ];
    let mut seed = 0x6a09_e667_f3bc_c909_u64;
    (0..count)
        .map(|index| {
            let mut body = String::new();
            for word_index in 0..96 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let word = WORDS[(seed as usize) % WORDS.len()];
                if word_index > 0 {
                    body.push(' ');
                }
                body.push_str(word);
            }
            body.push_str(&format!("\n固有の検証番号: {index:08x}-{seed:016x}"));
            Note {
                id: format!("note-{index:06}"),
                title: format!("Storage Contract fixture {index:06}"),
                tags: vec!["knowledge-base".into(), "design".into()],
                body,
                version: 1,
                provenance: format!("fixture:{index:06}"),
            }
        })
        .collect()
}

fn logical_digest(notes: &[Note]) -> Result<String> {
    let mut sorted = notes.to_vec();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    let digest = Sha256::digest(serde_json::to_vec(&sorted)?);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn git(root: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    if !output.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn git_commit(root: &Path, message: &str) -> Result<()> {
    git(root, &["add", "-A"])?;
    git(
        root,
        &[
            "-c",
            "user.name=kb-app-poc",
            "-c",
            "user.email=kb-app@localhost",
            "commit",
            "-q",
            "-m",
            message,
        ],
    )
}

fn git_clone(source: &Path, dest: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["clone", "-q", "--no-local"])
        .arg(source)
        .arg(dest)
        .output()?;
    if !output.status.success() {
        bail!(
            "git clone: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn dir_stats(path: &Path) -> Result<(u64, usize)> {
    let mut bytes = 0;
    let mut files = 0;
    if !path.exists() {
        return Ok((0, 0));
    }
    for entry in walkdir::WalkDir::new(path) {
        let entry = entry?;
        if entry.file_type().is_file() {
            bytes += entry.metadata()?.len();
            files += 1;
        }
    }
    Ok((bytes, files))
}

fn median_u128(values: impl Iterator<Item = u128>) -> u128 {
    let mut values: Vec<u128> = values.collect();
    values.sort_unstable();
    values[values.len() / 2]
}

fn median_u64(values: impl Iterator<Item = u64>) -> u64 {
    let mut values: Vec<u64> = values.collect();
    values.sort_unstable();
    values[values.len() / 2]
}

fn aggregate(runs: &[Metrics]) -> Metrics {
    let first = &runs[0];
    Metrics {
        backend: first.backend.clone(),
        notes: first.notes,
        updates: first.updates,
        initial_write_ms: median_u128(runs.iter().map(|m| m.initial_write_ms)),
        initial_commit_ms: median_u128(runs.iter().map(|m| m.initial_commit_ms)),
        update_ms: median_u128(runs.iter().map(|m| m.update_ms)),
        update_commit_ms: median_u128(runs.iter().map(|m| m.update_commit_ms)),
        point_reads_ms: median_u128(runs.iter().map(|m| m.point_reads_ms)),
        export_ms: median_u128(runs.iter().map(|m| m.export_ms)),
        clone_ms: median_u128(runs.iter().map(|m| m.clone_ms)),
        clone_cold_rebuild_ms: median_u128(runs.iter().map(|m| m.clone_cold_rebuild_ms)),
        authority_bytes: median_u64(runs.iter().map(|m| m.authority_bytes)),
        authority_files: first.authority_files,
        derived_bytes: median_u64(runs.iter().map(|m| m.derived_bytes)),
        git_bytes: median_u64(runs.iter().map(|m| m.git_bytes)),
        clone_digest_match: runs.iter().all(|m| m.clone_digest_match),
        corruption_detected: runs.iter().all(|m| m.corruption_detected),
        disjoint_git_merge_preserved: runs.iter().all(|m| m.disjoint_git_merge_preserved),
    }
}

fn main() -> Result<()> {
    let counts: Vec<usize> = {
        let parsed: Result<Vec<_>, _> = std::env::args().skip(1).map(|s| s.parse()).collect();
        let parsed = parsed.context("note count must be an integer")?;
        if parsed.is_empty() {
            vec![1_000, 10_000]
        } else {
            parsed
        }
    };
    let runs = std::env::var("KB_STORAGE_POC_RUNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    if runs == 0 || counts.contains(&0) {
        bail!("runs and note counts must be positive");
    }

    let environment = Command::new("rustc")
        .arg("--version")
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_else(|_| "rustc unknown".into());
    let temp = TempDir::new()?;
    let mut results = Vec::new();
    for count in counts {
        let notes = fixture(count);
        let expected = logical_digest(&notes)?;
        eprintln!("fixture: {count} notes, digest={expected}");
        let case_root = temp.path().join(format!("notes-{count}"));
        fs::create_dir_all(&case_root)?;

        let mut markdown = Vec::new();
        let mut sqlite = Vec::new();
        let mut event_log = Vec::new();
        for run in 0..runs {
            eprintln!("  run {}/{runs}", run + 1);
            markdown.push(benchmark::<MarkdownBackend>(&case_root, &notes, run)?);
            sqlite.push(benchmark::<SqliteBackend>(&case_root, &notes, run)?);
            event_log.push(benchmark::<EventLogBackend>(&case_root, &notes, run)?);
        }
        results.push(aggregate(&markdown));
        results.push(aggregate(&sqlite));
        results.push(aggregate(&event_log));
    }

    let report = Report {
        schema: "kb-app.storage-backends-poc/v1",
        environment,
        runs,
        point_reads: POINT_READS,
        results,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    if report
        .results
        .iter()
        .any(|m| !m.clone_digest_match || !m.corruption_detected)
    {
        bail!("Storage Contract acceptance failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_notes() -> Vec<Note> {
        fixture(2)
    }

    #[test]
    fn segmented_log_preserves_disjoint_device_updates() {
        let temp = TempDir::new().unwrap();
        let notes = two_notes();
        let mut base = EventLogBackend::create(temp.path()).unwrap();
        base.put_many(&notes).unwrap();
        drop(base);

        let mut left = EventLogBackend::open(temp.path()).unwrap();
        let mut right = EventLogBackend::open(temp.path()).unwrap();
        left.set_writer("100000-left").unwrap();
        right.set_writer("100000-right").unwrap();
        let mut left_note = notes[0].clone();
        left_note.version = 2;
        let mut right_note = notes[1].clone();
        right_note.version = 2;
        left.put_many(&[left_note]).unwrap();
        right.put_many(&[right_note]).unwrap();

        let merged = EventLogBackend::open(temp.path()).unwrap();
        assert_eq!(merged.get(&notes[0].id).unwrap().unwrap().version, 2);
        assert_eq!(merged.get(&notes[1].id).unwrap().unwrap().version, 2);
    }

    #[test]
    fn same_base_version_is_a_causal_conflict_not_last_writer_wins() {
        let temp = TempDir::new().unwrap();
        let notes = fixture(1);
        let mut base = EventLogBackend::create(temp.path()).unwrap();
        base.put_many(&notes).unwrap();
        drop(base);

        let mut left = EventLogBackend::open(temp.path()).unwrap();
        let mut right = EventLogBackend::open(temp.path()).unwrap();
        left.set_writer("100000-left").unwrap();
        right.set_writer("100000-right").unwrap();
        let mut left_note = notes[0].clone();
        left_note.version = 2;
        left_note.body.push_str(" left");
        let mut right_note = notes[0].clone();
        right_note.version = 2;
        right_note.body.push_str(" right");
        left.put_many(&[left_note]).unwrap();
        right.put_many(&[right_note]).unwrap();

        let error = EventLogBackend::open(temp.path()).unwrap_err().to_string();
        assert!(error.contains("causal conflict"), "{error}");
        assert!(temp.path().join("events/100000-left.ndjson").is_file());
        assert!(temp.path().join("events/100000-right.ndjson").is_file());
    }
}
