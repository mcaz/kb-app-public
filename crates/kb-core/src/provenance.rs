//! ノートの来歴 — 誰が・いつ・どの部分を・なぜ書いたかの追記専用イベント台帳(契約20)。
//!
//! frontmatter の `generated` は「最後の書き手」1件しか持てず、次の書き手が上書きすると
//! 前の書き手が消える。履歴を本文へ書き戻すと蒸留・検索の対象が汚れるので、正本は
//! ノートと分離した `.kb-events/YYYY-MM.jsonl`(追記専用・merge=union)に置き、
//! DB の `note_events` を実行時の索引にする。判断の背景は
//! [ADR-0023](../../../docs/adr/0023-note-provenance-events.md)。
//!
//! イベントはコアの書込経路([`crate::note_store`])だけが発行する。既存行の改変・削除は
//! コアの操作として持たない — 訂正は新しいイベントを足して表す。

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::authority::NoteUid;
use crate::client_surface::ClientSurface;
use crate::frontmatter::{Frontmatter, Note, now_iso};
use crate::vault::Vault;

/// 来歴イベントの正本ディレクトリ(vault 直下・Git 追跡下)。
pub const EVENTS_DIR: &str = ".kb-events";
/// イベント1件の schema version。読み手は未知キーを無視し、増えた版も落とさない。
const EVENT_VERSION: u8 = 1;
/// 本文 diff の上限 byte 数。1回の書込で 8KiB を超える改稿は diff を持たず、
/// 前後の document hash と Git 履歴で追う(イベント台帳を本文の複製にしない)。
const MAX_BODY_DIFF: usize = 8_192;
/// 最初の見出しより前の本文につける見出し名。
const PREAMBLE: &str = "(前文)";
/// summary / reason / origin_claim の上限。削除理由(vault.rs)と同じ1行500文字に揃える。
const MAX_TEXT: usize = 500;

// ------------------------------------------------------------------ actor

/// モデル名をどこから得たか。文字列の見た目が同じでも、確からしさは同じではない。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelBasis {
    /// MCP の initialize handshake で相手が名乗った(Phase 2)。
    Handshake,
    /// アプリ自身が呼び出したAPIの指定モデル。
    AppApi,
    /// 会話中にモデルが自己申告した。
    SelfReported,
    /// 接続設定(`--client`)の値。
    Config,
    #[default]
    Unknown,
}

/// クライアント製品名をどこから得たか。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientBasis {
    Handshake,
    Config,
    #[default]
    Unknown,
}

fn unknown_surface() -> ClientSurface {
    ClientSurface::Unknown
}

/// 1回の書込を行った書き手。製品面(`surface`)は既存の厳密変換を再利用し、
/// 曖昧な部分一致へ戻さない([`ClientSurface::from_hint`])。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WriteActor {
    /// 製品名。例: `claude-code` / `codex-cli` / `kb-app-distillation`。
    pub client: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
    #[serde(default)]
    pub client_basis: ClientBasis,
    #[serde(default = "unknown_surface")]
    pub surface: ClientSurface,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// モデルの動作設定(推論強度・思考モード・ペルソナ名など)。モデル名だけでは
    /// 「どのモデルのどの設定で書いたか」が分からない(2026-09-10 本人指摘)。
    /// 根拠(basis)はモデルと共有し、モデル無しでは持たない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default)]
    pub model_basis: ModelBasis,
}

impl WriteActor {
    /// `--client` の hint 文字列から作る。先頭 segment が製品名、2番目があればモデル、
    /// 3番目があればそのモデルの動作設定(`codex-cli/gpt-5-codex/medium`)。
    /// いずれも接続設定由来なので `Config` 止まりで、確定扱いにしない。
    pub fn from_client_hint(hint: &str) -> Self {
        let mut segments = hint.split('/');
        let client = segments.next().unwrap_or_default().trim();
        let model = segments.next().unwrap_or_default().trim();
        let mode = segments.next().unwrap_or_default().trim();
        WriteActor {
            client: if client.is_empty() {
                "unknown".to_string()
            } else {
                client.to_string()
            },
            client_version: None,
            client_basis: if client.is_empty() {
                ClientBasis::Unknown
            } else {
                ClientBasis::Config
            },
            surface: ClientSurface::from_hint(hint),
            model: (!model.is_empty()).then(|| model.to_string()),
            mode: (!model.is_empty() && !mode.is_empty()).then(|| mode.to_string()),
            model_basis: if model.is_empty() {
                ModelBasis::Unknown
            } else {
                ModelBasis::Config
            },
        }
    }

    /// アプリ主導の書込(蒸留・initiative closure)。モデルは呼び出し元の構造から渡す。
    /// hint 文字列の2番目の segment を機械的に読むと、
    /// `kb-app-distillation/requested/…/gpt-5.6-sol/…` のような形で別の語を掴む。
    pub fn app_api(client: &str, model: Option<&str>) -> Self {
        let client = client.trim();
        WriteActor {
            client: if client.is_empty() {
                "kb-app".to_string()
            } else {
                client.to_string()
            },
            client_version: None,
            client_basis: ClientBasis::Config,
            surface: ClientSurface::from_hint(client),
            model: model.map(str::to_string),
            mode: None,
            model_basis: if model.is_some() {
                ModelBasis::AppApi
            } else {
                ModelBasis::Unknown
            },
        }
    }

    /// MCP の initialize で相手が名乗った製品名・版で上書きする(Phase 2 の入口)。
    pub fn with_handshake(mut self, name: &str, version: Option<&str>) -> Self {
        let name = name.trim();
        if !name.is_empty() {
            self.client = name.to_string();
            self.client_basis = ClientBasis::Handshake;
            self.surface = ClientSurface::from_hint(name);
        }
        self.client_version = version
            .map(str::trim)
            .filter(|version| !version.is_empty())
            .map(str::to_string);
        self
    }

    /// 会話中の自己申告(モデルと、あればその動作設定)を載せる。設定値より新しいが、確定ではない。
    /// 設定値の動作設定は設定値のモデルに付いていたものなので、モデルを申告し直したら
    /// 動作設定も申告の値へ置き換える(申告が無ければ空 — 別のモデルの設定を引き継がない)。
    pub fn with_self_report(mut self, model: &str, mode: Option<&str>) -> Self {
        let model = model.trim();
        if !model.is_empty() {
            self.model = Some(model.to_string());
            self.model_basis = ModelBasis::SelfReported;
            self.mode = mode
                .map(str::trim)
                .filter(|mode| !mode.is_empty())
                .map(str::to_string);
        }
        self
    }

    /// 書き手の同一性キー(client + model)。`distinct_actors` の数え方の正本。
    pub fn identity(&self) -> String {
        format!("{}/{}", self.client, self.model.as_deref().unwrap_or(""))
    }

    /// 1行要約用の表示名。モデル不明を空欄で誤魔化さない。動作設定があれば
    /// モデルの後ろに空白区切りで続ける(例 `codex-cli/gpt-6-codex Astra medium(自己申告)`)。
    pub fn label(&self) -> String {
        match self.model_with_mode() {
            Some(model) => format!("{}/{model}({})", self.client, self.model_basis.label()),
            None => format!("{}/モデル不明", self.client),
        }
    }

    /// `<モデル> <動作設定>` の表示形。モデルが無ければ動作設定も出さない。
    fn model_with_mode(&self) -> Option<String> {
        let model = self.model.as_deref()?;
        Some(match self.mode.as_deref() {
            Some(mode) => format!("{model} {mode}"),
            None => model.to_string(),
        })
    }

    /// frontmatter `generated.by` の actor 文字列。OKF §7 の `<クライアント>/<モデル>` に
    /// 動作設定を空白区切りで続けた形(例 `codex-cli/gpt-6-codex Astra medium`)。
    /// 先頭 segment は `ClientSurface::from_hint` が読むので、handshake で名乗った名前
    /// ではなく接続設定 hint の製品名を置く。モデル不明なら製品名だけ。
    pub fn generated_by(&self, client_hint: &str) -> String {
        let product = client_product(client_hint);
        match self.model_with_mode() {
            Some(model) => format!("{product}/{model}"),
            None => product.to_string(),
        }
    }

