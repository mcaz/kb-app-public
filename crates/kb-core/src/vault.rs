//! vault = 1ディレクトリ(内部は git リポ)。正本は Markdown+Git、
//! .kb/ 以下(索引)は再構築可能な派生(原則1)。
//! 規約(frontmatter・index.md・log.md)はアプリが生成し人間に暗記させない(原則7)。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use git2::{Repository, Signature};

use crate::frontmatter::{Frontmatter, Generated, Note, now_iso, today};

pub const NOTES_DIR: &str = "notes";
const RESERVED: &[&str] = &["index.md", "log.md"];

/// 添付(FR-C8)のサイズガード。GitHub 同期(単一ファイル 100MB 上限)を守る。
pub const ATTACH_WARN_BYTES: u64 = 10 * 1024 * 1024;
pub const ATTACH_MAX_BYTES: u64 = 50 * 1024 * 1024;

pub struct Vault {
    pub root: PathBuf,
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

    /// vault を新規作成: git init + .gitignore + index.md + notes/。
    pub fn create(root: impl AsRef<Path>) -> Result<Vault> {
        let root = root.as_ref().to_path_buf();
        if root.join(".git").exists() {
            bail!("{} には既に vault がある", root.display());
        }
        fs::create_dir_all(root.join(NOTES_DIR))?;
        Repository::init(&root).context("git init")?;
        fs::write(root.join(".gitignore"), ".kb/\n")?;
        let vault = Vault { root };
        vault.write_index_md()?;
        vault.commit(&[".gitignore", "index.md"], "vault: initialize")?;
        Ok(vault)
    }

    pub fn index_db_path(&self) -> PathBuf {
        self.root.join(".kb").join("index.db")
    }

