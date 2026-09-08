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
use serde::Serialize;
use sha2::{Digest as _, Sha256};

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

#[derive(Debug, Serialize)]
pub struct MarkdownExportConflict {
    pub note: String,
    pub operation_id: String,
    pub markdown_hash: String,
    pub pending_document_hash: String,
    pub base_document_hash: Option<String>,
    pub markdown_document: String,
    pub pending_document: String,
}

#[derive(Debug, Serialize)]
pub struct MarkdownExportResolution {
    pub note: String,
    pub operation_id: String,
    pub strategy: &'static str,
    pub pending_exports: usize,
}

/// ノート起票の入力。タグ契約の明示フラグを起票内容と一体で渡す。
pub struct NoteProposal<'a> {
    pub title: &'a str,
    pub body: &'a str,
    pub description: Option<&'a str>,
    pub tags: &'a [String],
    pub authority: Authority,
    pub relations: Vec<NoteRelation>,
    pub judgment: Option<crate::judgment::Judgment>,
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
    /// Noneは保持、Some(None)は削除、Some(Some(_))は全置換する。
    pub judgment: Option<Option<crate::judgment::Judgment>>,
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
        let before = crate::note_store::contains(conn, id)?
            .then(|| crate::note_store::read(conn, id))
            .transpose()?;
        crate::proposal_workflow::guard_import(before.as_ref(), note)?;
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
            judgment,
            allow_new_tags,
            client,
        } = proposal;
        validate_note_text_input("title", title)?;
        validate_note_text_input("body", body)?;
        crate::tags::validate(conn, tags, allow_new_tags)
            .map_err(crate::write_rejection::confirm_before_write)?;
        let mut front = Frontmatter::new_note(title);
        front.origin = Some("agent".into());
        front.created = Some(now_iso());
        front.description = description.map(|s| s.to_string());
        front.tags = tags.to_vec();
        front.note_uid = Some(NoteUid::new());
        front.authority = Some(authority);
        front.relations = relations;
        front.judgment = judgment;
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

    pub(crate) fn next_note_id(&self, conn: &rusqlite::Connection, title: &str) -> Result<String> {
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
            return Err(crate::write_rejection::WriteRejection::LegacyReadOnly.reject(deny_msg));
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
        crate::proposal_workflow::guard_note_delete(&note)?;
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
            self,
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
        self.agent_update_note_with_warnings(conn, update)
            .map(|_| ())
    }

    pub(crate) fn agent_update_note_with_warnings(
        &self,
        conn: &rusqlite::Connection,
        update: NoteUpdate<'_>,
    ) -> Result<Vec<crate::write_guidance::UpdateWarning>> {
        let NoteUpdate {
            id,
            title,
            body,
            description,
            tags,
            authority,
            relations,
            judgment,
            allow_new_tags,
            client,
        } = update;
        let mut note = self.require_origin(
            conn,
            id,
            "agent",
            "旧 human ノートは編集できない(互換読み取り専用)",
        )?;
        let before = note.clone();
        if let Some(t) = title {
            validate_note_text_input("title", t)?;
            note.front.title = Some(t.to_string());
        }
        if let Some(b) = body {
            validate_note_text_input("body", b)?;
        }
        if let Some(d) = description {
            note.front.description = Some(d.to_string());
        }
        if let Some(ts) = tags {
            crate::tags::validate(conn, ts, allow_new_tags)
                .map_err(crate::write_rejection::confirm_before_write)?;
            note.front.tags = ts.to_vec();
        }
        if let Some(authority) = authority {
            note.front.authority = Some(authority);
            note.front.note_uid.get_or_insert_with(NoteUid::new);
        }
        if let Some(relations) = relations {
            note.front.relations = relations;
        }
        if let Some(judgment) = judgment {
            note.front.judgment = judgment;
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
        Ok(crate::write_guidance::update_warnings(&before, &note))
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
                judgment: None,
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

    /// 外部編集とDB outboxの衝突を、内容を変更せずに検査する。
    /// 解消側はここで返す両hashへ固定し、検査後の差し替えを拒否する。
    pub fn inspect_markdown_export_conflict(
        &self,
        conn: &rusqlite::Connection,
        raw: &str,
    ) -> Result<MarkdownExportConflict> {
        let id = NoteId::parse(raw)?;
        let export = crate::note_store::pending(conn)?
            .into_iter()
            .find(|export| export.note_id == id.as_str())
            .with_context(|| format!("保留中のMarkdown出力がない: {id}"))?;
        if export.operation != crate::note_store::ExportOperation::Upsert {
            bail!("delete出力の競合はこの操作では解消できない: {id}");
        }
        let pending_document = export.document.context("upsert exportに本文がない")?;
        let markdown_document = fs::read_to_string(self.note_path(id.as_str())?)
            .with_context(|| format!("表示用Markdownを読めない: {id}"))?;
        if export.base_document.as_deref() == Some(markdown_document.as_str())
            || pending_document == markdown_document
        {
            bail!("表示用Markdownは外部編集状態ではない: {id}");
        }
        Ok(MarkdownExportConflict {
            note: id.to_string(),
            operation_id: export.op_id,
            markdown_hash: document_hash(&markdown_document),
            pending_document_hash: document_hash(&pending_document),
            base_document_hash: export.base_document.as_deref().map(document_hash),
            markdown_document,
            pending_document,
        })
    }

    /// 検査済みの外部Markdownだけを、hash固定でDB側の確定documentへ置き換える。
    /// 外部編集を黙って捨てないため、inspectで得た両hashが一致しなければ停止する。
    pub fn resolve_markdown_export_keep_db(
        &self,
        conn: &rusqlite::Connection,
        raw: &str,
        expected_markdown_hash: &str,
        expected_pending_document_hash: &str,
    ) -> Result<MarkdownExportResolution> {
        let conflict = self.inspect_markdown_export_conflict(conn, raw)?;
        if conflict.markdown_hash != expected_markdown_hash {
            bail!(
                "検査後に表示用Markdownが変わったため停止: {}",
                conflict.note
            );
        }
        if conflict.pending_document_hash != expected_pending_document_hash {
            bail!(
                "検査後にDB確定documentが変わったため停止: {}",
                conflict.note
            );
        }
        let export = crate::note_store::pending(conn)?
            .into_iter()
            .find(|export| export.op_id == conflict.operation_id)
            .context("検査したMarkdown出力が見つからない")?;
        let document = export.document.context("upsert exportに本文がない")?;
        let note = Note::parse(&document)?;
        self.write_note(&conflict.note, &note)?;
        self.append_log_once(&export.op_id, &export.log_entry)?;
        self.write_index_md()?;
        self.commit_note_op(&conflict.note, &export.commit_message)?;
        crate::note_store::complete(conn, export.seq)?;
        self.flush_note_exports(conn)?;
        crate::index::mark_runtime_store_db(conn)?;
        crate::connect::auto_push(self);
        Ok(MarkdownExportResolution {
            note: conflict.note,
            operation_id: conflict.operation_id,
            strategy: "keep_db",
            pending_exports: crate::note_store::pending_count(conn)?,
        })
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

fn document_hash(document: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(document.as_bytes()))
}

// 既存の空ノートは読めるまま、新しく渡された入力だけを拒否する。
// parse/exportの共通検証へ置くと、無関係なmetadata更新やバックアップまで止まる。
fn validate_note_text_input(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(crate::write_rejection::WriteRejection::InvalidArgument
            .reject(format!("{field} は空白以外の文字を含める")));
    }
    Ok(())
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
                            judgment: None,
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
                        judgment: None,
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
                        judgment: None,
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
                        judgment: None,
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

    fn text_proposal<'a>(title: &'a str, body: &'a str, tags: &'a [String]) -> NoteProposal<'a> {
        NoteProposal {
            judgment: None,
            title,
            body,
            description: None,
            tags,
            authority: Authority {
                namespace: NoteNamespace::Records,
                role: crate::authority::AuthorityRole::Record,
                status: crate::authority::AuthorityStatus::Active,
                scope: "test/text-input".into(),
            },
            relations: Vec::new(),
            allow_new_tags: true,
            client: "test/text-input",
        }
    }

    fn text_write_snapshot(vault: &Vault, conn: &rusqlite::Connection) -> serde_json::Value {
        let documents: Vec<(String, String)> = conn
            .prepare("SELECT id, document FROM notes ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        let markdown: Vec<(String, String)> = vault
            .list_note_files()
            .unwrap()
            .into_iter()
            .map(|(id, path)| (id, fs::read_to_string(path).unwrap()))
            .collect();
        let head = Repository::open(&vault.root)
            .unwrap()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id();
        serde_json::json!({
            "documents": documents,
            "db_changes": conn.total_changes(),
            "outbox": crate::note_store::pending_count(conn).unwrap(),
            "head": head.to_string(),
            "markdown": markdown,
            "index": fs::read_to_string(vault.root.join("index.md")).unwrap(),
            "log": fs::read_to_string(vault.root.join("log.md")).unwrap(),
        })
    }

    fn judgment_fixture() -> crate::judgment::Judgment {
        serde_json::from_value(serde_json::json!({
            "kind": "decision", "basis": "user_correction",
            "source": {"reference": "conversation:fixture/turn-2", "excerpt": "反映コマンドは本人が実行する"},
            "applies_when": "アプリ反映を依頼されたとき",
            "action": "検証済みコマンドを提示する",
            "exceptions": ["今回だけAIが実行すると本人が明示した場合"]
        })).unwrap()
    }

    #[test]
    fn judgment_persists_in_db_and_markdown_and_update_can_preserve_or_remove_it() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let tags = ["known".into()];
        let judgment = judgment_fixture();
        let mut proposal = text_proposal("反映担当", "検証と反映の記録", &tags);
        proposal.judgment = Some(judgment.clone());
        let id = vault.propose(&conn, proposal).unwrap();
        for note in [
            vault.read_note_from_db(&conn, &id).unwrap(),
            vault.read_note(&id).unwrap(),
        ] {
            assert_eq!(note.front.judgment, Some(judgment.clone()));
        }
        let mut replacement = judgment.clone();
        if let crate::judgment::Judgment::Decision { action, .. } = &mut replacement {
            *action = "本人の実行後に署名と反映内容を検証する".into();
        }
        for (change, expected) in [
            (None, Some(judgment.clone())),
            (Some(Some(replacement.clone())), Some(replacement)),
            (Some(None), None),
        ] {
            vault
                .agent_update_note(
                    &conn,
                    NoteUpdate {
                        id: &id,
                        title: None,
                        body: None,
                        description: Some("別項目だけを更新"),
                        tags: None,
                        authority: None,
                        relations: None,
                        judgment: change,
                        allow_new_tags: false,
                        client: "test/judgment",
                    },
                )
                .unwrap();
            assert_eq!(
                vault.read_note_from_db(&conn, &id).unwrap().front.judgment,
                expected
            );
            assert_eq!(vault.read_note(&id).unwrap().front.judgment, expected);
        }
        assert!(
            !vault
                .read_note(&id)
                .unwrap()
                .to_file_string()
                .unwrap()
                .contains("judgment:")
        );
    }

    /// 2026-09-08: 読んだ本人決定を使わなかった事故に備え、出典欠損を本文更新とともに拒否する。
    #[test]
    fn invalid_judgment_rejects_the_entire_create_or_update_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let tags = ["known".into()];
        let id = vault
            .propose(&conn, text_proposal("元の記録", "変わらない本文", &tags))
            .unwrap();
        let before = text_write_snapshot(&vault, &conn);
        let mut invalid = judgment_fixture();
        if let crate::judgment::Judgment::Decision { source, .. } = &mut invalid {
            source.excerpt.clear();
        }
        let mut proposal = text_proposal("保存してはいけない", "新規本文", &tags);
        proposal.judgment = Some(invalid.clone());
        let create_error = vault.propose(&conn, proposal).unwrap_err();
        assert_eq!(
            crate::write_rejection::WriteRejection::from_error(&create_error),
            Some(crate::write_rejection::WriteRejection::InvalidArgument)
        );
        assert_eq!(text_write_snapshot(&vault, &conn), before);
        let update_error = vault
            .agent_update_note(
                &conn,
                NoteUpdate {
                    id: &id,
                    title: Some("保存してはいけない"),
                    body: Some("保存してはいけない本文"),
                    description: None,
                    tags: None,
                    authority: None,
                    relations: None,
                    judgment: Some(Some(invalid)),
                    allow_new_tags: false,
                    client: "test/judgment",
                },
            )
            .unwrap_err();
        assert_eq!(
            crate::write_rejection::WriteRejection::from_error(&update_error),
            Some(crate::write_rejection::WriteRejection::InvalidArgument)
        );
        assert_eq!(text_write_snapshot(&vault, &conn), before);
    }

    #[test]
    fn action_judgment_uses_typed_relations_for_referent_deletion_protection() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let tags = ["known".into()];
        let mut proposal = text_proposal("採用した判断", "判断の記録", &tags);
        proposal.judgment = Some(judgment_fixture());
        let decision = vault.propose(&conn, proposal).unwrap();
        let uid = vault
            .read_note_from_db(&conn, &decision)
            .unwrap()
            .front
            .note_uid
            .unwrap();
        let mut action = text_proposal("反映の実績", "本人がTerminalで実行した", &tags);
        action.judgment = Some(serde_json::from_value(serde_json::json!({
            "kind": "action", "source": {"reference": "conversation:fixture/turn-3", "excerpt": "実行した"},
            "situation": "アプリ反映", "action": "本人が反映コマンドを実行した", "outcome": "succeeded",
            "evidence": "user_report", "decision_refs": [uid],
        })).unwrap());
        action.relations = vec![NoteRelation {
            kind: crate::authority::RelationKind::Mentions,
            target: uid,
        }];
        let action_id = vault.propose(&conn, action).unwrap();
        assert!(
            vault
                .read_note_from_db(&conn, &action_id)
                .unwrap()
                .front
                .judgment
                .is_some()
        );
        assert!(
            vault
                .agent_delete_note(&conn, &decision, "参照保護の検証", "test/judgment")
                .is_err()
        );
        assert!(crate::note_store::contains(&conn, &decision).unwrap());
        let before = text_write_snapshot(&vault, &conn);
        assert!(
            vault
                .agent_update_note(
                    &conn,
                    NoteUpdate {
                        id: &action_id,
                        title: None,
                        body: None,
                        description: None,
                        tags: None,
                        authority: None,
                        relations: Some(Vec::new()),
                        judgment: None,
                        allow_new_tags: false,
                        client: "test/judgment",
                    }
                )
                .is_err()
        );
        assert_eq!(text_write_snapshot(&vault, &conn), before);
    }

    #[test]
    fn judgment_survives_semantic_normalization() {
        use crate::distillation_executor::{
            DistillationChange, DistillationTarget, ExecutableOperation, NullableString,
        };
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let tags = ["known".into()];
        let mut proposal = text_proposal("本人の決定", "判断の本文", &tags);
        proposal.judgment = Some(judgment_fixture());
        let id = vault.propose(&conn, proposal).unwrap();
        let before = vault.read_note_from_db(&conn, &id).unwrap();
        let change = DistillationChange {
            note: id,
            input_hash: String::new(),
            operation: ExecutableOperation::Normalize,
            reason: "descriptionを補う".into(),
            target: DistillationTarget {
                title: NullableString(before.front.title.clone()),
                body: before.body.clone(),
                description: NullableString(Some("反映担当を示す決定".into())),
                tags: before.front.tags.clone(),
                relations: before.front.relations.clone(),
            },
        };
        let after =
            crate::distillation_executor::prepare_target(&conn, &before, &change, "test/judgment")
                .unwrap();
        assert_eq!(after.front.judgment, before.front.judgment);
        assert_eq!(
            Note::parse(&after.to_file_string().unwrap())
                .unwrap()
                .front
                .judgment,
            before.front.judgment
        );
    }

    /// 2026-09-05: 自律起票で空ノートを残さない。MCPとCLIの共通口で書込前に止める。
    #[test]
    fn note_text_blank_inputs_leave_db_outbox_and_history_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let tags = ["known".into()];
        let id = vault
            .propose(&conn, text_proposal("元のタイトル", "元の本文", &tags))
            .unwrap();
        let before = text_write_snapshot(&vault, &conn);

        for blank in ["", "   ", "\r\n\t", "\u{3000}\u{00a0}"] {
            for (title, body) in [(blank, "置換本文"), ("置換タイトル", blank)] {
                let error = vault
                    .propose(&conn, text_proposal(title, body, &tags))
                    .unwrap_err();
                assert_eq!(
                    crate::write_rejection::WriteRejection::from_error(&error),
                    Some(crate::write_rejection::WriteRejection::InvalidArgument)
                );
                assert_eq!(text_write_snapshot(&vault, &conn), before);

                let error = vault
                    .agent_update_note(
                        &conn,
                        NoteUpdate {
                            judgment: None,
                            id: &id,
                            title: Some(title),
                            body: Some(body),
                            description: Some("保存しない説明"),
                            tags: None,
                            authority: None,
                            relations: None,
                            allow_new_tags: false,
                            client: "test/text-input",
                        },
                    )
                    .unwrap_err();
                assert_eq!(
                    crate::write_rejection::WriteRejection::from_error(&error),
                    Some(crate::write_rejection::WriteRejection::InvalidArgument)
                );
                assert_eq!(text_write_snapshot(&vault, &conn), before);
            }
        }
    }

    #[test]
    fn note_text_nonblank_inputs_keep_title_spacing_and_body_indentation() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();
        let id = vault
            .propose(
                &conn,
                text_proposal(
                    "  余白を持つ題  ",
                    "    code();\n\n本文\n",
                    &["known".into()],
                ),
            )
            .unwrap();
        let proposed = vault.read_note_from_db(&conn, &id).unwrap();
        assert_eq!(proposed.front.title.as_deref(), Some("  余白を持つ題  "));
        assert_eq!(proposed.body, "    code();\n\n本文\n");

        vault
            .agent_update_note(
                &conn,
                NoteUpdate {
                    judgment: None,
                    id: &id,
                    title: Some("\u{3000}変更後の題\u{3000}"),
                    body: Some("\t変更本文\n"),
                    description: Some(""),
                    tags: None,
                    authority: None,
                    relations: None,
                    allow_new_tags: false,
                    client: "test/text-input",
                },
            )
            .unwrap();
        let updated = vault.read_note_from_db(&conn, &id).unwrap();
        assert_eq!(
            updated.front.title.as_deref(),
            Some("\u{3000}変更後の題\u{3000}")
        );
        assert_eq!(updated.body, "\t変更本文\n");
        assert_eq!(updated.front.description.as_deref(), Some(""));
    }

    /// 2026-09-05: 入力拒否を既存文書全体へ広げると、空ノートの読取と手入れまで止まる。
    #[test]
    fn note_text_legacy_blank_fields_remain_readable_and_allow_metadata_updates() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();

        for (index, title) in [None, Some(""), Some("\u{3000} ")].into_iter().enumerate() {
            let id = format!("notes/legacy-empty-{index}");
            let mut legacy = agent_note("", " \n\t");
            legacy.front.title = title.map(str::to_string);
            legacy.front.tags = vec!["known".into()];
            // 旧版で既に保存された文書を再現する。現行の入力APIから空文書は作らない。
            crate::note_store::put(&vault, &conn, &id, &legacy, "fixture", "fixture").unwrap();
            vault.flush_note_exports(&conn).unwrap();
            let before = vault.read_note_from_db(&conn, &id).unwrap();
            assert!(before.body.trim().is_empty());
            assert_eq!(before.front.title.as_deref(), title);
            assert_eq!(vault.read_note(&id).unwrap().body, before.body);

            vault
                .agent_update_note(
                    &conn,
                    NoteUpdate {
                        judgment: None,
                        id: &id,
                        title: None,
                        body: None,
                        description: Some("既存空ノートの説明"),
                        tags: None,
                        authority: None,
                        relations: None,
                        allow_new_tags: false,
                        client: "test/text-input",
                    },
                )
                .unwrap();
            let updated = vault.read_note_from_db(&conn, &id).unwrap();
            assert_eq!(updated.front.title, before.front.title);
            assert_eq!(updated.body, before.body);
            assert_eq!(
                updated.front.description.as_deref(),
                Some("既存空ノートの説明")
            );
        }
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
                        judgment: None,
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
                    judgment: None,
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
                        judgment: None,
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
                        judgment: None,
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