    #[cfg(test)]
    pub fn test() -> Self {
        WriteActor::from_client_hint("test-client/test-model")
    }
}

/// hint文字列から製品名segmentだけを取り出す。モデルの位置は面ごとに違うので読まない。
pub fn client_product(hint: &str) -> &str {
    let product = hint.split('/').next().unwrap_or_default().trim();
    if product.is_empty() {
        "unknown"
    } else {
        product
    }
}

impl ModelBasis {
    fn label(self) -> &'static str {
        match self {
            // handshake も相手の名乗りであって、こちらが確かめた事実ではない。
            ModelBasis::Handshake | ModelBasis::SelfReported => "自己申告",
            ModelBasis::AppApi => "アプリ確定",
            ModelBasis::Config => "設定値",
            ModelBasis::Unknown => "不明",
        }
    }
}

// --------------------------------------------------------------- revision

/// この書込が前の版に対して何をしたか。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionKind {
    Create,
    Amend,
    Reverse,
    Correct,
    Normalize,
    Remove,
    #[default]
    Unknown,
}

/// どの経路の書込か。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Propose,
    Update,
    Remove,
    Distill,
    Closure,
    Import,
    HumanEdit,
    #[default]
    Other,
}

impl RevisionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RevisionKind::Create => "create",
            RevisionKind::Amend => "amend",
            RevisionKind::Reverse => "reverse",
            RevisionKind::Correct => "correct",
            RevisionKind::Normalize => "normalize",
            RevisionKind::Remove => "remove",
            RevisionKind::Unknown => "unknown",
        }
    }
}

impl Operation {
    pub fn as_str(self) -> &'static str {
        match self {
            Operation::Propose => "propose",
            Operation::Update => "update",
            Operation::Remove => "remove",
            Operation::Distill => "distill",
            Operation::Closure => "closure",
            Operation::Import => "import",
            Operation::HumanEdit => "human_edit",
            Operation::Other => "other",
        }
    }
}

/// 書き手が申告する改版の意図。空欄でも書込は通す(強制すると経路が迂回される)。
#[derive(Clone, Debug, Default)]
pub struct RevisionInput {
    pub kind: Option<RevisionKind>,
    pub summary: Option<String>,
    pub reason: Option<String>,
    pub evidence: Vec<String>,
    pub origin_claim: Option<String>,
}

impl RevisionInput {
    pub fn validate(&self) -> Result<()> {
        for (field, value) in [
            ("summary", &self.summary),
            ("reason", &self.reason),
            ("origin_claim", &self.origin_claim),
        ] {
            let Some(value) = value else { continue };
            let trimmed = value.trim();
            if trimmed.is_empty() || trimmed.chars().count() > MAX_TEXT {
                bail!("{field}は1〜{MAX_TEXT}文字で指定する");
            }
            if trimmed.contains(['\n', '\r']) {
                bail!("{field}は改行を含めない");
            }
        }
        for evidence in &self.evidence {
            NoteUid::from_str(evidence.trim())
                .with_context(|| format!("evidenceはnote_uidで指定する: {evidence}"))?;
        }
        Ok(())
    }

    fn trimmed(value: &Option<String>) -> Option<String> {
        value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }
}

/// 1回の書込に添える来歴の入力一式。
pub struct WriteContext<'a> {
    pub actor: &'a WriteActor,
    pub revision: Option<&'a RevisionInput>,
    pub operation: Operation,
}

impl WriteContext<'_> {
    fn kind(&self, creating: bool) -> RevisionKind {
        match self.revision.and_then(|revision| revision.kind) {
            Some(kind) => kind,
            None if creating => RevisionKind::Create,
            None => RevisionKind::Unknown,
        }
    }
}

/// テスト用の既定context。実データを触るテストが来歴の作り込みを毎回書かずに済ませる。
#[cfg(test)]
pub fn test_context() -> WriteContext<'static> {
    static ACTOR: std::sync::OnceLock<WriteActor> = std::sync::OnceLock::new();
    WriteContext {
        actor: ACTOR.get_or_init(WriteActor::test),
        revision: None,
        operation: Operation::Other,
    }
}

// ------------------------------------------------------------------ event

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FieldChange {
    pub from: serde_json::Value,
    pub to: serde_json::Value,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// ノート1件・書込1回の来歴。`event_id` は export outbox の `op_id` と同じ値で、
/// Markdown 出力・commit・DB 行を同じ操作として突き合わせられる。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NoteEvent {
    pub v: u8,
    pub event_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_uid: Option<String>,
    pub note_id: String,
    pub at: String,
    pub operation: Operation,
    pub actor: WriteActor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_claim: Option<String>,
    pub kind: RevisionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub changes: BTreeMap<String, FieldChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_diff: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub diff_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_hash: Option<String>,
}

impl NoteEvent {
    /// 新規・更新の書込から作る。前後の document を比べるだけで、本文は複製しない。
    pub fn for_upsert(
        op_id: &str,
        note_id: &str,
        base_document: Option<&str>,
        document: &str,
        context: &WriteContext<'_>,
    ) -> Result<NoteEvent> {
        let note = Note::parse(document)
            .with_context(|| format!("来歴イベントの対象documentをparseできない: {note_id}"))?;
        let base = base_document
            .map(Note::parse)
            .transpose()
            .with_context(|| format!("来歴イベントの変更前documentをparseできない: {note_id}"))?;
        let before_body = base.as_ref().map(|note| note.body.as_str()).unwrap_or("");
        let (body_diff, diff_truncated) = match &base {
            Some(_) => body_diff(before_body, &note.body)?,
            // 新規は差分ではなく document 自体が全部なので diff を持たない。
            None => (None, false),
        };
        let changes = match &base {
            Some(base) => frontmatter_changes(&base.front, &note.front)?,
            None => BTreeMap::new(),
        };
        Ok(NoteEvent {
            v: EVENT_VERSION,
            event_id: op_id.to_string(),
            note_uid: note.front.note_uid.as_ref().map(NoteUid::to_string),
            note_id: note_id.to_string(),
            at: now_iso(),
            operation: context.operation,
            actor: context.actor.clone(),
            origin_claim: context
                .revision
                .and_then(|revision| RevisionInput::trimmed(&revision.origin_claim)),
            kind: context.kind(base.is_none()),
            summary: context
                .revision
                .and_then(|revision| RevisionInput::trimmed(&revision.summary)),
            reason: context
                .revision
                .and_then(|revision| RevisionInput::trimmed(&revision.reason)),
            evidence: context
                .revision
                .map(|revision| revision.evidence.clone())
                .unwrap_or_default(),
            sections: changed_sections(before_body, &note.body),
            changes,
            body_diff,
            diff_truncated,
            base_hash: base_document.map(document_hash),
            doc_hash: Some(document_hash(document)),
        })
    }

    /// 削除の書込から作る。消えた見出しを全部残し、後から「何が失われたか」を引けるようにする。
    pub fn for_remove(
        op_id: &str,
        note_id: &str,
        note_uid: Option<&str>,
        base_document: &str,
        context: &WriteContext<'_>,
    ) -> Result<NoteEvent> {
        let base = Note::parse(base_document)
            .with_context(|| format!("来歴イベントの削除対象をparseできない: {note_id}"))?;
        Ok(NoteEvent {
            v: EVENT_VERSION,
            event_id: op_id.to_string(),
            note_uid: note_uid
                .map(str::to_string)
                .or_else(|| base.front.note_uid.as_ref().map(NoteUid::to_string)),
            note_id: note_id.to_string(),
            at: now_iso(),
            operation: Operation::Remove,
            actor: context.actor.clone(),
            origin_claim: context
                .revision
                .and_then(|revision| RevisionInput::trimmed(&revision.origin_claim)),
            kind: RevisionKind::Remove,
            summary: context
                .revision
                .and_then(|revision| RevisionInput::trimmed(&revision.summary)),
            reason: context
                .revision
                .and_then(|revision| RevisionInput::trimmed(&revision.reason)),
            evidence: context
                .revision
                .map(|revision| revision.evidence.clone())
                .unwrap_or_default(),
            sections: section_entries(&base.body)
                .into_iter()
                .map(|(heading, _)| heading)
                .collect(),
            changes: BTreeMap::new(),
            body_diff: None,
            diff_truncated: false,
            base_hash: Some(document_hash(base_document)),
            doc_hash: None,
        })
    }
}

