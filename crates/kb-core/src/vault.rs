//! vault = 1ディレクトリ(内部は git リポ)。日常の読み書きはローカルSQLite、
//! Markdown+Gitは表示・バックアップ・復元adapter(Storage Contract / ADR-0004)。
//! 規約(frontmatter・index.md・log.md)はアプリが生成し人間に暗記させない(原則7)。

use std::fs;
use std::io::ErrorKind;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use git2::{Repository, Signature};
use rusqlite::OptionalExtension;

#[cfg(test)]
use crate::authority::NoteNamespace;
use crate::authority::{Authority, NoteRelation, NoteUid};
use crate::frontmatter::{Frontmatter, Generated, Note, now_iso, today};
use crate::note_id::NoteId;

pub const NOTES_DIR: &str = "notes";
const RESERVED: &[&str] = &["index.md", "log.md"];

pub struct Vault {
    pub root: PathBuf,
}

/// ノート起票の入力。タグ契約の明示フラグを起票内容と一体で渡す。
pub struct NoteProposal<'a> {
    pub title: &'a str,
    pub body: &'a str,
    pub description: Option<&'a str>,
    pub tags: &'a [String],
    pub authority: Authority,
    pub relations: Vec<NoteRelation>,
    pub allow_new_tags: bool,
    pub client: &'a str,
}

/// ノート更新の入力。`None` の項目は変更しない。
pub struct NoteUpdate<'a> {
    pub id: &'a str,
    pub title: Option<&'a str>,
    pub body: Option<&'a str>,
    pub description: Option<&'a str>,
    pub tags: Option<&'a [String]>,
    pub authority: Option<Authority>,
    pub relations: Option<Vec<NoteRelation>>,
    pub allow_new_tags: bool,
    pub client: &'a str,
}

impl Vault {
    /// 既存 vault を開く(git リポであることを確認)。
    pub fn open(root: impl AsRef<Path>) -> Result<Vault> {
        let root = root.as_ref().to_path_buf();
        if !root.join(".git").exists() {
            bail!("{} は vault ではない(.git がない)", root.display());
        }
        Ok(Vault { root })
    }

    /// vault を新規作成: git init + 永続 ID + .gitignore + index.md + notes/。
    pub fn create(root: impl AsRef<Path>) -> Result<Vault> {
        let root = root.as_ref().to_path_buf();
        if root.join(".git").exists() {
            bail!("{} には既に vault がある", root.display());
        }
        fs::create_dir_all(root.join(NOTES_DIR))?;
        Repository::init(&root).context("git init")?;
        fs::write(root.join(".gitignore"), ".kb/\n")?;
        let vault = Vault { root };
        crate::workspace::initialize_workspace_id(&vault)?;
        vault.write_index_md()?;
        vault.commit(
            &[".gitignore", crate::workspace::ID_FILE, "index.md"],
            "vault: initialize",
        )?;
        Ok(vault)
    }

    pub fn index_db_path(&self) -> PathBuf {
        self.root.join(".kb").join("index.db")
    }

    /// 検証済みノートIDから、Vault内の字句上のパスを作る。
    /// 読み書きは下のchecked_*も通し、symlink経由を拒否する。
    pub(crate) fn note_path(&self, raw: &str) -> Result<PathBuf> {
        let id = NoteId::parse(raw)?;
        Ok(self.root.join(id.markdown_relative_path()))
    }

