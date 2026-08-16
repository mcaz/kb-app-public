//! vault = 1ディレクトリ(内部は git リポ)。現行の保存 adapter は Markdown+Git、
//! .kb/ 以下(索引)は再構築可能な派生(Storage Contract / ADR-0004)。
//! 規約(frontmatter・index.md・log.md)はアプリが生成し人間に暗記させない(原則7)。

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use git2::{Repository, Signature};

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

    pub fn read_note(&self, raw: &str) -> Result<Note> {
        let id = NoteId::parse(raw)?;
        let path = self.checked_existing_path(&id.markdown_relative_path())?;
        let content = fs::read_to_string(&path)
            .with_context(|| format!("ノートが読めない: {}", path.display()))?;
        Note::parse(&content).with_context(|| format!("parse 失敗: {id}"))
    }

    /// 新規ノートを書き込む。ID(notes/<slug>)を返す。git コミットは呼び側で。
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
        fs::write(&path, note.to_file_string()?)?;
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
            allow_new_tags,
            client,
        } = proposal;
        crate::tags::validate(conn, tags, allow_new_tags)?;
        let mut front = Frontmatter::new_note(title);
        front.origin = Some("agent".into());
        front.created = Some(now_iso());
        front.description = description.map(|s| s.to_string());
        front.tags = tags.to_vec();
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
        let id = self.write_new_note(title, &note)?;
        self.append_log(&format!(
            "**Proposal**: [{title}](/{id}.md) を起票(via {client})。"
        ))?;
        self.write_index_md()?;
        self.commit_note_op(&id, &format!("propose {id} (via {client})"))?;
        Ok(id)
    }

    /// 所有ガード。現行ノートは agent 所有で、旧 human ノートは互換読み取り専用。
    fn require_origin(&self, id: &str, expected: &str, deny_msg: &str) -> Result<Note> {
        let note = self.read_note(id)?;
        let origin = note.front.origin.as_deref().unwrap_or("human");
        if origin != expected {
            bail!("{deny_msg}");
        }
        Ok(note)
    }

    /// AI 自身によるノート削除(MCP)。自分のノート(origin: agent)のみ。
    pub fn agent_delete_note(&self, id: &str, client: &str) -> Result<()> {
        self.require_origin(
            id,
            "agent",
            "旧 human ノートは削除できない(互換読み取り専用)",
        )?;
        self.delete_note_inner(id, &format!("note: delete {id} (via {client})"))
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
            allow_new_tags,
            client,
        } = update;
        let mut note = self.require_origin(
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
        if let Some(b) = body {
            note.body = b.to_string();
        }
        note.front.generated = Some(Generated {
            by: client.into(),
            at: now_iso(),
        });
        self.write_note(id, &note)?;
        let t = note.front.title.as_deref().unwrap_or(id);
        self.append_log(&format!(
            "**Update**: [{t}](/{id}.md) を AI が更新(via {client})。"
        ))?;
        self.write_index_md()?;
        self.commit_note_op(id, &format!("note: update {id} (via {client})"))?;
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

    fn delete_note_inner(&self, id: &str, message: &str) -> Result<()> {
        let id = NoteId::parse(id)?;
        let note_relative = id.markdown_relative_path();
        let attachments_relative = id.attachments_relative_path();
        let note_path = self.checked_existing_path(&note_relative)?;
        let attachments = self.checked_optional_path(&attachments_relative)?;
        let title = self
            .read_note(id.as_str())
            .ok()
            .and_then(|n| n.front.title)
            .unwrap_or_else(|| id.to_string());
        let repo = Repository::open(&self.root)?;
        let mut index = repo.index()?;
        index.remove_path(&note_relative)?;
        let _ = index.remove_dir(&attachments_relative, 0);
        index.write()?;
        fs::remove_file(note_path)?;
        if let Some(attachments) = attachments {
            // 旧実装と同じく、ノート本体の削除は旧添付の後始末失敗で巻き戻せない。
            let _ = fs::remove_dir_all(attachments);
        }
        self.write_index_md()?;
        self.append_log(&format!("**Deletion**: 「{title}」({id})を削除。"))?;
        self.commit(&["index.md", "log.md"], message)?;
        crate::connect::auto_push(self);
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
        // 随時 push(FR-A6 改定)。remote 未設定なら no-op、失敗しても操作は成功のまま
        crate::connect::auto_push(self);
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
                            allow_new_tags: false,
                            client: "test/client",
                        },
                    )
                    .is_err(),
                "更新できてはいけない: {id}"
            );
            assert!(
                vault.agent_delete_note(id, "test/client").is_err(),
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
                        allow_new_tags: false,
                        client: "test/client",
                    },
                )
                .is_err()
        );
        assert!(
            vault
                .agent_delete_note("notes/linked", "test/client")
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
                        allow_new_tags: false,
                        client: "claude/x",
                    },
                )
                .is_err()
        );
        assert!(vault.agent_delete_note(&legacy, "claude/x").is_err());

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
        assert!(vault.agent_delete_note(&ai2, "claude/x").is_ok());
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