/// 来歴イベントの検索テキスト。申告文(要約・理由)と、動いた見出し・改版種別だけを
/// 入れる。本文は入れない — ノート本文は`fts_main`が既に索引しており、
/// ここで重ねると同じ文が二重に効いて順位が歪む。
pub fn event_search_text(event: &NoteEvent) -> String {
    let mut parts = Vec::new();
    if let Some(summary) = &event.summary {
        parts.push(summary.clone());
    }
    if let Some(reason) = &event.reason {
        parts.push(reason.clone());
    }
    parts.extend(event.sections.iter().cloned());
    parts.push(event.kind.as_str().to_string());
    parts.join(" ")
}

/// document(frontmatter込みのファイル全文)の内容 hash。
/// Markdown 出力の衝突検査と来歴の突き合わせが同じ式を使う。
pub fn document_hash(document: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(document.as_bytes()))
}

fn is_heading(line: &str) -> bool {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes) && line[hashes..].starts_with([' ', '\t'])
}

/// 本文を見出し単位へ切る。同じ見出し行が複数あるときは1つへまとめる
/// (見出しの重複はノート側の問題で、来歴の見出し名を機械的に増やす理由にしない)。
fn section_entries(body: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut heading = PREAMBLE.to_string();
    let mut buffer = String::new();
    for line in body.lines() {
        if is_heading(line) {
            push_section(&mut out, &heading, &buffer);
            heading = line.trim().to_string();
            buffer.clear();
        } else {
            buffer.push_str(line);
            buffer.push('\n');
        }
    }
    push_section(&mut out, &heading, &buffer);
    out
}

fn push_section(out: &mut Vec<(String, String)>, heading: &str, buffer: &str) {
    let content = buffer.trim_end().to_string();
    if heading == PREAMBLE && content.is_empty() {
        return;
    }
    match out.iter_mut().find(|(existing, _)| existing == heading) {
        Some((_, existing)) => {
            existing.push('\n');
            existing.push_str(&content);
        }
        None => out.push((heading.to_string(), content)),
    }
}

/// 現在の本文に存在する見出し(本文順)。来歴要約を今ある見出しへ絞るのに使う。
pub fn body_headings(body: &str) -> Vec<String> {
    section_entries(body)
        .into_iter()
        .map(|(heading, _)| heading)
        .collect()
}

/// 追加・変更された見出しを新しい本文の順で並べ、最後に消えた見出しを旧本文の順で足す。
fn changed_sections(before: &str, after: &str) -> Vec<String> {
    let old = section_entries(before);
    let new = section_entries(after);
    let old_map: BTreeMap<&str, &str> = old
        .iter()
        .map(|(heading, content)| (heading.as_str(), content.as_str()))
        .collect();
    let new_map: BTreeMap<&str, &str> = new
        .iter()
        .map(|(heading, content)| (heading.as_str(), content.as_str()))
        .collect();
    let mut out = Vec::new();
    for (heading, content) in &new {
        if old_map.get(heading.as_str()) != Some(&content.as_str()) {
            out.push(heading.clone());
        }
    }
    for (heading, _) in &old {
        if !new_map.contains_key(heading.as_str()) {
            out.push(heading.clone());
        }
    }
    out
}

fn frontmatter_changes(
    before: &Frontmatter,
    after: &Frontmatter,
) -> Result<BTreeMap<String, FieldChange>> {
    let mut out = BTreeMap::new();
    let fields: [(&str, serde_json::Value, serde_json::Value); 7] = [
        (
            "title",
            serde_json::to_value(&before.title)?,
            serde_json::to_value(&after.title)?,
        ),
        (
            "description",
            serde_json::to_value(&before.description)?,
            serde_json::to_value(&after.description)?,
        ),
        (
            "tags",
            serde_json::to_value(&before.tags)?,
            serde_json::to_value(&after.tags)?,
        ),
        (
            "status",
            serde_json::to_value(before.effective_status())?,
            serde_json::to_value(after.effective_status())?,
        ),
        (
            "origin",
            serde_json::to_value(&before.origin)?,
            serde_json::to_value(&after.origin)?,
        ),
        (
            "authority",
            serde_json::to_value(&before.authority)?,
            serde_json::to_value(&after.authority)?,
        ),
        (
            "relations",
            serde_json::to_value(&before.relations)?,
            serde_json::to_value(&after.relations)?,
        ),
    ];
    for (field, from, to) in fields {
        if from != to {
            out.insert(field.to_string(), FieldChange { from, to });
        }
    }
    Ok(out)
}

/// unified diff。`similar` のような差分専用crateは品質もAPIも優れているが、
/// この環境では新規依存を取得できず、レビュー負担も増える。既存依存の
/// libgit2 は同じ unified 形式を出せるのでそちらを使う(ADR-0023)。
fn body_diff(before: &str, after: &str) -> Result<(Option<String>, bool)> {
    let mut patch =
        git2::Patch::from_buffers(before.as_bytes(), None, after.as_bytes(), None, None)
            .context("本文diffを作れない")?;
    let buffer = patch.to_buf().context("本文diffを文字列化できない")?;
    let text = String::from_utf8_lossy(&buffer).into_owned();
    if text.is_empty() {
        return Ok((None, false));
    }
    if text.len() > MAX_BODY_DIFF {
        return Ok((None, true));
    }
    Ok((Some(text), false))
}

// ------------------------------------------------------------------ files

/// 移行で作るイベントを置く日別 shard の vault 相対path。既存の月別 shard を
/// 書き換えず(台帳は追記専用)、実施日ごとに1ファイルへまとめて後から見分けられる。
pub fn backfill_shard_relative_path(day: &str) -> String {
    let day = day
        .get(..10)
        .filter(|day| {
            let bytes = day.as_bytes();
            bytes.len() == 10
                && bytes[4] == b'-'
                && bytes[7] == b'-'
                && bytes
                    .iter()
                    .enumerate()
                    .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
        })
        .unwrap_or("unknown");
    format!("{EVENTS_DIR}/backfill-{day}.jsonl")
}

/// イベントを置く月別 shard の vault 相対path。
pub fn shard_relative_path(at: &str) -> String {
    let month = at.get(..7).filter(|month| {
        let bytes = month.as_bytes();
        bytes.len() == 7
            && bytes[4] == b'-'
            && bytes[..4].iter().all(u8::is_ascii_digit)
            && bytes[5..].iter().all(u8::is_ascii_digit)
    });
    format!("{EVENTS_DIR}/{}.jsonl", month.unwrap_or("unknown"))
}

/// shard へ1行追記する。同じ `event_id` が既にあれば何もしない(flush の再実行で重ならない)。
pub fn append_event(root: &Path, event: &NoteEvent) -> Result<()> {
    append_event_to(root, &shard_relative_path(&event.at), event)
}

/// 追記先の shard を明示する形。移行は月別 shard を触らず専用 shard へ書く。
pub fn append_event_to(root: &Path, relative: &str, event: &NoteEvent) -> Result<()> {
    let path = root.join(relative);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .with_context(|| format!("来歴shardの親を作れない: {}", dir.display()))?;
    }
    let existing = fs::read_to_string(&path).unwrap_or_default();
    if existing.contains(&format!("\"event_id\":\"{}\"", event.event_id)) {
        return Ok(());
    }
    let mut line = String::new();
    if !existing.is_empty() && !existing.ends_with('\n') {
        line.push('\n');
    }
    line.push_str(&serde_json::to_string(event).context("来歴イベントのserialize")?);
    line.push('\n');
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("来歴shardを開けない: {}", path.display()))?;
    file.write_all(line.as_bytes())
        .with_context(|| format!("来歴shardへ追記できない: {}", path.display()))?;
    file.sync_all()?;
    Ok(())
}