    fn reject_symlink_components(&self, relative: &Path) -> Result<()> {
        let mut current = self.root.clone();
        for component in relative.components() {
            let std::path::Component::Normal(part) = component else {
                bail!("Vault相対パスに通常成分以外がある");
            };
            current.push(part);
            match fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    bail!("Vault内パスにsymlinkがある: {}", current.display());
                }
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => break,
                Err(error) => return Err(error).context("Vault内パスの検査に失敗"),
            }
        }
        Ok(())
    }

    fn checked_existing_path(&self, relative: &Path) -> Result<PathBuf> {
        self.reject_symlink_components(relative)?;
        let root = self
            .root
            .canonicalize()
            .context("Vault rootを解決できない")?;
        let path = self.root.join(relative);
        let canonical = path
            .canonicalize()
            .with_context(|| format!("Vault内パスを解決できない: {}", path.display()))?;
        if !canonical.starts_with(&root) {
            bail!("Vault外のパスは扱えない: {}", path.display());
        }
        Ok(path)
    }

    fn checked_optional_path(&self, relative: &Path) -> Result<Option<PathBuf>> {
        self.reject_symlink_components(relative)?;
        let path = self.root.join(relative);
        match fs::symlink_metadata(&path) {
            Ok(_) => self.checked_existing_path(relative).map(Some),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => {
                Err(error).with_context(|| format!("パスを検査できない: {}", path.display()))
            }
        }
    }

    fn checked_write_path(&self, relative: &Path) -> Result<PathBuf> {
        self.reject_symlink_components(relative)?;
        let path = self.root.join(relative);
        let parent = path.parent().context("書き込み先に親がない")?;
        fs::create_dir_all(parent)?;

        // create_dir_allの前後で調べる。既存ancestorがsymlinkなら前段で、
        // 作成先がVault外へ解決された場合はcanonical containmentで拒否する。
        let parent_relative = relative.parent().unwrap_or_else(|| Path::new(""));
        self.reject_symlink_components(parent_relative)?;
        let root = self
            .root
            .canonicalize()
            .context("Vault rootを解決できない")?;
        let canonical_parent = parent
            .canonicalize()
            .with_context(|| format!("書き込み先の親を解決できない: {}", parent.display()))?;
        if !canonical_parent.starts_with(&root) {
            bail!("Vault外へは書き込めない: {}", path.display());
        }
        if fs::symlink_metadata(&path).is_ok() {
            self.checked_existing_path(relative)?;
        }
        Ok(path)
    }

    #[cfg(test)]
    pub(crate) fn read_note(&self, raw: &str) -> Result<Note> {
        let id = NoteId::parse(raw)?;
        let path = self.checked_existing_path(&id.markdown_relative_path())?;
        let content = fs::read_to_string(&path)
            .with_context(|| format!("ノートが読めない: {}", path.display()))?;
        Note::parse(&content).with_context(|| format!("parse 失敗: {id}"))
    }

    /// AI・GUI・CLI の日常読み取りは、同じDB snapshotを共有する。
    pub fn read_note_from_db(&self, conn: &rusqlite::Connection, raw: &str) -> Result<Note> {
        crate::note_store::read(conn, raw)
    }

    /// 新規ノートを書き込む。ID(notes/<slug>)を返す。git コミットは呼び側で。
    #[cfg(test)]
    fn write_new_note(&self, title: &str, note: &Note) -> Result<String> {
        let slug = slugify(title);
        let mut id = format!("{NOTES_DIR}/{slug}");
        let mut n = 1;
        while self.note_path(&id)?.exists() {
            n += 1;
            id = format!("{NOTES_DIR}/{slug}-{n}");
        }
        self.write_note(&id, note)?;
        Ok(id)
    }

    fn write_note(&self, raw: &str, note: &Note) -> Result<()> {
        let id = NoteId::parse(raw)?;
        let path = self.checked_write_path(&id.markdown_relative_path())?;
        let parent = path.parent().context("ノート出力先に親がない")?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(note.to_file_string()?.as_bytes())?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(&path)
            .map_err(|error| error.error)
            .with_context(|| format!("Markdown exportを確定できない: {}", path.display()))?;
        Ok(())
    }

    /// 旧 Markdown import の内部互換口。通常書き込みと同じタグ契約を通し、
    /// raw writer 自体は vault の外へ公開しない。
    pub(crate) fn write_imported_note(
        &self,
        conn: &rusqlite::Connection,
        id: &str,
        note: &Note,
        allow_new_tags: bool,
    ) -> Result<()> {
        crate::tags::validate(conn, &note.front.tags, allow_new_tags)?;
        self.write_note(id, note)
    }

    /// AI からのノート起票(origin: agent)。FR-C4 propose。
    /// 2026-08-11 改定: draft という特別な状態は持たない — 暫定・要確認といった
    /// 扱いはタグで表現し、その意味づけはユーザーと AI の会話で決まる(運用)。
    pub fn propose(
        &self,
        conn: &rusqlite::Connection,
        proposal: NoteProposal<'_>,
    ) -> Result<String> {
        let NoteProposal {
            title,
            body,
            description,
            tags,
            authority,
            relations,
            allow_new_tags,
            client,
        } = proposal;
        crate::tags::validate(conn, tags, allow_new_tags)?;
        let mut front = Frontmatter::new_note(title);
        front.origin = Some("agent".into());
        front.created = Some(now_iso());
        front.description = description.map(|s| s.to_string());
        front.tags = tags.to_vec();
        front.note_uid = Some(NoteUid::new());
        front.authority = Some(authority);
        front.relations = relations;
        front.generated = Some(Generated {
            by: client.into(),
            at: now_iso(),
        });
        front.sources = Some(serde_yaml::from_str(&format!(
            "[{{ resource: \"conversation:{client}/{}\" }}]",
            today()
        ))?);
        let note = Note {
            front,
            body: body.to_string(),
        };
        let id = self.next_note_id(conn, title)?;
        crate::note_store::put(
            self,
            conn,
            &id,
            &note,
            &format!("**Proposal**: [{title}](/{id}.md) を起票(via {client})。"),
            &format!("propose {id} (via {client})"),
        )?;
        self.flush_note_exports(conn)?;
        Ok(id)
    }

    fn next_note_id(&self, conn: &rusqlite::Connection, title: &str) -> Result<String> {
        let slug = slugify(title);
        let mut id = format!("{NOTES_DIR}/{slug}");
        let mut suffix = 1;
        while crate::note_store::contains(conn, &id)? || self.note_path(&id)?.exists() {
            suffix += 1;
            id = format!("{NOTES_DIR}/{slug}-{suffix}");
        }
        Ok(id)
    }

    /// 所有ガード。現行ノートは agent 所有で、旧 human ノートは互換読み取り専用。
    fn require_origin(
        &self,
        conn: &rusqlite::Connection,
        id: &str,
        expected: &str,
        deny_msg: &str,
    ) -> Result<Note> {
        let note = self.read_note_from_db(conn, id)?;
        let origin = note.front.origin.as_deref().unwrap_or("human");
        if origin != expected {
            bail!("{deny_msg}");
        }
        Ok(note)
    }

    /// MCPの削除準備で所有ガードと対象snapshotを同じDB正本から取得する。
    pub fn agent_removal_candidate(&self, conn: &rusqlite::Connection, id: &str) -> Result<Note> {
        let note = self.require_origin(
            conn,
            id,
            "agent",
            "旧 human ノートは削除できない(互換読み取り専用)",
        )?;
        if let Some(uid) = &note.front.note_uid {
            let source: Option<String> = conn
                .query_row(
                    "SELECT source.id FROM note_relations relation
                     JOIN notes source ON source.note_uid = relation.src_uid
                     WHERE relation.target_uid=?1 LIMIT 1",
                    [uid.as_str()],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(source) = source {
                bail!("typed relationの参照先は削除できない: {source} -> {id}");
            }
        }
        Ok(note)
    }

    /// AI 自身によるノート削除(MCP)。自分のノート(origin: agent)のみ。
    pub fn agent_delete_note(
        &self,
        conn: &rusqlite::Connection,
        id: &str,
        reason: &str,
        client: &str,
    ) -> Result<()> {
        let reason = reason.trim();
        if reason.is_empty() || reason.chars().count() > 500 || reason.contains(['\n', '\r']) {
            bail!("削除理由は1〜500文字の一行で指定する");
        }
        let title = self
            .agent_removal_candidate(conn, id)?
            .front
            .title
            .unwrap_or_else(|| id.to_string());
        crate::note_store::delete(
            conn,
            id,
            &format!("**Deletion**: 「{title}」({id})を削除。理由: {reason}"),
            &format!("note: delete {id} (via {client})"),
        )?;
        self.flush_note_exports(conn)?;
        Ok(())
    }

    /// AI 自身によるノート更新(MCP)。自分のノート(origin: agent)のみ。
    pub fn agent_update_note(
        &self,
        conn: &rusqlite::Connection,
        update: NoteUpdate<'_>,
    ) -> Result<()> {
        let NoteUpdate {
            id,
            title,
            body,
            description,
            tags,
            authority,
            relations,
            allow_new_tags,
            client,
        } = update;
        let mut note = self.require_origin(
            conn,
            id,
            "agent",
            "旧 human ノートは編集できない(互換読み取り専用)",
        )?;
        if let Some(t) = title {
            note.front.title = Some(t.to_string());
        }
        if let Some(d) = description {
            note.front.description = Some(d.to_string());
        }
        if let Some(ts) = tags {
            crate::tags::validate(conn, ts, allow_new_tags)?;
            note.front.tags = ts.to_vec();
        }
        if let Some(authority) = authority {
            note.front.authority = Some(authority);
            note.front.note_uid.get_or_insert_with(NoteUid::new);
        }
        if let Some(relations) = relations {
            note.front.relations = relations;
        }
        if let Some(b) = body {
            note.body = b.to_string();
        }
        note.front.generated = Some(Generated {
            by: client.into(),
            at: now_iso(),
        });
        let t = note.front.title.as_deref().unwrap_or(id);
        crate::note_store::put(
            self,
            conn,
            id,
            &note,
            &format!("**Update**: [{t}](/{id}.md) を AI が更新(via {client})。"),
            &format!("note: update {id} (via {client})"),
        )?;
        self.flush_note_exports(conn)?;
        Ok(())
    }

    /// 製品APIを迂回せずに他モジュールのテストfixtureを作るための専用口。
    #[cfg(test)]
    pub(crate) fn propose_for_test(
        &self,
        title: &str,
        body: &str,
        description: Option<&str>,
        tags: &[String],
        client: &str,
    ) -> Result<String> {
        let conn = crate::index::open_db(self)?;
        crate::index::sync(self, &conn)?;
        self.propose(
            &conn,
            NoteProposal {
                title,
                body,
                description,
                tags,
                authority: Authority {
                    namespace: NoteNamespace::Records,
                    role: crate::authority::AuthorityRole::Record,
                    status: crate::authority::AuthorityStatus::Active,
                    scope: format!("test/{}", slugify(title)),
                },
                relations: Vec::new(),
                allow_new_tags: true,
                client,
            },
        )
    }

    /// 既に壊れた外部編集データを再現するテスト専用fixture。製品buildには存在しない。
    #[cfg(test)]
    pub(crate) fn write_note_fixture(&self, id: &str, note: &Note) -> Result<()> {
        self.write_note(id, note)
    }

    /// DB commitと同じtransactionで積まれたMarkdown出力を、順番どおり冪等に反映する。
    pub fn flush_note_exports(&self, conn: &rusqlite::Connection) -> Result<usize> {
        let pending = crate::note_store::pending(conn)?;
        let count = pending.len();
        for export in pending {
            let id = NoteId::parse(&export.note_id)?;
            match export.operation {
                crate::note_store::ExportOperation::Upsert => {
                    let document = export.document.context("upsert exportに本文がない")?;
                    self.ensure_export_base(&id, export.base_document.as_deref(), Some(&document))?;
                    let note = Note::parse(&document)?;
                    self.write_note(id.as_str(), &note)?;
                }
                crate::note_store::ExportOperation::Delete => {
                    self.ensure_export_base(&id, export.base_document.as_deref(), None)?;
                    let relative = id.markdown_relative_path();
                    let attachments_relative = id.attachments_relative_path();
                    let repo = Repository::open(&self.root)?;
                    let mut index = repo.index()?;
                    let _ = index.remove_dir(&attachments_relative, 0);
                    index.write()?;
                    if let Some(path) = self.checked_optional_path(&relative)? {
                        fs::remove_file(path)?;
                    }
                    if let Some(attachments) = self.checked_optional_path(&attachments_relative)? {
                        let _ = fs::remove_dir_all(attachments);
                    }
                }
            }
            self.append_log_once(&export.op_id, &export.log_entry)?;
            self.write_index_md()?;
            self.commit_note_op(id.as_str(), &export.commit_message)?;
            crate::note_store::complete(conn, export.seq)?;
        }
        if count != 0 {
            crate::connect::auto_push(self);
        }
        Ok(count)
    }

    fn ensure_export_base(
        &self,
        id: &NoteId,
        base_document: Option<&str>,
        target_document: Option<&str>,
    ) -> Result<()> {
        let relative = id.markdown_relative_path();
        let Some(path) = self.checked_optional_path(&relative)? else {
            return Ok(());
        };
        let current = fs::read_to_string(path)?;
        if base_document.is_some_and(|base| current == base)
            || target_document.is_some_and(|target| current == target)
        {
            return Ok(());
        }
        bail!(
            "表示用Markdownが外部編集されているため上書きしない: {}",
            id.as_str()
        )
    }

    fn append_log_once(&self, operation_id: &str, entry: &str) -> Result<()> {
        let marker = format!("<!-- kb-export:{operation_id} -->");
        let existing = fs::read_to_string(self.root.join("log.md")).unwrap_or_default();
        if !existing.contains(&marker) {
            self.append_log(&format!("{entry} {marker}"))?;
        }
        Ok(())
    }

    /// 旧添付のディレクトリ(FR-C8 の `<id>.files/` サイドカー)。
    ///
    /// **読み取り専用の legacy transport**(ADR-0003 決定4)。実体が保管庫 Git の
    /// 中にあるので、ここへ足すと binary が履歴に積み上がる。新規の書き込みは
    /// [`crate::intake`] へ合流させ、追加も削除もこの経路には残していない
    /// (削除は不変性を壊すので Artifact に流用できない)。
    #[cfg(test)]
    pub(crate) fn attach_dir(&self, raw: &str) -> Result<PathBuf> {
        let id = NoteId::parse(raw)?;
        Ok(self.root.join(id.attachments_relative_path()))
    }

    /// 旧添付を読む唯一の実ファイル解決口。IDとファイル名の両方を検査し、
    /// symlinkを含むVault外到達を拒否する。
    pub fn legacy_attachment_path(&self, raw: &str, file_name: &str) -> Result<PathBuf> {
        let id = NoteId::parse(raw)?;
        let relative = id.legacy_attachment_relative_path(file_name)?;
        Ok(self
            .checked_optional_path(&relative)?
            .unwrap_or_else(|| self.root.join(relative)))
    }

    /// 旧添付の一覧 (名前, バイト数)。移行するまで画面と索引が読む。
    pub fn list_attachments(&self, raw: &str) -> Result<Vec<(String, u64)>> {
        let id = NoteId::parse(raw)?;
        let mut out = Vec::new();
        let relative = id.attachments_relative_path();
        if let Some(dir) = self.checked_optional_path(&relative)? {
            let entries = fs::read_dir(dir)?;
            for entry in entries {
                let e = entry?;
                if e.file_type()?.is_file() {
                    let size = e.metadata()?.len();
                    let name = e.file_name();
                    let name = name.to_str().context("旧添付名がUTF-8ではない")?;
                    out.push((name.to_string(), size));
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// 全ノートの (id, 絶対パス)。予約ファイル・.kb・.git・添付(*.files)は除外。
    pub fn list_note_files(&self) -> Result<Vec<(String, PathBuf)>> {
        let mut out = Vec::new();
        for entry in walkdir::WalkDir::new(&self.root)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                name != ".git"
                    && name != ".kb"
                    && !(e.file_type().is_dir() && name.ends_with(".files"))
            })
        {
            let entry = entry
                .with_context(|| format!("Vaultのノート走査に失敗: {}", self.root.display()))?;
            let path = entry.path();
            if !entry.file_type().is_file()
                || path.extension().and_then(|e| e.to_str()) != Some("md")
            {
                continue;
            }
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if RESERVED.contains(&name.as_ref()) {
                continue;
            }
            let rel = path
                .strip_prefix(&self.root)
                .context("Vault走査結果がrootの外にある")?;
            let id = rel.with_extension("");
            let id = id.to_str().context("Note IDがUTF-8ではない")?;
            out.push((id.to_string(), path.to_path_buf()));
        }
        out.sort();
        Ok(out)
    }

    /// バンドルルート index.md を自動生成(OKF §8+§12: okf_version 宣言)。
    pub fn write_index_md(&self) -> Result<()> {
        let mut lines = vec![
            "---".to_string(),
            "okf_version: \"0.2\"".to_string(),
            "---".to_string(),
            String::new(),
            "# Notes".to_string(),
            String::new(),
        ];
        for (id, path) in self.list_note_files()? {
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(note) = Note::parse(&content) else {
                continue;
            };
            let title = note.front.title.as_deref().unwrap_or(&id);
            let desc = note.front.description.as_deref().unwrap_or("");
            let sep = if desc.is_empty() { "" } else { " - " };
            lines.push(format!("* [{title}]({id}.md){sep}{desc}"));
        }
        lines.push(String::new());
        fs::write(self.root.join("index.md"), lines.join("\n"))?;
        Ok(())
    }

    /// log.md へ1行追記(OKF §9: 日付見出しグループ・新しい日付が先頭)。
    pub fn append_log(&self, entry: &str) -> Result<()> {
        let path = self.root.join("log.md");
        let today_heading = format!("## {}", today());
        let existing = fs::read_to_string(&path).unwrap_or_default();
        let content = if existing.is_empty() {
            format!("# Update Log\n\n{today_heading}\n* {entry}\n")
        } else if let Some(pos) = existing.find(&today_heading) {
            let insert_at = pos + today_heading.len();
            format!(
                "{}\n* {entry}{}",
                &existing[..insert_at],
                &existing[insert_at..]
            )
        } else {
            // 新しい日付セクションをタイトル行の直後(=先頭側)へ
            match existing.find("\n## ") {
                Some(pos) => format!(
                    "{}\n{today_heading}\n* {entry}\n{}",
                    &existing[..pos],
                    &existing[pos + 1..]
                ),
                None => format!("{}\n{today_heading}\n* {entry}\n", existing.trim_end()),
            }
        };
        fs::write(&path, content)?;
        Ok(())
    }

    fn commit_note_op(&self, id: &str, message: &str) -> Result<()> {
        let id = NoteId::parse(id)?;
        let note_path = id.markdown_relative_path();
        let note_path = note_path.to_str().context("ノートIDがUTF-8ではない")?;
        self.commit(&[note_path, "index.md", "log.md"], message)?;
        Ok(())
    }

    /// 指定パスをステージしてコミット(git 履歴 = 監査痕跡)。
    pub fn commit(&self, rel_paths: &[&str], message: &str) -> Result<()> {
        let repo = Repository::open(&self.root)?;
        let mut index = repo.index()?;
        for p in rel_paths {
            if self.root.join(p).exists() {
                index.add_path(Path::new(p))?;
            } else {
                // 消えたパスも同じ呼び出しで畳む。追跡外なら何もしない
                // (台帳が同期境界を跨いで移動するとき、旧側の削除が commit に乗らないため)
                let _ = index.remove_path(Path::new(p));
            }
        }
        index.write()?;
        let tree = repo.find_tree(index.write_tree()?)?;
        let sig = repo
            .signature()
            .or_else(|_| Signature::now("kb-app", "kb-app@localhost"))?;
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        if parent
            .as_ref()
            .is_some_and(|commit| commit.tree_id() == tree.id())
        {
            return Ok(());
        }
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)?;
        Ok(())
    }
}

/// タイトル → ファイル名 slug。日本語はそのまま残す(パス=ID、APFS/NTFS で有効)。
pub fn slugify(title: &str) -> String {
    let mut out = String::new();
    for c in title.trim().chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "note".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 旧 vault に既に存在しうる human ノートの互換フィクスチャ。
    /// 製品の作成 API ではなく、所有ガードの境界確認にだけ使う。
    fn write_legacy_human_fixture(vault: &Vault, title: &str, body: &str) -> String {
        let mut front = Frontmatter::new_note(title);
        front.origin = Some("human".into());
        front.created = Some(now_iso());
        front.generated = Some(Generated {
            by: "human:legacy".into(),
            at: now_iso(),
        });
        vault
            .write_new_note(
                title,
                &Note {
                    front,
                    body: body.into(),
                },
            )
            .unwrap()
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("認証 設計 メモ"), "認証-設計-メモ");
        assert_eq!(slugify("!!!"), "note");
    }

    /// 旧添付は読むだけ(ADR-0003 決定4)。書き込む経路は残していないので、
    /// ここで確かめるのは「既にあるものが読めること」と「ノート走査に映らないこと」。
    #[test]
    fn legacy_attachments_are_listed_but_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let id = vault
            .propose_for_test(
                "添付テスト",
                "本文。",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let files = vault.attach_dir(&id).unwrap();
        std::fs::create_dir_all(&files).unwrap();
        std::fs::write(files.join("図.png"), b"png-bytes").unwrap();
        assert_eq!(
            vault.list_attachments(&id).unwrap(),
            vec![("図.png".into(), 9u64)]
        );

        // 添付ディレクトリはノート走査に映らない(OKF 互換の保全)
        std::fs::write(files.join("紛れ.md"), "---\ntype: Note\n---\nx").unwrap();
        assert_eq!(vault.list_note_files().unwrap().len(), 1);
    }

    fn agent_note(title: &str, body: &str) -> Note {
        let mut front = Frontmatter::new_note(title);
        front.origin = Some("agent".into());
        front.created = Some(now_iso());
        Note {
            front,
            body: body.into(),
        }
    }

    /// get/update/remove は同じNoteId境界を通り、Vaultの外を読まず変更しない。
    #[test]
    fn note_operations_cannot_escape_the_vault() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let secret = outside.join("secret.md");
        let original = agent_note("外部", "変更されない本文")
            .to_file_string()
            .unwrap();
        std::fs::write(&secret, &original).unwrap();

        let absolute = outside.join("secret").to_string_lossy().to_string();
        for id in ["../outside/secret", "notes/../../outside/secret", &absolute] {
            assert!(vault.read_note(id).is_err(), "読めてはいけない: {id}");
            assert!(
                vault
                    .agent_update_note(
                        &conn,
                        NoteUpdate {
                            id,
                            title: None,
                            body: Some("侵入"),
                            description: None,
                            tags: None,
                            authority: None,
                            relations: None,
                            allow_new_tags: false,
                            client: "test/client",
                        },
                    )
                    .is_err(),
                "更新できてはいけない: {id}"
            );
            assert!(
                vault
                    .agent_delete_note(&conn, id, "境界テスト", "test/client")
                    .is_err(),
                "削除できてはいけない: {id}"
            );
            assert_eq!(std::fs::read_to_string(&secret).unwrap(), original);
        }
    }

    /// 字句上はVault内でも、symlinkで外へ出るノートは全操作で拒否する。
    #[cfg(unix)]
    #[test]
    fn note_operations_reject_symlink_escape() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let outside = dir.path().join("secret.md");
        let original = agent_note("外部", "変更されない本文")
            .to_file_string()
            .unwrap();
        std::fs::write(&outside, &original).unwrap();
        symlink(&outside, vault.root.join("notes/linked.md")).unwrap();

        assert!(vault.read_note("notes/linked").is_err());
        assert!(
            vault
                .agent_update_note(
                    &conn,
                    NoteUpdate {
                        id: "notes/linked",
                        title: None,
                        body: Some("侵入"),
                        description: None,
                        tags: None,
                        authority: None,
                        relations: None,
                        allow_new_tags: false,
                        client: "test/client",
                    },
                )
                .is_err()
        );
        assert!(
            vault
                .agent_delete_note(&conn, "notes/linked", "添付境界テスト", "test/client")
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), original);
    }

    #[test]
    fn nested_unicode_note_ids_remain_valid() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let note = agent_note("同期設計", "本文");
        vault.write_note_fixture("設計/同期/端末間", &note).unwrap();
        assert_eq!(vault.read_note("設計/同期/端末間").unwrap().body, "本文\n");
    }

    #[test]
    fn agent_delete_requires_a_bounded_single_line_reason() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let id = vault
            .propose_for_test(
                "削除理由の検証",
                "本文",
                None,
                &["dev".into()],
                "test/client",
            )
            .unwrap();

        for reason in ["", "一行目\n二行目"] {
            let err = vault
                .agent_delete_note(&conn, &id, reason, "test/client")
                .unwrap_err();
            assert!(err.to_string().contains("削除理由は1〜500文字の一行"));
            assert_eq!(vault.read_note(&id).unwrap().body, "本文\n");
        }

        let too_long = "あ".repeat(501);
        let err = vault
            .agent_delete_note(&conn, &id, &too_long, "test/client")
            .unwrap_err();
        assert!(err.to_string().contains("削除理由は1〜500文字の一行"));
        assert_eq!(vault.read_note(&id).unwrap().body, "本文\n");

        vault
            .agent_delete_note(&conn, &id, "重複ノートへ統合済み", "test/client")
            .unwrap();
        assert!(vault.read_note(&id).is_err());
    }

    #[test]
    fn legacy_attachment_paths_share_the_same_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        assert!(
            vault
                .legacy_attachment_path("../outside", "secret.txt")
                .is_err()
        );
        assert!(
            vault
                .legacy_attachment_path("notes/a", "../secret.txt")
                .is_err()
        );
        assert!(
            vault
                .legacy_attachment_path("notes/a", r"..\secret.txt")
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn legacy_attachment_paths_reject_symlink_directories_even_when_file_is_missing() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, vault.root.join("notes/a.files")).unwrap();

        assert!(
            vault
                .legacy_attachment_path("notes/a", "missing.txt")
                .is_err()
        );
    }

    /// 現行ノートは AI 所有。旧 human ノートは読めるが、更新・削除はできない。
    #[test]
    fn ai_owns_current_notes_and_legacy_human_notes_are_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let legacy = write_legacy_human_fixture(&vault, "旧メモ", "本文");
        let ai = vault
            .propose_for_test("AI の知見", "本文", None, &["dev".into()], "claude/x")
            .unwrap();

        assert_eq!(
            vault.read_note(&legacy).unwrap().front.origin.as_deref(),
            Some("human")
        );
        assert!(
            vault
                .agent_update_note(
                    &conn,
                    NoteUpdate {
                        id: &legacy,
                        title: None,
                        body: Some("侵入"),
                        description: None,
                        tags: None,
                        authority: None,
                        relations: None,
                        allow_new_tags: false,
                        client: "claude/x",
                    },
                )
                .is_err()
        );
        assert!(
            vault
                .agent_delete_note(&conn, &legacy, "所有ガードテスト", "claude/x")
                .is_err()
        );

        assert_eq!(
            vault.read_note(&ai).unwrap().front.origin.as_deref(),
            Some("agent")
        );
        assert!(
            vault
                .agent_update_note(
                    &conn,
                    NoteUpdate {
                        id: &ai,
                        title: Some("AI の知見 v2"),
                        body: None,
                        description: None,
                        tags: None,
                        authority: None,
                        relations: None,
                        allow_new_tags: false,
                        client: "claude/x",
                    },
                )
                .is_ok()
        );

        // AI は自分のノートを消せる
        let ai2 = vault
            .propose_for_test("捨てる知見", "本文", None, &["dev".into()], "claude/x")
            .unwrap();
        assert!(
            vault
                .agent_delete_note(&conn, &ai2, "重複整理", "claude/x")
                .is_ok()
        );
    }

    #[test]
    fn all_note_write_apis_use_the_same_tag_contract() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();

        assert!(
            vault
                .propose(
                    &conn,
                    NoteProposal {
                        title: "タグなし",
                        body: "本文",
                        description: None,
                        tags: &[],
                        authority: Authority {
                            namespace: NoteNamespace::Knowledge,
                            role: crate::authority::AuthorityRole::Canonical,
                            status: crate::authority::AuthorityStatus::Active,
                            scope: "test/no-tags".into(),
                        },
                        relations: Vec::new(),
                        allow_new_tags: true,
                        client: "test/client",
                    },
                )
                .is_err()
        );
        let id = vault
            .propose(
                &conn,
                NoteProposal {
                    title: "既存語",
                    body: "本文",
                    description: None,
                    tags: &["known".into()],
                    authority: Authority {
                        namespace: NoteNamespace::Knowledge,
                        role: crate::authority::AuthorityRole::Canonical,
                        status: crate::authority::AuthorityStatus::Active,
                        scope: "test/known-tag".into(),
                    },
                    relations: Vec::new(),
                    allow_new_tags: false,
                    client: "test/client",
                },
            )
            .unwrap();
        crate::index::sync(&vault, &conn).unwrap();

        assert!(
            vault
                .propose(
                    &conn,
                    NoteProposal {
                        title: "語彙外",
                        body: "本文",
                        description: None,
                        tags: &["brand-new".into()],
                        authority: Authority {
                            namespace: NoteNamespace::Knowledge,
                            role: crate::authority::AuthorityRole::Canonical,
                            status: crate::authority::AuthorityStatus::Active,
                            scope: "test/new-tag".into(),
                        },
                        relations: Vec::new(),
                        allow_new_tags: false,
                        client: "test/client",
                    },
                )
                .is_err()
        );
        assert!(
            vault
                .agent_update_note(
                    &conn,
                    NoteUpdate {
                        id: &id,
                        title: None,
                        body: None,
                        description: None,
                        tags: Some(&[]),
                        authority: None,
                        relations: None,
                        allow_new_tags: true,
                        client: "test/client",
                    },
                )
                .is_err()
        );
        assert_eq!(vault.read_note(&id).unwrap().front.tags, vec!["known"]);
    }

    #[test]
    fn create_and_propose_flow() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let id = vault
            .propose_for_test(
                "テスト起票",
                "本文です。",
                Some("説明"),
                &["dev".into()],
                "test-client/model",
            )
            .unwrap();
        let note = vault.read_note(&id).unwrap();
        // draft という特別な状態は持たない(2026-08-11 改定)
        assert_eq!(note.front.effective_status(), "stable");
        assert_eq!(note.front.origin.as_deref(), Some("agent"));
        assert_eq!(note.front.tags, vec!["dev".to_string()]);
        // index.md / log.md が生成され、予約名はノート一覧に出ない
        assert!(vault.root.join("index.md").exists());
        assert!(vault.root.join("log.md").exists());
        assert_eq!(vault.list_note_files().unwrap().len(), 1);
    }
}