    /// ノート ID(パス、.md 抜き)→ 絶対パス。
    pub fn note_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.md"))
    }

    pub fn read_note(&self, id: &str) -> Result<Note> {
        if id.trim().is_empty() {
            bail!("ノート ID が空");
        }
        let path = self.note_path(id);
        let content = fs::read_to_string(&path)
            .with_context(|| format!("ノートが読めない: {}", path.display()))?;
        Note::parse(&content).with_context(|| format!("parse 失敗: {id}"))
    }

    /// 新規ノートを書き込む。ID(notes/<slug>)を返す。git コミットは呼び側で。
    pub fn write_new_note(&self, title: &str, note: &Note) -> Result<String> {
        let slug = slugify(title);
        let mut id = format!("{NOTES_DIR}/{slug}");
        let mut n = 1;
        while self.note_path(&id).exists() {
            n += 1;
            id = format!("{NOTES_DIR}/{slug}-{n}");
        }
        self.write_note(&id, note)?;
        Ok(id)
    }

    pub fn write_note(&self, id: &str, note: &Note) -> Result<()> {
        let file = Path::new(id)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or_default();
        if RESERVED.contains(&format!("{file}.md").as_str()) {
            bail!("{file}.md は予約ファイル名(OKF §3.1)");
        }
        let path = self.note_path(id);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&path, note.to_file_string()?)?;
        Ok(())
    }

    /// 人間のメモを作成(origin: human、status 不在 = stable)。コミットまで行う。
    pub fn new_human_note(&self, title: &str, body: &str, actor: &str) -> Result<String> {
        let mut front = Frontmatter::new_note(title);
        front.origin = Some("human".into());
        front.generated = Some(Generated { by: actor.into(), at: now_iso() });
        let note = Note { front, body: body.to_string() };
        let id = self.write_new_note(title, &note)?;
        self.append_log(&format!("**Creation**: [{title}](/{id}.md) を作成。"))?;
        self.write_index_md()?;
        self.commit_note_op(&id, &format!("note: add {id}"))?;
        Ok(id)
    }

    /// アプリ契約1(docs/contract.md): タグは1〜4個。
    fn validate_tags(tags: &[String]) -> Result<()> {
        if tags.is_empty() || tags.len() > 4 {
            bail!("契約: ノートにはタグを1〜4個付ける(いまは {} 個)。既存の語彙に揃えること", tags.len());
        }
        if let Some(bad) = tags.iter().find(|t| t.trim().is_empty() || t.chars().count() > 20 || t.contains(char::is_whitespace)) {
            bail!("契約: タグは空白を含まない20文字以内の語(不正: 「{bad}」)");
        }
        Ok(())
    }

    /// AI からのノート起票(origin: agent)。FR-C4 propose。
    /// 2026-08-11 改定: draft という特別な状態は持たない — 暫定・要確認といった
    /// 扱いはタグで表現し、その意味づけはユーザーと AI の会話で決まる(運用)。
    pub fn propose(
        &self,
        title: &str,
        body: &str,
        description: Option<&str>,
        tags: &[String],
        client: &str,
    ) -> Result<String> {
        Self::validate_tags(tags)?;
        let mut front = Frontmatter::new_note(title);
        front.origin = Some("agent".into());
        front.description = description.map(|s| s.to_string());
        front.tags = tags.to_vec();
        front.generated = Some(Generated { by: client.into(), at: now_iso() });
        front.sources = Some(serde_yaml::from_str(&format!(
            "[{{ resource: \"conversation:{client}/{}\" }}]",
            today()
        ))?);
        let note = Note { front, body: body.to_string() };
        let id = self.write_new_note(title, &note)?;
        self.append_log(&format!(
            "**Proposal**: [{title}](/{id}.md) を起票(via {client})。"
        ))?;
        self.write_index_md()?;
        self.commit_note_op(&id, &format!("propose {id} (via {client})"))?;
        Ok(id)
    }

    /// 所有ガード(2026-08-10 改定: 所有は「生まれ」で決まる・対称)。
    /// human ノート=人間の領分(AI は読むだけ)/ agent ノート=AI の領分(人間は読むだけ)。
    fn require_origin(&self, id: &str, expected: &str, deny_msg: &str) -> Result<Note> {
        let note = self.read_note(id)?;
        let origin = note.front.origin.as_deref().unwrap_or("human");
        if origin != expected {
            bail!("{deny_msg}");
        }
        Ok(note)
    }

    /// ノートの編集(GUI エディタの保存)。タイトル・本文を更新し generated を更新。
    /// AI のノート(origin: agent)は人間からは編集不可 — 越境は make_mine で。
    pub fn edit_note(&self, id: &str, title: &str, body: &str, actor: &str) -> Result<()> {
        let mut note = self.require_origin(
            id,
            "human",
            "AI のノートは AI が管理する(編集したいときは Claude に依頼するか「自分のメモにする」で引き取る)",
        )?;
        note.front.title = Some(title.to_string());
        note.front.generated = Some(Generated { by: actor.into(), at: now_iso() });
        note.body = body.to_string();
        self.write_note(id, &note)?;
        self.write_index_md()?;
        self.commit_note_op(id, &format!("note: edit {id}"))?;
        Ok(())
    }

    /// ノートの削除(GUI/CLI = 人間側)。AI のノートは削除不可(AI 自身が remove する)。
    pub fn delete_note(&self, id: &str) -> Result<()> {
        self.require_origin(id, "human", "AI のノートは AI が管理する(削除したいときは Claude に依頼)")?;
        self.delete_note_inner(id, &format!("note: delete {id}"))
    }

    /// AI 自身によるノート削除(MCP)。自分のノート(origin: agent)のみ。
    pub fn agent_delete_note(&self, id: &str, client: &str) -> Result<()> {
        self.require_origin(id, "agent", "ユーザーのメモは削除できない(読むだけ — 原則9)")?;
        self.delete_note_inner(id, &format!("note: delete {id} (via {client})"))
    }

    /// AI 自身によるノート更新(MCP)。自分のノート(origin: agent)のみ。
    pub fn agent_update_note(
        &self,
        id: &str,
        title: Option<&str>,
        body: Option<&str>,
        description: Option<&str>,
        tags: Option<&[String]>,
        client: &str,
    ) -> Result<()> {
        let mut note = self.require_origin(id, "agent", "ユーザーのメモは編集できない(読むだけ — 原則9)")?;
        if let Some(t) = title {
            note.front.title = Some(t.to_string());
        }
        if let Some(d) = description {
            note.front.description = Some(d.to_string());
        }
        if let Some(ts) = tags {
            Self::validate_tags(ts)?; // 契約1: タグの全消し・過多は不可
            note.front.tags = ts.to_vec();
        }
        if let Some(b) = body {
            note.body = b.to_string();
        }
        note.front.generated = Some(Generated { by: client.into(), at: now_iso() });
        self.write_note(id, &note)?;
        let t = note.front.title.as_deref().unwrap_or(id);
        self.append_log(&format!("**Update**: [{t}](/{id}.md) を AI が更新(via {client})。"))?;
        self.write_index_md()?;
        self.commit_note_op(id, &format!("note: update {id} (via {client})"))?;
        Ok(())
    }

    /// 越境(原則9): AI のノートを「自分のメモにする」— origin を human へ。
    /// 以後は人間の領分(編集・削除可、AI は読むだけ)。
    pub fn make_mine(&self, id: &str) -> Result<()> {
        let mut note = self.require_origin(id, "agent", "もともとあなたのメモです")?;
        note.front.origin = Some("human".into());
        self.write_note(id, &note)?;
        let t = note.front.title.clone().unwrap_or_else(|| id.to_string());
        self.append_log(&format!("**Ownership**: [{t}](/{id}.md) を自分のメモにした。"))?;
        self.commit_note_op(id, &format!("note: make-mine {id}"))?;
        Ok(())
    }

    fn delete_note_inner(&self, id: &str, message: &str) -> Result<()> {
        let title = self
            .read_note(id)
            .ok()
            .and_then(|n| n.front.title)
            .unwrap_or_else(|| id.to_string());
        let repo = Repository::open(&self.root)?;
        let mut index = repo.index()?;
        index.remove_path(Path::new(&format!("{id}.md")))?;
        let _ = index.remove_dir(Path::new(&format!("{id}.files")), 0);
        index.write()?;
        fs::remove_file(self.note_path(id))?;
        let _ = fs::remove_dir_all(self.attach_dir(id));
        self.write_index_md()?;
        self.append_log(&format!("**Deletion**: 「{title}」({id})を削除。"))?;
        self.commit(&["index.md", "log.md"], message)?;
        crate::connect::auto_push(self);
        Ok(())
    }

    /// 退役(status: deprecated)。ファイルは消さない(OKF §5.4: kept for links and history)。
    pub fn archive(&self, id: &str) -> Result<()> {
        let mut note = self.read_note(id)?;
        note.front.status = Some(crate::frontmatter::STATUS_DEPRECATED.into());
        self.write_note(id, &note)?;
        let title = note.front.title.as_deref().unwrap_or(id);
        self.append_log(&format!("**Deprecation**: [{title}](/{id}.md) を退役。"))?;
        self.write_index_md()?;
        self.commit_note_op(id, &format!("archive {id}"))?;
        Ok(())
    }

    /// 添付ディレクトリ(FR-C8: `<id>.files/` サイドカー)。ノート=1ファイルの
    /// OKF 互換を保ったまま、添付の「1ノート=1単位」管理はアプリが保証する。
    pub fn attach_dir(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.files"))
    }

    /// 添付を追加。戻り値 = (保存名, サイズ警告)。
    pub fn add_attachment(&self, id: &str, name: &str, data: &[u8]) -> Result<(String, Option<String>)> {
        if !self.note_path(id).exists() {
            bail!("ノートが無い: {id}");
        }
        let size = data.len() as u64;
        if size > ATTACH_MAX_BYTES {
            bail!("50MB を超えるファイルは添付できない({} MB)", size / 1024 / 1024);
        }
        let warning = (size > ATTACH_WARN_BYTES)
            .then(|| format!("大きな添付({} MB)— 同期に時間がかかることがある", size / 1024 / 1024));
        // パス潜り対策: ファイル名成分のみ使用
        let base = Path::new(name)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("file")
            .to_string();
        let dir = self.attach_dir(id);
        fs::create_dir_all(&dir)?;
        let mut saved = base.clone();
        let mut n = 1;
        while dir.join(&saved).exists() {
            n += 1;
            let p = Path::new(&base);
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
            let ext = p.extension().and_then(|e| e.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
            saved = format!("{stem}-{n}{ext}");
        }
        fs::write(dir.join(&saved), data)?;
        self.touch_note(id); // 添付変更を索引の mtime 検知に乗せる
        let title = self.read_note(id).ok().and_then(|n| n.front.title).unwrap_or_else(|| id.into());
        self.append_log(&format!("**Attachment**: [{title}](/{id}.md) に {saved} を添付。"))?;
        self.commit(
            &[&format!("{id}.files/{saved}"), &format!("{id}.md"), "log.md"],
            &format!("note: attach {id} {saved}"),
        )?;
        crate::connect::auto_push(self);
        Ok((saved, warning))
    }

    /// 添付一覧 (名前, バイト数)。
    pub fn list_attachments(&self, id: &str) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        if let Ok(entries) = fs::read_dir(self.attach_dir(id)) {
            for e in entries.flatten() {
                if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                    out.push((e.file_name().to_string_lossy().to_string(), size));
                }
            }
        }
        out.sort();
        out
    }

    /// 添付の削除(git 履歴には残る)。
    pub fn remove_attachment(&self, id: &str, name: &str) -> Result<()> {
        let base = Path::new(name).file_name().and_then(|f| f.to_str()).unwrap_or_default();
        let path = self.attach_dir(id).join(base);
        if !path.exists() {
            bail!("添付が無い: {base}");
        }
        fs::remove_file(&path)?;
        self.touch_note(id);
        {
            let repo = Repository::open(&self.root)?;
            let mut index = repo.index()?;
            index.remove_path(Path::new(&format!("{id}.files/{base}")))?;
            index.write()?;
        }
        self.append_log(&format!("**Attachment**: /{id}.md の添付 {base} を削除。"))?;
        self.commit(&[&format!("{id}.md"), "log.md"], &format!("note: detach {id} {base}"))?;
        crate::connect::auto_push(self);
        Ok(())
    }

    fn touch_note(&self, id: &str) {
        if let Ok(f) = fs::File::options().write(true).open(self.note_path(id)) {
            let _ = f.set_modified(std::time::SystemTime::now());
        }
    }

    /// 全ノートの (id, 絶対パス)。予約ファイル・.kb・.git・添付(*.files)は除外。
    pub fn list_note_files(&self) -> Vec<(String, PathBuf)> {
        let mut out = Vec::new();
        for entry in walkdir::WalkDir::new(&self.root)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                name != ".git"
                    && name != ".kb"
                    && !(e.file_type().is_dir() && name.ends_with(".files"))
            })
            .flatten()
        {
            let path = entry.path();
            if !entry.file_type().is_file() || path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if RESERVED.contains(&name.as_ref()) {
                continue;
            }
            if let Ok(rel) = path.strip_prefix(&self.root) {
                let id = rel.with_extension("");
                out.push((id.to_string_lossy().to_string(), path.to_path_buf()));
            }
        }
        out.sort();
        out
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
        for (id, path) in self.list_note_files() {
            let Ok(content) = fs::read_to_string(&path) else { continue };
            let Ok(note) = Note::parse(&content) else { continue };
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
            format!("{}\n* {entry}{}", &existing[..insert_at], &existing[insert_at..])
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
        self.commit(&[&format!("{id}.md"), "index.md", "log.md"], message)?;
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
    if out.is_empty() { "note".to_string() } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("認証 設計 メモ"), "認証-設計-メモ");
        assert_eq!(slugify("!!!"), "note");
    }

    #[test]
    fn attachment_roundtrip_and_guard() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let id = vault.new_human_note("添付テスト", "本文。", "human:o").unwrap();
        // 追加(同名は連番)・一覧
        let (a, warn) = vault.add_attachment(&id, "図.png", b"png-bytes").unwrap();
        assert_eq!(a, "図.png");
        assert!(warn.is_none());
        let (b, _) = vault.add_attachment(&id, "図.png", b"png-bytes-2").unwrap();
        assert_eq!(b, "図-2.png");
        assert_eq!(vault.list_attachments(&id).len(), 2);
        // パス潜りは basename に落ちる
        let (c, _) = vault.add_attachment(&id, "../../etc/passwd", b"x").unwrap();
        assert_eq!(c, "passwd");
        // サイズ拒否
        let big = vec![0u8; (ATTACH_MAX_BYTES + 1) as usize];
        assert!(vault.add_attachment(&id, "big.bin", &big).is_err());
        // 添付ディレクトリはノート走査に映らない(OKF 互換の保全)
        std::fs::write(vault.attach_dir(&id).join("紛れ.md"), "---\ntype: Note\n---\nx").unwrap();
        assert_eq!(vault.list_note_files().len(), 1);
        // 削除(残り = 図.png・passwd・紛れ.md の3つ。紛れ.md は「添付」としては見える)
        vault.remove_attachment(&id, "図-2.png").unwrap();
        assert_eq!(vault.list_attachments(&id).len(), 3);
    }

    /// 所有の対称性(2026-08-10 改定): human ノートは人間のみ・agent ノートは AI のみが編集・削除。
    #[test]
    fn ownership_symmetry() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let mine = vault.new_human_note("俺のメモ", "本文", "human:owner").unwrap();
        let ai = vault.propose("AI の知見", "本文", None, &["dev".into()], "claude/x").unwrap();

        // human ノート: 人間は可・AI は不可
        assert!(vault.edit_note(&mine, "俺のメモ", "編集後", "human:owner").is_ok());
        assert!(vault.agent_update_note(&mine, None, Some("侵入"), None, None, "claude/x").is_err());
        assert!(vault.agent_delete_note(&mine, "claude/x").is_err());

        // agent ノート: AI は可・人間は不可
        assert!(vault.edit_note(&ai, "AI の知見", "人間の編集", "human:owner").is_err());
        assert!(vault.delete_note(&ai).is_err());
        assert!(vault.agent_update_note(&ai, Some("AI の知見 v2"), None, None, None, "claude/x").is_ok());

        // 越境: 自分のメモにする → 領分が反転
        vault.make_mine(&ai).unwrap();
        assert!(vault.edit_note(&ai, "引き取り", "人間の編集", "human:owner").is_ok());
        assert!(vault.agent_update_note(&ai, None, Some("もう触れない"), None, None, "claude/x").is_err());

        // AI は自分のノートを消せる
        let ai2 = vault.propose("捨てる知見", "本文", None, &["dev".into()], "claude/x").unwrap();
        assert!(vault.agent_delete_note(&ai2, "claude/x").is_ok());
    }

    #[test]
    fn create_and_propose_flow() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let id = vault
            .propose("テスト起票", "本文です。", Some("説明"), &["dev".into()], "test-client/model")
            .unwrap();
        let note = vault.read_note(&id).unwrap();
        // draft という特別な状態は持たない(2026-08-11 改定)
        assert_eq!(note.front.effective_status(), "stable");
        assert_eq!(note.front.origin.as_deref(), Some("agent"));
        assert_eq!(note.front.tags, vec!["dev".to_string()]);
        // index.md / log.md が生成され、予約名はノート一覧に出ない
        assert!(vault.root.join("index.md").exists());
        assert!(vault.root.join("log.md").exists());
        assert_eq!(vault.list_note_files().len(), 1);
    }
}