/// 全 shard を読む。返り値の2番目はparseできなかった行数 — 壊れた行で全体を失わない。
pub fn read_events(root: &Path) -> Result<(Vec<NoteEvent>, usize)> {
    let dir = root.join(EVENTS_DIR);
    let mut shards: Vec<PathBuf> = match fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
            })
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("来歴shardを列挙できない: {}", dir.display()));
        }
    };
    shards.sort();

    let mut events = Vec::new();
    let mut broken = 0usize;
    for shard in shards {
        let text = fs::read_to_string(&shard)
            .with_context(|| format!("来歴shardを読めない: {}", shard.display()))?;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<NoteEvent>(line) {
                Ok(event) => events.push(event),
                Err(_) => broken += 1,
            }
        }
    }
    sort_chronologically(&mut events);
    let mut seen = std::collections::BTreeSet::new();
    events.retain(|event| seen.insert(event.event_id.clone()));
    Ok((events, broken))
}

/// 時刻→event_idの順に並べる。**event_idだけでは時系列にならない** — 蒸留・closureの
/// op_idはsha256 digest由来の小文字hexで、ULID(大文字)より常に後ろへ並ぶ。
/// 時刻が同じ操作(同一transactionの複数ノート)はevent_idで決定的に割る。
fn sort_chronologically(events: &mut [NoteEvent]) {
    events.sort_by(|left, right| {
        left.at
            .cmp(&right.at)
            .then_with(|| left.event_id.cmp(&right.event_id))
    });
}

// ---------------------------------------------------------------- summary

#[derive(Clone, Debug, Serialize)]
pub struct SectionAuthor {
    pub heading: String,
    pub actor: WriteActor,
    pub at: String,
    pub event_id: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ProvenanceSummary {
    pub created_by: Option<WriteActor>,
    pub created_at: Option<String>,
    pub last_by: Option<WriteActor>,
    pub last_at: Option<String>,
    pub event_count: usize,
    pub distinct_actors: usize,
    pub section_authors: Vec<SectionAuthor>,
}

/// 時系列に再生して、作成者・最終書き手・見出しごとの最終書き手を出す。
pub fn summarize(events: &[NoteEvent]) -> ProvenanceSummary {
    let mut ordered: Vec<NoteEvent> = events.to_vec();
    sort_chronologically(&mut ordered);

    let mut summary = ProvenanceSummary {
        event_count: ordered.len(),
        ..ProvenanceSummary::default()
    };
    let mut identities = std::collections::BTreeSet::new();
    let mut sections: BTreeMap<String, SectionAuthor> = BTreeMap::new();
    for event in &ordered {
        identities.insert(event.actor.identity());
        if summary.created_by.is_none() && event.kind == RevisionKind::Create {
            summary.created_by = Some(event.actor.clone());
            summary.created_at = Some(event.at.clone());
        }
        summary.last_by = Some(event.actor.clone());
        summary.last_at = Some(event.at.clone());
        for heading in &event.sections {
            sections.insert(
                heading.clone(),
                SectionAuthor {
                    heading: heading.clone(),
                    actor: event.actor.clone(),
                    at: event.at.clone(),
                    event_id: event.event_id.clone(),
                },
            );
        }
    }
    // create イベントが無い(移行前のノート)場合も、最初の観測を作成扱いにはしない。
    summary.distinct_actors = identities.len();
    summary.section_authors = sections.into_values().collect();
    summary
}

/// 会話・画面へ出す1行要約。
pub fn provenance_line(summary: &ProvenanceSummary) -> String {
    if summary.event_count == 0 {
        return "来歴: 記録なし".to_string();
    }
    let mut parts = Vec::new();
    if let (Some(actor), Some(at)) = (&summary.created_by, &summary.created_at) {
        parts.push(format!("作成 {} {}", date_of(at), actor.label()));
    }
    let updates = summary
        .event_count
        .saturating_sub(usize::from(summary.created_by.is_some()));
    if updates > 0 {
        parts.push(format!("更新{updates}回"));
    }
    if let (Some(actor), Some(at)) = (&summary.last_by, &summary.last_at) {
        parts.push(format!("最終 {} {}", actor.label(), date_of(at)));
    }
    format!("来歴: {}", parts.join(" · "))
}

fn date_of(at: &str) -> &str {
    at.get(..10).unwrap_or(at)
}

// --------------------------------------------------------------------- db

const INSERT_EVENT: &str = "INSERT INTO note_events(
        event_id, note_uid, note_id, at, operation, actor_client, actor_client_version,
        actor_surface, actor_model, actor_model_basis, kind, summary, payload, exported
     ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)";

const RESTORE_EVENT: &str = "INSERT OR IGNORE INTO note_events(
        event_id, note_uid, note_id, at, operation, actor_client, actor_client_version,
        actor_surface, actor_model, actor_model_basis, kind, summary, payload, exported
     ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)";

fn surface_key(surface: ClientSurface) -> String {
    serde_json::to_value(surface)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

fn write_event(conn: &Connection, sql: &str, event: &NoteEvent, exported: i64) -> Result<usize> {
    let payload = serde_json::to_string(event).context("来歴イベントのserialize")?;
    let changed = conn.execute(
        sql,
        rusqlite::params![
            event.event_id,
            event.note_uid,
            event.note_id,
            event.at,
            event.operation.as_str(),
            event.actor.client,
            event.actor.client_version,
            surface_key(event.actor.surface),
            event.actor.model,
            serde_json::to_value(event.actor.model_basis)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_else(|| "unknown".to_string()),
            event.kind.as_str(),
            event.summary,
            payload,
            exported,
        ],
    )?;
    Ok(changed)
}

pub(crate) fn insert_event(conn: &Connection, event: &NoteEvent) -> Result<()> {
    write_event(conn, INSERT_EVENT, event, 0)?;
    Ok(())
}

pub(crate) fn mark_exported(conn: &Connection, event_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE note_events SET exported = 1 WHERE event_id = ?1",
        [event_id],
    )?;
    Ok(())
}

pub fn event_by_id(conn: &Connection, event_id: &str) -> Result<Option<NoteEvent>> {
    let payload: Option<String> = conn
        .query_row(
            "SELECT payload FROM note_events WHERE event_id = ?1",
            [event_id],
            |row| row.get(0),
        )
        .optional()?;
    payload
        .map(|payload| serde_json::from_str(&payload).context("来歴イベントのparse"))
        .transpose()
}

/// 新しい順。`limit` は呼び出し面の表示件数で、台帳は削らない。
///
/// note_idはタイトル由来のslugで、改名すると変わる。安定IDである`note_uid`を
/// 持つノートは両方で引き、改名前に積んだイベントを落とさない(uidを持たない
/// legacyノートはnote_idだけが手掛かりなので、そちらも常に見る)。
pub fn events_for_note(
    conn: &Connection,
    note_id: &str,
    note_uid: Option<&str>,
    limit: usize,
) -> Result<Vec<NoteEvent>> {
    // 時系列の降順。event_idだけでは並ばない理由は`sort_chronologically`のコメント。
    let mut statement = conn.prepare(
        "SELECT payload FROM note_events
         WHERE note_id = ?1 OR (?2 IS NOT NULL AND note_uid = ?2)
         ORDER BY at DESC, event_id DESC LIMIT ?3",
    )?;
    let rows = statement.query_map(
        rusqlite::params![note_id, note_uid, i64::try_from(limit).unwrap_or(i64::MAX)],
        |row| row.get::<_, String>(0),
    )?;
    let mut out = Vec::new();
    for payload in rows {
        out.push(serde_json::from_str(&payload?).context("来歴イベントのparse")?);
    }
    Ok(out)
}

pub fn event_count(conn: &Connection) -> Result<usize> {
    let count: i64 = conn.query_row("SELECT count(*) FROM note_events", [], |row| row.get(0))?;
    usize::try_from(count).context("来歴イベント数が負")
}

// ------------------------------------------------------------------ activity

/// ホームの活動ビュー向け絞り込み。GUI から渡す入力なので specta 型としても公開する。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ActivityFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// `RevisionKind::as_str()` の値(create/amend/…)で絞り込む。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// RFC3339。この時刻以降(`at >= since`)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
}

/// 活動フィード1行。ノートを跨いで新しい順に並べる用途なので、`note_id` だけでなく
/// 一覧に出す `title` も一緒に運ぶ(呼び出し面で毎回 `notes` を引き直させない)。
#[derive(Clone, Debug, Serialize)]
pub struct ActivityRow {
    pub event_id: String,
    pub note_id: String,
    pub title: String,
    pub at: String,
    pub actor: WriteActor,
    pub operation: Operation,
    pub kind: RevisionKind,
    pub summary: Option<String>,
    pub section_count: usize,
}

/// フィルタや表示件数に左右されない、台帳全体の健全性の目安。
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct ActivitySummary {
    pub last_7_days: usize,
    pub distinct_actors: usize,
    pub unknown_model_ratio: f32,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ActivityFeed {
    pub rows: Vec<ActivityRow>,
    pub summary: ActivitySummary,
    /// フィルタの選択肢(絞り込みの結果ではなく、台帳に実在する値の一覧)。
    pub clients: Vec<String>,
    pub models: Vec<String>,
}

/// 新しい順の活動フィード。`summary` / `clients` / `models` は `filter` の影響を
/// 受けない(絞り込んでいる最中でも「全体としてどうか」を見せるため)。
pub fn activity_feed(
    conn: &Connection,
    filter: &ActivityFilter,
    limit: usize,
) -> Result<ActivityFeed> {
    let mut statement = conn.prepare(
        "SELECT ne.payload, coalesce(n.title, ne.note_id)
         FROM note_events ne
         LEFT JOIN notes n ON n.id = ne.note_id
         WHERE (?1 IS NULL OR ne.actor_client = ?1)
           AND (?2 IS NULL OR ne.actor_model = ?2)
           AND (?3 IS NULL OR ne.kind = ?3)
           AND (?4 IS NULL OR ne.at >= ?4)
         ORDER BY ne.at DESC, ne.event_id DESC
         LIMIT ?5",
    )?;
    let query_rows = statement.query_map(
        rusqlite::params![
            filter.client,
            filter.model,
            filter.kind,
            filter.since,
            i64::try_from(limit).unwrap_or(i64::MAX),
        ],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )?;
    let mut rows = Vec::new();
    for row in query_rows {
        let (payload, title) = row?;
        let event: NoteEvent = serde_json::from_str(&payload).context("来歴イベントのparse")?;
        rows.push(ActivityRow {
            event_id: event.event_id,
            note_id: event.note_id,
            title,
            at: event.at,
            section_count: event.sections.len(),
            actor: event.actor,
            operation: event.operation,
            kind: event.kind,
            summary: event.summary,
        });
    }
    Ok(ActivityFeed {
        rows,
        summary: activity_summary(conn)?,
        clients: distinct_values(
            conn,
            "SELECT DISTINCT actor_client FROM note_events ORDER BY actor_client",
        )?,
        models: distinct_values(
            conn,
            "SELECT DISTINCT actor_model FROM note_events \
             WHERE actor_model IS NOT NULL ORDER BY actor_model",
        )?,
    })
}

fn activity_summary(conn: &Connection) -> Result<ActivitySummary> {
    let total: i64 = conn.query_row("SELECT count(*) FROM note_events", [], |row| row.get(0))?;
    if total == 0 {
        return Ok(ActivitySummary::default());
    }
    let cutoff = (time::OffsetDateTime::now_utc() - time::Duration::days(7))
        .format(&time::format_description::well_known::Rfc3339)
        .context("直近7日のcutoffをRFC3339にできない")?;
    let last_7_days: i64 = conn.query_row(
        "SELECT count(*) FROM note_events WHERE at >= ?1",
        [&cutoff],
        |row| row.get(0),
    )?;
    // WriteActor::identity()と同じ「client/model」をキーにする(空文字はmodel無し)。
    let distinct_actors: i64 = conn.query_row(
        "SELECT count(DISTINCT actor_client || '/' || coalesce(actor_model, '')) \
         FROM note_events",
        [],
        |row| row.get(0),
    )?;
    let unknown: i64 = conn.query_row(
        "SELECT count(*) FROM note_events WHERE actor_model_basis = 'unknown'",
        [],
        |row| row.get(0),
    )?;
    Ok(ActivitySummary {
        last_7_days: usize::try_from(last_7_days).unwrap_or(0),
        distinct_actors: usize::try_from(distinct_actors).unwrap_or(0),
        unknown_model_ratio: unknown as f32 / total as f32,
    })
}

/// `sql` はこのモジュール内の固定文字列だけを渡す(呼び出し面からの文字列合成はしない)。
fn distinct_values(conn: &Connection, sql: &str) -> Result<Vec<String>> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// 正本(`.kb-events`)から実行時索引を作り直す。既存行は触らない(台帳が正本)。
pub fn restore_from_files(vault: &Vault, conn: &Connection) -> Result<usize> {
    let (events, _broken) = read_events(&vault.root)?;
    let mut restored = 0usize;
    for event in &events {
        let written = write_event(conn, RESTORE_EVENT, event, 1)?;
        if written != 0 {
            // 台帳へ入れた行は同じ経路で検索索引にも入れる。open時の自己修復は
            // この復元より前に走るので、ここで足さないと索引だけが空のまま残る。
            crate::derived_index::apply_event(conn, event)?;
        }
        restored += written;
    }
    Ok(restored)
}

/// DB を作り直した直後だけ正本から復元する。既に行があれば何もしない。
pub(crate) fn restore_events_if_empty(vault: &Vault, conn: &Connection) -> Result<usize> {
    if event_count(conn)? != 0 {
        return Ok(0);
    }
    restore_from_files(vault, conn)
}

// ----------------------------------------------------------------- backfill

pub const BACKFILL_PLAN_SCHEMA: &str = "kb-app.provenance-backfill-plan/v1";
pub const BACKFILL_RESULT_SCHEMA: &str = "kb-app.provenance-backfill-result/v1";
/// planに載せる下見の件数。全件を会話へ流さず、digestで集合を固定する。
const BACKFILL_PREVIEW: usize = 20;

/// 移行で作る作成相当イベント1件分。Git履歴の最初のcommitだけを根拠にする。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BackfillEntry {
    pub note: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note_uid: Option<String>,
    pub client: String,
    pub at: String,
    pub commit: String,
    pub doc_hash: String,
    pub event_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BackfillPlan {
    pub schema: &'static str,
    pub read_only: bool,
    pub plan_digest: String,
    pub total: usize,
    pub preview: Vec<BackfillEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BackfillReport {
    pub schema: &'static str,
    pub plan_digest: String,
    pub written: usize,
    pub shard: String,
    pub notes: Vec<String>,
}

/// commit message `propose {id} (via {client})` から読み取った起票の記録。
struct ProposeCommit {
    client: String,
    at: String,
    commit: git2::Oid,
}

fn parse_propose_message(message: &str) -> Option<(String, String)> {
    let line = message.lines().next()?.trim();
    let rest = line.strip_prefix("propose ")?;
    let (note, client) = rest.rsplit_once(" (via ")?;
    let client = client.strip_suffix(')')?.trim();
    let note = note.trim();
    if note.is_empty() || client.is_empty() {
        return None;
    }
    Some((note.to_string(), client.to_string()))
}

fn commit_time_iso(commit: &git2::Commit<'_>) -> Result<String> {
    let seconds = commit.time().seconds();
    time::OffsetDateTime::from_unix_timestamp(seconds)
        .context("commit時刻をUTCへ変換できない")?
        .format(&time::format_description::well_known::Rfc3339)
        .context("commit時刻をRFC3339にできない")
}

/// 各ノートの**最初**のpropose commitを拾う。改名後の再起票を作成日にしない。
fn first_propose_commits(vault: &Vault) -> Result<BTreeMap<String, ProposeCommit>> {
    let repo = git2::Repository::open(&vault.root).context("vaultのGit履歴を開けない")?;
    let mut walk = repo.revwalk().context("Git履歴を走査できない")?;
    // 既定の順序は保証されない。時刻降順を明示して、同じ履歴から同じplanを作る。
    walk.set_sorting(git2::Sort::TIME)?;
    if walk.push_head().is_err() {
        // commitが1つも無い保管庫(初期化直後)。移行対象も無い。
        return Ok(BTreeMap::new());
    }
    let mut out: BTreeMap<String, ProposeCommit> = BTreeMap::new();
    for oid in walk {
        let oid = oid?;
        let commit = repo.find_commit(oid)?;
        let Some(message) = commit.message() else {
            continue;
        };
        let Some((note, client)) = parse_propose_message(message) else {
            continue;
        };
        let at = commit_time_iso(&commit)?;
        // revwalkは新しい順。同じノートを何度も見たら、後に見た(=より古い)方を残す。
        out.insert(
            note,
            ProposeCommit {
                client,
                at,
                commit: oid,
            },
        );
    }
    Ok(out)
}

/// その commit 時点のノート全文。移行イベントの doc_hash は当時の内容を指す
/// (現在のdocumentと一致させると、記録されていない後続の改稿を隠すことになる)。
fn document_at_commit(vault: &Vault, commit: git2::Oid, note: &str) -> Result<Option<String>> {
    let repo = git2::Repository::open(&vault.root)?;
    let tree = repo.find_commit(commit)?.tree()?;
    let path = format!("{note}.md");
    let Ok(entry) = tree.get_path(Path::new(&path)) else {
        return Ok(None);
    };
    let blob = repo.find_blob(entry.id())?;
    Ok(String::from_utf8(blob.content().to_vec()).ok())
}

fn backfill_event_id(note: &str, commit: git2::Oid) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("provenance-backfill/v1|{note}|{commit}").as_bytes())
    )
}

/// まだイベントを1件も持たないノートだけを対象にする。既に記録のあるノートへ
/// 後から作成イベントを差し込むと、実際の最初の書き手と食い違う。
fn backfill_targets(vault: &Vault, conn: &Connection) -> Result<Vec<BackfillEntry>> {
    let mut entries = Vec::new();
    for (note, propose) in first_propose_commits(vault)? {
        let stored: Option<Option<String>> = conn
            .query_row("SELECT note_uid FROM notes WHERE id = ?1", [&note], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(stored_uid) = stored else {
            continue; // 既に消えたノートは移行しない(台帳の削除記録も無い)
        };
        let existing = events_for_note(conn, &note, stored_uid.as_deref(), 1)?;
        if !existing.is_empty() {
            continue;
        }
        let Some(document) = document_at_commit(vault, propose.commit, &note)? else {
            continue;
        };
        let note_uid = Note::parse(&document)
            .ok()
            .and_then(|note| note.front.note_uid.as_ref().map(NoteUid::to_string))
            .or(stored_uid);
        entries.push(BackfillEntry {
            event_id: backfill_event_id(&note, propose.commit),
            note,
            note_uid,
            client: propose.client,
            at: propose.at,
            commit: propose.commit.to_string(),
            doc_hash: document_hash(&document),
        });
    }
    entries.sort_by(|left, right| (&left.at, &left.note).cmp(&(&right.at, &right.note)));
    Ok(entries)
}

fn plan_digest(entries: &[BackfillEntry]) -> Result<String> {
    let payload =
        serde_json::to_string(&(BACKFILL_PLAN_SCHEMA, entries)).context("移行planのserialize")?;
    Ok(format!("sha256:{:x}", Sha256::digest(payload.as_bytes())))
}

/// 読み取り専用の移行plan。Git履歴とDBだけを見て、書込は一切行わない。
pub fn plan_backfill(vault: &Vault, conn: &Connection) -> Result<BackfillPlan> {
    let entries = backfill_targets(vault, conn)?;
    Ok(BackfillPlan {
        schema: BACKFILL_PLAN_SCHEMA,
        read_only: true,
        plan_digest: plan_digest(&entries)?,
        total: entries.len(),
        preview: entries.into_iter().take(BACKFILL_PREVIEW).collect(),
    })
}

/// planと同じ集合にだけ書く。二重実行は対象が空集合になりdigestが変わるので、
/// 「同じdigestを受け取れない」形で自然に拒否される。
pub fn apply_backfill(
    vault: &Vault,
    conn: &Connection,
    expected_digest: &str,
) -> Result<BackfillReport> {
    let entries = backfill_targets(vault, conn)?;
    let digest = plan_digest(&entries)?;
    if digest != expected_digest {
        bail!(
            "移行planが古い(対象が変わった)。plan_provenance_backfillを取り直す: 期待 {expected_digest} / 現在 {digest}"
        );
    }
    if entries.is_empty() {
        bail!("移行対象が無い");
    }
    let shard = backfill_shard_relative_path(&crate::frontmatter::today());
    let transaction = conn.unchecked_transaction()?;
    let mut notes = Vec::new();
    for entry in &entries {
        let Some(document) =
            document_at_commit(vault, git2::Oid::from_str(&entry.commit)?, &entry.note)?
        else {
            bail!("移行対象のcommit時点documentを読めない: {}", entry.note);
        };
        let actor = WriteActor::from_client_hint(&entry.client);
        let context = WriteContext {
            actor: &actor,
            revision: None,
            operation: Operation::Import,
        };
        let mut event =
            NoteEvent::for_upsert(&entry.event_id, &entry.note, None, &document, &context)?;
        // 生成時刻ではなく、起票commitの時刻を記録する。
        event.at = entry.at.clone();
        event.note_uid = entry.note_uid.clone();
        // 正本(shard)へ先に書いてからDBへ入れる。exportedで積み直しを起こさない。
        append_event_to(&vault.root, &shard, &event)?;
        write_event(&transaction, INSERT_EVENT, &event, 1)?;
        crate::derived_index::apply_event(&transaction, &event)?;
        notes.push(entry.note.clone());
    }
    transaction.commit()?;
    vault.commit(&[shard.as_str()], "provenance: backfill create events")?;
    Ok(BackfillReport {
        schema: BACKFILL_RESULT_SCHEMA,
        plan_digest: digest,
        written: notes.len(),
        shard,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{Authority, AuthorityRole, AuthorityStatus, NoteNamespace};
    use crate::frontmatter::Frontmatter;

    fn document(body: &str, tags: &[&str]) -> String {
        let mut front = Frontmatter::new_note("来歴テスト");
        front.origin = Some("agent".into());
        front.tags = tags.iter().map(|tag| (*tag).to_string()).collect();
        Note {
            front,
            body: body.to_string(),
        }
        .to_file_string()
        .unwrap()
    }

    fn upsert(before: Option<&str>, after: &str) -> NoteEvent {
        let actor = WriteActor::test();
        let context = WriteContext {
            actor: &actor,
            revision: None,
            operation: Operation::Update,
        };
        NoteEvent::for_upsert("op-1", "notes/来歴テスト", before, after, &context).unwrap()
    }

    #[test]
    fn sections_track_added_changed_and_removed_headings() {
        let before = document("## 背景\n旧。\n\n## 決定\n変えない。\n", &["test"]);
        let after = document(
            "## 背景\n新。\n\n## 決定\n変えない。\n\n## 影響\n増えた。\n",
            &["test"],
        );
        let event = upsert(Some(&before), &after);
        assert_eq!(event.sections, vec!["## 背景", "## 影響"]);

        let removed = upsert(Some(&after), &before);
        assert_eq!(removed.sections, vec!["## 背景", "## 影響"]);
    }

    #[test]
    fn a_body_without_headings_is_compared_as_a_single_preamble() {
        let before = document("見出しのない本文。", &["test"]);
        let after = document("見出しのない本文を直した。", &["test"]);
        assert_eq!(upsert(Some(&before), &after).sections, vec![PREAMBLE]);
        // 見出しの前に置かれた文も同じ扱いにする
        let mixed = document("前書き。\n\n## 節\n中身。\n", &["test"]);
        assert_eq!(
            upsert(Some(&after), &mixed).sections,
            vec![PREAMBLE, "## 節"]
        );
    }

    #[test]
    fn changes_capture_tags_and_authority_but_not_untouched_fields() {
        let before = document("本文。", &["test"]);
        let mut note = Note::parse(&before).unwrap();
        note.front.tags = vec!["test".into(), "kb-app".into()];
        note.front.note_uid = Some(NoteUid::at(1));
        note.front.authority = Some(Authority {
            namespace: NoteNamespace::Records,
            role: AuthorityRole::Record,
            status: AuthorityStatus::Active,
            scope: "test/provenance".into(),
        });
        let after = note.to_file_string().unwrap();

        let event = upsert(Some(&before), &after);
        assert_eq!(
            event.changes.keys().collect::<Vec<_>>(),
            vec!["authority", "tags"]
        );
        assert_eq!(
            event.changes["tags"].to,
            serde_json::json!(["test", "kb-app"])
        );
        assert!(event.changes["authority"].from.is_null());
        assert!(event.sections.is_empty(), "本文は変えていない");
        assert!(event.body_diff.is_none());
    }

    #[test]
    fn body_diff_is_unified_and_dropped_past_the_size_cap() {
        let before = document("一行目。\n", &["test"]);
        let after = document("一行目。\n二行目。\n", &["test"]);
        let event = upsert(Some(&before), &after);
        let diff = event.body_diff.expect("差分がある");
        assert!(diff.contains("@@"), "{diff}");
        assert!(diff.contains("+二行目。"), "{diff}");
        assert!(!event.diff_truncated);

        let huge = document(&"追加された長い行。\n".repeat(1_000), &["test"]);
        let large = upsert(Some(&before), &huge);
        assert!(large.body_diff.is_none());
        assert!(large.diff_truncated);
        // 上限を超えても、前後のhashは残す
        assert!(large.base_hash.is_some() && large.doc_hash.is_some());
    }

    #[test]
    fn creation_has_no_base_hash_and_lists_every_section() {
        let after = document("## 背景\n最初。\n\n## 決定\n決めた。\n", &["test"]);
        let actor = WriteActor::test();
        let context = WriteContext {
            actor: &actor,
            revision: None,
            operation: Operation::Propose,
        };
        let event =
            NoteEvent::for_upsert("op-create", "notes/新規", None, &after, &context).unwrap();
        assert_eq!(event.kind, RevisionKind::Create);
        assert_eq!(event.operation, Operation::Propose);
        assert_eq!(event.sections, vec!["## 背景", "## 決定"]);
        assert!(event.base_hash.is_none());
        assert_eq!(event.doc_hash, Some(document_hash(&after)));
    }

    #[test]
    fn append_event_is_idempotent_and_keeps_one_line_per_event() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let event = upsert(None, &document("本文。", &["test"]));
        append_event(root, &event).unwrap();
        append_event(root, &event).unwrap();

        let shard = root.join(shard_relative_path(&event.at));
        let text = fs::read_to_string(&shard).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(text.ends_with('\n'), "末尾改行を保つ: {text:?}");

        let mut second = event.clone();
        second.event_id = "op-2".into();
        append_event(root, &second).unwrap();
        assert_eq!(fs::read_to_string(&shard).unwrap().lines().count(), 2);
    }

    #[test]
    fn read_events_skips_broken_lines_and_orders_them_chronologically() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let base = upsert(None, &document("本文。", &["test"]));
        for id in ["op-3", "op-1"] {
            let mut event = base.clone();
            event.event_id = id.into();
            append_event(root, &event).unwrap();
        }
        let shard = root.join(shard_relative_path(&base.at));
        let mut text = fs::read_to_string(&shard).unwrap();
        text.push_str("\n{壊れた行}\n\n");
        fs::write(&shard, text).unwrap();

        let (events, broken) = read_events(root).unwrap();
        assert_eq!(broken, 1);
        assert_eq!(
            events
                .iter()
                .map(|event| event.event_id.as_str())
                .collect::<Vec<_>>(),
            vec!["op-1", "op-3"]
        );
    }

    #[test]
    fn summarize_attributes_each_heading_to_its_last_writer() {
        let codex = WriteActor::from_client_hint("codex-cli/gpt-5.6-sol");
        let claude = WriteActor::from_client_hint("claude-code/claude-fable-5-1");
        let event =
            |id: &str, actor: &WriteActor, kind: RevisionKind, sections: &[&str]| NoteEvent {
                v: EVENT_VERSION,
                event_id: id.to_string(),
                note_uid: None,
                note_id: "notes/x".into(),
                at: "2026-09-09T00:00:00Z".into(),
                operation: Operation::Update,
                actor: actor.clone(),
                origin_claim: None,
                kind,
                summary: None,
                reason: None,
                evidence: Vec::new(),
                sections: sections.iter().map(|s| (*s).to_string()).collect(),
                changes: BTreeMap::new(),
                body_diff: None,
                diff_truncated: false,
                base_hash: None,
                doc_hash: None,
            };
        let events = vec![
            event("01", &codex, RevisionKind::Create, &["## 背景", "## 決定"]),
            event("02", &claude, RevisionKind::Amend, &["## 決定"]),
            event("03", &codex, RevisionKind::Amend, &["## 影響"]),
        ];

        let summary = summarize(&events);
        assert_eq!(summary.event_count, 3);
        assert_eq!(summary.distinct_actors, 2);
        assert_eq!(summary.created_by.as_ref().unwrap().client, "codex-cli");
        assert_eq!(summary.last_by.as_ref().unwrap().client, "codex-cli");
        let authors: Vec<(&str, &str)> = summary
            .section_authors
            .iter()
            .map(|author| (author.heading.as_str(), author.actor.client.as_str()))
            .collect();
        assert_eq!(
            authors,
            // summarize自体は見出し名のcode point順。本文順への並べ替えは
            // 現在の本文を持つ`Vault::note_provenance`が行う。
            vec![
                ("## 影響", "codex-cli"),
                ("## 決定", "claude-code"),
                ("## 背景", "codex-cli"),
            ]
        );
    }

    /// 2026-09-10: 蒸留・closureのop_idはsha256 digest由来の小文字hexで、ULIDの
    /// 大文字英数より常に後ろへ並ぶ。event_idだけで並べると、蒸留の後に人手の
    /// クライアントが更新しても「最終書き手は蒸留」に見えてしまう。
    #[test]
    fn ordering_is_chronological_even_when_ids_are_not() {
        let distillation = WriteActor::app_api("kb-app-distillation", None);
        let claude = WriteActor::from_client_hint("claude-code/claude-fable-5-1");
        let event = |id: &str, at: &str, actor: &WriteActor, kind: RevisionKind| NoteEvent {
            v: EVENT_VERSION,
            event_id: id.to_string(),
            note_uid: None,
            note_id: "notes/x".into(),
            at: at.to_string(),
            operation: Operation::Update,
            actor: actor.clone(),
            origin_claim: None,
            kind,
            summary: None,
            reason: None,
            evidence: Vec::new(),
            sections: vec!["## 決定".into()],
            changes: BTreeMap::new(),
            body_diff: None,
            diff_truncated: false,
            base_hash: None,
            doc_hash: None,
        };
        let events = vec![
            // 後から起きた更新のIDが、先に起きた蒸留のIDより小さい
            event(
                "01K5ZZZZZZZZZZZZZZZZZZZZZZ",
                "2026-09-10T00:00:00Z",
                &claude,
                RevisionKind::Amend,
            ),
            event(
                "9f2c:apply:0",
                "2026-09-09T00:00:00Z",
                &distillation,
                RevisionKind::Create,
            ),
        ];

        let summary = summarize(&events);
        assert_eq!(
            summary.created_by.as_ref().unwrap().client,
            "kb-app-distillation"
        );
        assert_eq!(summary.last_by.as_ref().unwrap().client, "claude-code");
        assert_eq!(summary.last_at.as_deref(), Some("2026-09-10T00:00:00Z"));
        assert_eq!(summary.section_authors[0].actor.client, "claude-code");
    }

    /// モデル名だけでは「どの設定で書いたか」が分からない(2026-09-10 本人指摘)。
    /// 動作設定はモデルの後ろに空白区切りで続け、表示名と `generated.by` の両方へ出す。
    #[test]
    fn actor_label_and_generated_by_carry_the_mode_after_the_model() {
        // 設定値: hintの3番目のsegmentが動作設定
        let configured = WriteActor::from_client_hint("codex-cli/gpt-5.6-sol/medium");
        assert_eq!(configured.mode.as_deref(), Some("medium"));
        assert_eq!(configured.model_basis, ModelBasis::Config);
        assert_eq!(configured.label(), "codex-cli/gpt-5.6-sol medium(設定値)");
        assert_eq!(
            configured.generated_by("codex-cli/gpt-5.6-sol/medium"),
            "codex-cli/gpt-5.6-sol medium"
        );

        // 自己申告はモデルと動作設定を一緒に置き換える
        let reported = configured
            .clone()
            .with_self_report("gpt-6-codex", Some(" Astra medium "));
        assert_eq!(reported.mode.as_deref(), Some("Astra medium"));
        assert_eq!(
            reported.label(),
            "codex-cli/gpt-6-codex Astra medium(自己申告)"
        );
        assert_eq!(
            reported.generated_by("codex-cli/gpt-5.6-sol/medium"),
            "codex-cli/gpt-6-codex Astra medium"
        );
        assert_eq!(
            reported.identity(),
            "codex-cli/gpt-6-codex",
            "動作設定は書き手の同一性キーに含めない"
        );

        // モデルだけ申告し直したら、設定値の動作設定は引き継がない
        let model_only = configured.with_self_report("gpt-6-codex", None);
        assert_eq!(model_only.mode, None);
        assert_eq!(model_only.label(), "codex-cli/gpt-6-codex(自己申告)");

        // handshake名が製品名を上書きしても generated.by の先頭は接続設定の製品名
        let handshaken = WriteActor::from_client_hint("claude-desktop/claude")
            .with_handshake("local-agent-mode-kb-app-write", Some("1.0.0"))
            .with_self_report("claude-fable-5-1", Some("Code tab"));
        assert_eq!(
            handshaken.generated_by("claude-desktop/claude"),
            "claude-desktop/claude-fable-5-1 Code tab"
        );

        // モデル不明なら動作設定も持たず、generated.by は製品名だけ
        let bare = WriteActor::from_client_hint("test//medium");
        assert_eq!(bare.model, None);
        assert_eq!(bare.mode, None);
        assert_eq!(bare.label(), "test/モデル不明");
        assert_eq!(bare.generated_by("test//medium"), "test");

        // JSONL往復で動作設定を失わず、無いときは鍵も出さない
        let json = serde_json::to_string(&reported).unwrap();
        assert!(json.contains("\"mode\":\"Astra medium\""), "{json}");
        let back: WriteActor = serde_json::from_str(&json).unwrap();
        assert_eq!(back, reported);
        assert!(
            !serde_json::to_string(&model_only)
                .unwrap()
                .contains("\"mode\":")
        );
    }

    #[test]
    fn provenance_line_names_the_writer_and_how_certain_the_model_is() {
        let codex = WriteActor::from_client_hint("codex-cli/gpt-5.6-sol");
        let summary = ProvenanceSummary {
            created_by: Some(codex.clone()),
            created_at: Some("2026-09-09T01:02:03Z".into()),
            last_by: Some(WriteActor::app_api("kb-app-distillation", None)),
            last_at: Some("2026-09-10T04:05:06Z".into()),
            event_count: 4,
            distinct_actors: 2,
            section_authors: Vec::new(),
        };
        assert_eq!(
            provenance_line(&summary),
            "来歴: 作成 2026-09-09 codex-cli/gpt-5.6-sol(設定値) · 更新3回 · \
             最終 kb-app-distillation/モデル不明 2026-09-10"
        );
        assert_eq!(
            provenance_line(&ProvenanceSummary::default()),
            "来歴: 記録なし"
        );
    }

    /// hint文字列の2番目のsegmentは面によって意味が違う。蒸留の
    /// `kb-app-distillation/requested/…` を「モデル requested」と読ませない。
    #[test]
    fn app_api_actors_do_not_reparse_the_client_hint_for_a_model() {
        let hint = "kb-app-distillation/requested/Some(Codex)/gpt-5.6-sol/effort=ultra";
        let parsed = WriteActor::from_client_hint(hint);
        assert_eq!(parsed.model.as_deref(), Some("requested"));

        let actor = WriteActor::app_api(client_product(hint), None);
        assert_eq!(actor.client, "kb-app-distillation");
        assert_eq!(actor.model, None);
        assert_eq!(actor.model_basis, ModelBasis::Unknown);
    }

    #[test]
    fn revision_input_rejects_empty_multiline_and_non_uid_evidence() {
        let long = "あ".repeat(MAX_TEXT + 1);
        for value in ["", "  ", "一行目\n二行目", long.as_str()] {
            let revision = RevisionInput {
                summary: Some(value.to_string()),
                ..RevisionInput::default()
            };
            assert!(revision.validate().is_err(), "{value:?}");
        }
        let ok = RevisionInput {
            summary: Some("統合したため".into()),
            evidence: vec![NoteUid::at(1).to_string()],
            ..RevisionInput::default()
        };
        ok.validate().unwrap();
        let bad = RevisionInput {
            evidence: vec!["notes/どこか".into()],
            ..RevisionInput::default()
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn shards_are_monthly_and_reject_a_broken_timestamp() {
        assert_eq!(
            shard_relative_path("2026-09-10T00:00:00Z"),
            ".kb-events/2026-09.jsonl"
        );
        assert_eq!(shard_relative_path("../../etc"), ".kb-events/unknown.jsonl");
    }

    /// フィルタは行を絞るが、summary/clients/modelsは台帳全体を見続ける
    /// (絞り込み中でも「全体としてどうか」のカードが動かないようにするため)。
    #[test]
    fn activity_feed_filters_rows_but_summarizes_everything() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();

        let claude_id = vault
            .propose_for_test(
                "活動フィードA",
                "本文A。",
                None,
                &["test".into()],
                "claude-code/claude-fable-5-1",
            )
            .unwrap();
        // モデル segment を持たないhintはmodel_basis=Unknownになる(WriteActor::from_client_hint)。
        let codex_id = vault
            .propose_for_test(
                "活動フィードB",
                "本文B。",
                None,
                &["test".into()],
                "codex-cli",
            )
            .unwrap();

        // claude側の1件だけ、直近7日より前の書込に見せかける。
        let mut old = vault.note_history(&conn, &claude_id, 1).unwrap().remove(0);
        old.at = "2020-01-01T00:00:00Z".into();
        let payload = serde_json::to_string(&old).unwrap();
        conn.execute(
            "UPDATE note_events SET at = ?1, payload = ?2 WHERE event_id = ?3",
            rusqlite::params![old.at, payload, old.event_id],
        )
        .unwrap();

        let unfiltered = activity_feed(&conn, &ActivityFilter::default(), 50).unwrap();
        assert_eq!(unfiltered.rows.len(), 2);
        assert_eq!(unfiltered.summary.distinct_actors, 2);
        assert_eq!(
            unfiltered.summary.last_7_days, 1,
            "書き換えた過去の1件は数えない"
        );
        assert!((unfiltered.summary.unknown_model_ratio - 0.5).abs() < f32::EPSILON);
        assert_eq!(unfiltered.clients, vec!["claude-code", "codex-cli"]);
        assert_eq!(unfiltered.models, vec!["claude-fable-5-1"]);

        let by_client = activity_feed(
            &conn,
            &ActivityFilter {
                client: Some("codex-cli".into()),
                ..ActivityFilter::default()
            },
            50,
        )
        .unwrap();
        assert_eq!(by_client.rows.len(), 1);
        assert_eq!(by_client.rows[0].note_id, codex_id);
        // 絞り込み中でも summary は全体のまま。
        assert_eq!(by_client.summary.distinct_actors, 2);

        let recent = activity_feed(
            &conn,
            &ActivityFilter {
                since: Some("2026-01-01T00:00:00Z".into()),
                ..ActivityFilter::default()
            },
            50,
        )
        .unwrap();
        assert_eq!(recent.rows.len(), 1);
        assert_eq!(recent.rows[0].note_id, codex_id);

        let by_kind = activity_feed(
            &conn,
            &ActivityFilter {
                kind: Some(RevisionKind::Create.as_str().to_string()),
                ..ActivityFilter::default()
            },
            50,
        )
        .unwrap();
        assert_eq!(by_kind.rows.len(), 2, "propose_for_testは両方とも新規作成");
    }

    #[test]
    fn activity_feed_is_empty_and_zeroed_when_the_ledger_has_no_events() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = crate::index::open_db(&vault).unwrap();

        let feed = activity_feed(&conn, &ActivityFilter::default(), 30).unwrap();
        assert!(feed.rows.is_empty());
        assert_eq!(feed.summary.last_7_days, 0);
        assert_eq!(feed.summary.distinct_actors, 0);
        assert_eq!(feed.summary.unknown_model_ratio, 0.0);
        assert!(feed.clients.is_empty());
        assert!(feed.models.is_empty());
    }
}
