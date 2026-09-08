//! 提案の版・AIレビュー・本人の採否をノートdocumentと同じtransactionに固定する。
//! authorityのproposalとは別のworkflow。採用は実装・配備の自動実行権限にしない。

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::authority::{Authority, AuthorityRole, AuthorityStatus, NoteNamespace, NoteUid};
use crate::frontmatter::{Frontmatter, Generated, Note, now_iso};
use crate::vault::Vault;

const KEY: &str = "proposal_ticket";
const SCHEMA: u32 = 1;
// 一覧・MCP・Markdown復元で履歴を丸ごと扱える大きさに限定する。
const MAX_REVISIONS: usize = 100;
const MAX_RECORDS: usize = 200;
const MAX_WORKFLOW_BYTES: usize = 1_048_576;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ProposalInput {
    pub title: String,
    pub problem: String,
    pub proposal: String,
    pub impact: String,
    pub acceptance: String,
    pub tags: Vec<String>,
    pub scope: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum ReviewRecommendation {
    Approve,
    Reject,
    Revise,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ReviewInput {
    pub summary: String,
    pub benefits: String,
    pub risks: String,
    pub alternatives: String,
    pub recommendation: ReviewRecommendation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum DecisionOutcome {
    Approve,
    Reject,
    Hold,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct DecisionInput {
    pub outcome: DecisionOutcome,
    pub reason: String,
    pub next_action: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum TicketStatus {
    ReviewPending,
    DecisionPending,
    Approved,
    Rejected,
    Held,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ProposalRevision {
    pub revision: u32,
    pub input: ProposalInput,
    pub author: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ProposalReview {
    pub revision: u32,
    pub input: ReviewInput,
    pub reviewer: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ProposalDecision {
    pub revision: u32,
    /// 本人が判断時に確認したレビュー集合を、履歴全体のprefix長で固定する。
    pub review_count: u32,
    pub input: DecisionInput,
    pub decider: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct TicketView {
    pub note_id: String,
    pub note_uid: String,
    pub title: String,
    pub status: TicketStatus,
    pub etag: String,
    pub current_revision: u32,
    pub revisions: Vec<ProposalRevision>,
    pub reviews: Vec<ProposalReview>,
    pub decisions: Vec<ProposalDecision>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct TicketMutation {
    pub ticket: TicketView,
    pub export_pending: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Workflow {
    schema: u32,
    revisions: Vec<ProposalRevision>,
    reviews: Vec<ProposalReview>,
    decisions: Vec<ProposalDecision>,
}

#[derive(Debug)]
struct Failure {
    code: &'static str,
    detail: String,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for Failure {}

fn failure(code: &'static str, detail: impl Into<String>) -> anyhow::Error {
    Failure {
        code,
        detail: detail.into(),
    }
    .into()
}

pub fn error_code(error: &anyhow::Error) -> Option<&'static str> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<Failure>().map(|value| value.code))
}

fn require(condition: bool, code: &'static str, detail: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(failure(code, detail))
    }
}

fn text(value: &str, limit: usize, optional: bool) -> Result<()> {
    require(
        (optional || !value.trim().is_empty())
            && value.chars().count() <= limit
            && !value.contains('\0'),
        "proposal_invalid_input",
        "提案の必須項目または入力長が不正",
    )
}

fn actor(value: &str) -> Result<()> {
    text(value, 256, false)?;
    require(
        !value.starts_with("human:") && !value.chars().any(char::is_control),
        "proposal_invalid_input",
        "AIの起票元・レビュー元が不正",
    )
}

impl ProposalInput {
    fn validate(&self) -> Result<()> {
        text(&self.title, 200, false)?;
        text(&self.problem, 8_000, false)?;
        text(&self.proposal, 12_000, false)?;
        text(&self.impact, 8_000, false)?;
        text(&self.acceptance, 8_000, false)?;
        text(&self.scope, 200, false)?;
        authority(self)
            .validate()
            .map_err(|_| failure("proposal_invalid_input", "提案scopeが不正"))?;
        crate::tags::validate_structure(&self.tags)
            .map_err(|_| failure("proposal_invalid_input", "提案タグが不正"))
    }
}

impl ReviewInput {
    fn validate(&self) -> Result<()> {
        for value in [
            &self.summary,
            &self.benefits,
            &self.risks,
            &self.alternatives,
        ] {
            text(value, 4_000, false)?;
        }
        Ok(())
    }
}

impl DecisionInput {
    fn validate(&self) -> Result<()> {
        text(&self.reason, 4_000, true)?;
        text(
            &self.next_action,
            4_000,
            self.outcome != DecisionOutcome::Hold,
        )
    }
}

fn authority(input: &ProposalInput) -> Authority {
    Authority {
        namespace: NoteNamespace::Decisions,
        role: AuthorityRole::Proposal,
        status: AuthorityStatus::Active,
        scope: input.scope.clone(),
    }
}

impl Workflow {
    fn current(&self) -> &ProposalRevision {
        // 呼出元はdecode/transition直後にvalidateする。空の履歴を有効なworkflowとして扱わない。
        &self.revisions[self.revisions.len() - 1]
    }

    fn status(&self) -> TicketStatus {
        let revision = self.current().revision;
        if let Some(decision) = self
            .decisions
            .iter()
            .rev()
            .find(|value| value.revision == revision)
        {
            return match decision.input.outcome {
                DecisionOutcome::Approve => TicketStatus::Approved,
                DecisionOutcome::Reject => TicketStatus::Rejected,
                DecisionOutcome::Hold => TicketStatus::Held,
            };
        }
        if self.reviews.iter().any(|value| value.revision == revision) {
            TicketStatus::DecisionPending
        } else {
            TicketStatus::ReviewPending
        }
    }

    fn validate(&self) -> Result<()> {
        require(
            self.schema == SCHEMA
                && !self.revisions.is_empty()
                && self.revisions.len() <= MAX_REVISIONS
                && self.reviews.len() <= MAX_RECORDS
                && self.decisions.len() <= MAX_RECORDS,
            "proposal_corrupt",
            "提案のschemaまたは履歴件数が不正",
        )?;
        for (index, revision) in self.revisions.iter().enumerate() {
            require(
                revision.revision as usize == index + 1,
                "proposal_corrupt",
                "提案の版が連続していない",
            )?;
            revision.input.validate()?;
            actor(&revision.author)?;
            timestamp(&revision.created_at)?;
        }
        let mut last_review_revision = 0;
        for review in &self.reviews {
            require(
                review.revision > 0
                    && review.revision <= self.current().revision
                    && review.revision >= last_review_revision,
                "proposal_corrupt",
                "レビューの対象版が不正",
            )?;
            review.input.validate()?;
            actor(&review.reviewer)?;
            timestamp(&review.created_at)?;
            last_review_revision = review.revision;
        }
        let mut finalized = std::collections::HashSet::new();
        let mut last_decision_revision = 0;
        let mut last_review_count = 0;
        for decision in &self.decisions {
            require(
                decision.revision > 0
                    && decision.revision <= self.current().revision
                    && decision.revision >= last_decision_revision
                    && decision.review_count >= last_review_count
                    && decision.review_count as usize <= self.reviews.len()
                    && !finalized.contains(&decision.revision)
                    && decision.decider == crate::OWNER_ACTOR,
                "proposal_corrupt",
                "採否の対象版・決定者・履歴が不正",
            )?;
            decision.input.validate()?;
            timestamp(&decision.created_at)?;
            let (reviewed, later) = self.reviews.split_at(decision.review_count as usize);
            require(
                reviewed
                    .iter()
                    .all(|review| review.revision <= decision.revision)
                    && later
                        .iter()
                        .all(|review| review.revision >= decision.revision),
                "proposal_corrupt",
                "採否時点の版とレビューの順序が一致しない",
            )?;
            if decision.input.outcome != DecisionOutcome::Hold {
                require(
                    reviewed
                        .iter()
                        .any(|review| review.revision == decision.revision)
                        && !later
                            .iter()
                            .any(|review| review.revision == decision.revision),
                    "proposal_corrupt",
                    "採否に対応するレビュー集合が不正",
                )?;
                finalized.insert(decision.revision);
            }
            last_decision_revision = decision.revision;
            last_review_count = decision.review_count;
        }
        require(
            serde_json::to_vec(self)?.len() <= MAX_WORKFLOW_BYTES,
            "proposal_invalid_input",
            "提案の履歴が保存上限に達した",
        )
    }
}

fn timestamp(value: &str) -> Result<()> {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .map(|_| ())
        .map_err(|_| failure("proposal_corrupt", "提案履歴の時刻が不正"))
}

fn render(workflow: &Workflow) -> String {
    let input = &workflow.current().input;
    let label = match workflow.status() {
        TicketStatus::ReviewPending => "レビュー待ち",
        TicketStatus::DecisionPending => "採否待ち",
        TicketStatus::Approved => "採用",
        TicketStatus::Rejected => "棄却",
        TicketStatus::Held => "保留",
    };
    let mut body = format!(
        "# {}\n\n提案チケット / 版{} / {}\n\n## 課題\n\n{}\n\n## 提案\n\n{}\n\n## 影響\n\n{}\n\n## 完了条件\n\n{}\n\n採用はこの版に対する判断です。実装・外部操作の実行や完了を意味しません。\n",
        input.title,
        workflow.current().revision,
        label,
        input.problem,
        input.proposal,
        input.impact,
        input.acceptance
    );
    for review in &workflow.reviews {
        body.push_str(&format!(
            "\n## レビュー（版{}・{}）\n\n{}\n\n利点: {}\n\n懸念: {}\n\n代替案: {}\n\n推奨: {:?}\n",
            review.revision,
            review.reviewer,
            review.input.summary,
            review.input.benefits,
            review.input.risks,
            review.input.alternatives,
            review.input.recommendation
        ));
    }
    for decision in &workflow.decisions {
        body.push_str(&format!(
            "\n## 本人の採否（版{}・{}）\n\n{:?}: {}\n\n次の確認: {}\n",
            decision.revision,
            decision.created_at,
            decision.input.outcome,
            decision.input.reason,
            decision.input.next_action
        ));
    }
    body
}

fn decode(note: &Note) -> Result<Option<Workflow>> {
    let Some(value) = note.front.extra.get(KEY) else {
        return Ok(None);
    };
    // YAML側のaliasや余剰フィールドも受け入れず、現行版のtyped dataとして検査する。
    let workflow: Workflow = serde_yaml::from_value(value.clone())
        .map_err(|_| failure("proposal_corrupt", "提案の保存形式が不正"))?;
    workflow
        .validate()
        .map_err(|_| failure("proposal_corrupt", "提案の保存履歴が不正"))?;
    let current = &workflow.current().input;
    require(
        note.front.note_uid.is_some()
            && note.front.origin.as_deref() == Some("agent")
            && note.front.authority.as_ref() == Some(&authority(current))
            && note.front.title.as_deref() == Some(current.title.as_str())
            && note.front.tags == current.tags
            && note.body.trim() == render(&workflow).trim(),
        "proposal_corrupt",
        "提案の本文・authorityと保存された版が一致しない",
    )?;
    Ok(Some(workflow))
}

/// 通常参照へ出せる状態は専用workflowから導く。旧authorityのproposal分類とは混同しない。
pub(crate) fn derive_normal_reference_allowed(note: &Note) -> bool {
    match decode(note) {
        Ok(None) => true,
        Ok(Some(workflow)) => workflow.status() == TicketStatus::Approved,
        Err(_) => false,
    }
}

/// 通常getとSQL検索が同じ判定を使う。未取得・欠落を許可へ倒さない。
pub fn normal_reference_allowed(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT normal_reference_allowed = 1 FROM notes WHERE id = ?1",
            [id],
            |row| row.get::<_, bool>(0),
        )
        .optional()?
        .unwrap_or(false))
}

pub fn require_normal_reference(conn: &Connection, id: &str) -> Result<()> {
    require(
        normal_reference_allowed(conn, id)?,
        "proposal_not_referenceable",
        "通常参照の対象ではありません。提案のレビュー・改訂にはget_proposalを使ってください。",
    )
}

fn store(note: &mut Note, workflow: &Workflow, by: &str) -> Result<()> {
    workflow.validate()?;
    let current = &workflow.current().input;
    note.front.title = Some(current.title.clone());
    note.front.tags = current.tags.clone();
    note.front.authority = Some(authority(current));
    note.front.generated = Some(Generated {
        by: by.into(),
        at: now_iso(),
    });
    note.front
        .extra
        .insert(KEY.into(), serde_yaml::to_value(workflow)?);
    note.body = render(workflow);
    Ok(())
}

fn view(id: &str, note: &Note, workflow: Workflow) -> Result<TicketView> {
    let uid = note
        .front
        .note_uid
        .as_ref()
        .context("提案のnote UIDがない")?
        .to_string();
    let etag = format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&(&uid, &workflow))?)
    );
    Ok(TicketView {
        note_id: id.into(),
        note_uid: uid,
        title: workflow.current().input.title.clone(),
        status: workflow.status(),
        etag,
        current_revision: workflow.current().revision,
        revisions: workflow.revisions,
        reviews: workflow.reviews,
        decisions: workflow.decisions,
    })
}

fn read_note(conn: &Connection, id: &str) -> Result<Note> {
    if !crate::note_store::contains(conn, id)? {
        return Err(failure("proposal_not_found", "提案が見つからない"));
    }
    crate::note_store::read(conn, id)
}

pub fn get_optional(conn: &Connection, note: &str) -> Result<Option<TicketView>> {
    let stored = read_note(conn, note)?;
    decode(&stored)?
        .map(|workflow| view(note, &stored, workflow))
        .transpose()
}

pub fn get(conn: &Connection, note: &str) -> Result<TicketView> {
    get_optional(conn, note)?
        .ok_or_else(|| failure("proposal_not_found", "このノートは提案チケットではない"))
}

pub fn list(conn: &Connection) -> Result<Vec<TicketView>> {
    // documentが正本。検索文や本文の「承認済み」からticket判定しない。
    let mut statement = conn.prepare("SELECT id, document FROM notes ORDER BY created DESC, id")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut result = Vec::new();
    for row in rows {
        let (id, document) = row?;
        if document.is_empty() {
            continue;
        }
        let note = Note::parse(&document)?;
        if let Some(workflow) = decode(&note)? {
            result.push(view(&id, &note, workflow)?);
        }
    }
    Ok(result)
}

/// 通常note更新口からは構築できない、workflow transitionだけの保存能力。
pub(crate) struct ProposalWritePermit {
    _private: (),
}

fn commit_mutation(
    vault: &Vault,
    conn: &Connection,
    id: &str,
    note: Note,
    workflow: Workflow,
) -> Result<TicketMutation> {
    let ticket = view(id, &note, workflow)?;
    crate::note_store::queue_proposal_put(
        vault,
        conn,
        id,
        &note,
        &ProposalWritePermit { _private: () },
    )?;
    Ok(TicketMutation {
        ticket,
        export_pending: false,
    })
}

fn finish_export(vault: &Vault, conn: &Connection, mut result: TicketMutation) -> TicketMutation {
    // DB確定後のexport失敗をerrorへ戻すと、既に保存した提案・採否が再送される。
    result.export_pending = vault.flush_note_exports(conn).is_err();
    result
}

pub fn create(
    vault: &Vault,
    conn: &Connection,
    input: ProposalInput,
    client: &str,
) -> Result<TicketMutation> {
    input.validate()?;
    actor(client)?;
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    crate::tags::validate(&tx, &input.tags, false)?;
    let id = vault.next_note_id(&tx, &input.title)?;
    let mut front = Frontmatter::new_note(&input.title);
    front.note_uid = Some(NoteUid::new());
    front.origin = Some("agent".into());
    front.created = Some(now_iso());
    let workflow = Workflow {
        schema: SCHEMA,
        revisions: vec![ProposalRevision {
            revision: 1,
            input,
            author: client.into(),
            created_at: now_iso(),
        }],
        reviews: vec![],
        decisions: vec![],
    };
    let mut note = Note {
        front,
        body: String::new(),
    };
    store(&mut note, &workflow, client)?;
    let result = commit_mutation(vault, &tx, &id, note, workflow)?;
    tx.commit()?;
    Ok(finish_export(vault, conn, result))
}

fn change(
    vault: &Vault,
    conn: &Connection,
    id: &str,
    expected_etag: &str,
    by: &str,
    transition: impl FnOnce(&Connection, &mut Workflow) -> Result<()>,
) -> Result<TicketMutation> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let mut note = read_note(&tx, id)?;
    let mut workflow =
        decode(&note)?.ok_or_else(|| failure("proposal_not_found", "提案チケットではない"))?;
    require(
        view(id, &note, workflow.clone())?.etag == expected_etag,
        "proposal_stale",
        "確認後に提案・レビュー・採否が変わったため、再取得が必要",
    )?;
    transition(&tx, &mut workflow)?;
    store(&mut note, &workflow, by)?;
    let result = commit_mutation(vault, &tx, id, note, workflow)?;
    tx.commit()?;
    Ok(finish_export(vault, conn, result))
}

pub fn revise(
    vault: &Vault,
    conn: &Connection,
    note: &str,
    expected_etag: &str,
    input: ProposalInput,
    client: &str,
) -> Result<TicketMutation> {
    input.validate()?;
    actor(client)?;
    change(vault, conn, note, expected_etag, client, |tx, workflow| {
        crate::tags::validate(tx, &input.tags, false)?;
        workflow.revisions.push(ProposalRevision {
            revision: workflow.current().revision + 1,
            input,
            author: client.into(),
            created_at: now_iso(),
        });
        Ok(())
    })
}

pub fn review(
    vault: &Vault,
    conn: &Connection,
    note: &str,
    expected_etag: &str,
    input: ReviewInput,
    client: &str,
) -> Result<TicketMutation> {
    input.validate()?;
    actor(client)?;
    change(vault, conn, note, expected_etag, client, |_, workflow| {
        require(
            !matches!(
                workflow.status(),
                TicketStatus::Approved | TicketStatus::Rejected
            ),
            "proposal_invalid_state",
            "採否確定済みの版は改訂してからレビューする",
        )?;
        workflow.reviews.push(ProposalReview {
            revision: workflow.current().revision,
            input,
            reviewer: client.into(),
            created_at: now_iso(),
        });
        Ok(())
    })
}

/// 本人のGUI操作だけに接続する。MCP/CLIに公開する入力能力には含めない。
pub fn decide(
    vault: &Vault,
    conn: &Connection,
    note: &str,
    expected_etag: &str,
    input: DecisionInput,
) -> Result<TicketMutation> {
    input.validate()?;
    change(
        vault,
        conn,
        note,
        expected_etag,
        crate::OWNER_ACTOR,
        |_, workflow| {
            require(
                !matches!(
                    workflow.status(),
                    TicketStatus::Approved | TicketStatus::Rejected
                ),
                "proposal_invalid_state",
                "この版の採否は既に確定している",
            )?;
            if input.outcome != DecisionOutcome::Hold {
                require(
                    workflow
                        .reviews
                        .iter()
                        .any(|review| review.revision == workflow.current().revision),
                    "proposal_invalid_state",
                    "現在の版のレビューを確認してから採否を決める",
                )?;
            }
            workflow.decisions.push(ProposalDecision {
                revision: workflow.current().revision,
                review_count: workflow.reviews.len() as u32,
                input,
                decider: crate::OWNER_ACTOR.into(),
                created_at: now_iso(),
            });
            Ok(())
        },
    )
}

fn same_protected_note(before: &Note, after: &Note) -> bool {
    before.front.extra.get(KEY) == after.front.extra.get(KEY)
        && before.body.trim() == after.body.trim()
        && before.front.title == after.front.title
        && before.front.tags == after.front.tags
        && before.front.authority == after.front.authority
        && before.front.note_uid == after.front.note_uid
        && before.front.origin == after.front.origin
        && before.front.created == after.front.created
        && before.front.kind == after.front.kind
}

pub(crate) fn guard_note_write(before: Option<&Note>, after: &Note) -> Result<()> {
    if before.is_some_and(|note| note.front.extra.contains_key(KEY)) {
        let previous = before.context("既存提案がない")?;
        decode(previous)?;
        require(
            same_protected_note(previous, after),
            "proposal_protected",
            "提案本文はrevise_proposal、レビューはreview_proposal、採否は本人の画面操作で更新する",
        )
    } else {
        require(
            !after.front.extra.contains_key(KEY),
            "proposal_protected",
            "提案チケットは専用操作で起票する",
        )
    }
}

/// 信頼したバックアップは復元できるが、既存の版・レビュー・採否の分岐や短縮は取り込まない。
pub(crate) fn guard_import(before: Option<&Note>, after: &Note) -> Result<()> {
    let next = decode(after)?;
    let Some(previous) = before else {
        return Ok(());
    };
    let Some(old) = decode(previous)? else {
        return Ok(());
    };
    let Some(next) = next else {
        return Err(failure(
            "proposal_protected",
            "提案の保存履歴を削除できない",
        ));
    };
    require(
        previous.front.note_uid == after.front.note_uid
            && previous.front.origin == after.front.origin
            && previous.front.created == after.front.created
            && next.revisions.starts_with(&old.revisions)
            && next.reviews.starts_with(&old.reviews)
            && next.decisions.starts_with(&old.decisions),
        "proposal_protected",
        "提案の既存履歴を上書き・分岐・短縮できない",
    )?;
    let current = old.current().revision;
    let terminal = matches!(
        old.status(),
        TicketStatus::Approved | TicketStatus::Rejected
    );
    require(
        next.reviews[old.reviews.len()..]
            .iter()
            .all(|review| review.revision >= current && (!terminal || review.revision > current))
            && next.decisions[old.decisions.len()..]
                .iter()
                .all(|decision| decision.revision >= current),
        "proposal_protected",
        "改訂済みの旧版や採否確定済みの版へ履歴を後付けできない",
    )
}

pub(crate) fn guard_note_delete(note: &Note) -> Result<()> {
    require(
        !note.front.extra.contains_key(KEY),
        "proposal_protected",
        "採否と提案履歴を削除できない。不要な提案は棄却する",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{index, note_store};

    fn setup() -> (tempfile::TempDir, Vault, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        vault
            .propose_for_test(
                "既存タグ",
                "語彙fixture",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        let conn = index::open_db(&vault).unwrap();
        index::sync(&vault, &conn).unwrap();
        (dir, vault, conn)
    }

    fn input() -> ProposalInput {
        ProposalInput {
            title: "提案の採否を残す".into(),
            problem: "検討と採用が混ざる".into(),
            proposal: "版を指定して採否を残す".into(),
            impact: "既存ノートの編集とは独立".into(),
            acceptance: "改訂後に再レビューが必要".into(),
            tags: vec!["test".into()],
            scope: "test/proposal-workflow".into(),
        }
    }

    fn review_input() -> ReviewInput {
        ReviewInput {
            summary: "採否の対象が明確になる".into(),
            benefits: "判断を追跡できる".into(),
            risks: "古い版への判断を避ける必要がある".into(),
            alternatives: "現行の手作業を継続".into(),
            recommendation: ReviewRecommendation::Approve,
        }
    }

    fn decision(outcome: DecisionOutcome) -> DecisionInput {
        DecisionInput {
            outcome,
            reason: "この版の検討を完了した".into(),
            next_action: if outcome == DecisionOutcome::Hold {
                "担当: 本人。次の確認: 対象件数が揃ったとき".into()
            } else {
                String::new()
            },
        }
    }

    fn assert_error<T: std::fmt::Debug>(result: Result<T>, code: &str) {
        assert_eq!(error_code(&result.unwrap_err()), Some(code));
    }

    fn document(conn: &Connection, id: &str) -> String {
        conn.query_row("SELECT document FROM notes WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .unwrap()
    }

    #[test]
    fn proposal_decision_binds_review_and_revision_and_preserves_history() {
        let (_dir, vault, conn) = setup();
        let created = create(&vault, &conn, input(), "codex").unwrap().ticket;
        let id = &created.note_id;
        assert_eq!(created.status, TicketStatus::ReviewPending);
        assert!(!normal_reference_allowed(&conn, id).unwrap());
        let original_document = document(&conn, id);
        assert_error(
            decide(
                &vault,
                &conn,
                id,
                &created.etag,
                decision(DecisionOutcome::Approve),
            ),
            "proposal_invalid_state",
        );
        assert_eq!(document(&conn, id), original_document);
        let reviewed = review(
            &vault,
            &conn,
            id,
            &created.etag,
            review_input(),
            "claude_code",
        )
        .unwrap()
        .ticket;
        assert_eq!(reviewed.status, TicketStatus::DecisionPending);
        assert!(!normal_reference_allowed(&conn, id).unwrap());
        assert_eq!(reviewed.reviews[0].reviewer, "claude_code");
        assert_error(
            decide(
                &vault,
                &conn,
                id,
                &created.etag,
                decision(DecisionOutcome::Approve),
            ),
            "proposal_stale",
        );
        let approved = decide(
            &vault,
            &conn,
            id,
            &reviewed.etag,
            decision(DecisionOutcome::Approve),
        )
        .unwrap()
        .ticket;
        assert_eq!(approved.status, TicketStatus::Approved);
        assert!(normal_reference_allowed(&conn, id).unwrap());
        require_normal_reference(&conn, id).unwrap();
        assert_eq!(approved.decisions[0].decider, crate::OWNER_ACTOR);
        assert_eq!(approved.decisions[0].review_count, 1);
        assert_error(
            review(&vault, &conn, id, &approved.etag, review_input(), "codex"),
            "proposal_invalid_state",
        );
        assert_error(
            decide(
                &vault,
                &conn,
                id,
                &approved.etag,
                decision(DecisionOutcome::Reject),
            ),
            "proposal_invalid_state",
        );
        let mut revised_input = input();
        revised_input.proposal.push_str("。対象を拡張する");
        let revised = revise(&vault, &conn, id, &approved.etag, revised_input, "codex")
            .unwrap()
            .ticket;
        assert_eq!(revised.current_revision, 2);
        assert_eq!(revised.status, TicketStatus::ReviewPending);
        assert!(!normal_reference_allowed(&conn, id).unwrap());
        assert_error(
            require_normal_reference(&conn, id),
            "proposal_not_referenceable",
        );
        assert_eq!(&revised.revisions[..1], approved.revisions.as_slice());
        assert_eq!(revised.reviews, approved.reviews);
        assert_eq!(revised.decisions, approved.decisions);
        assert_error(
            decide(
                &vault,
                &conn,
                id,
                &revised.etag,
                decision(DecisionOutcome::Approve),
            ),
            "proposal_invalid_state",
        );
    }

    #[test]
    fn proposal_hold_requires_followup_and_review_does_not_silently_release_hold() {
        let (_dir, vault, conn) = setup();
        let created = create(&vault, &conn, input(), "codex").unwrap().ticket;
        let id = &created.note_id;
        let mut incomplete = decision(DecisionOutcome::Hold);
        incomplete.next_action.clear();
        assert_error(
            decide(&vault, &conn, id, &created.etag, incomplete),
            "proposal_invalid_input",
        );
        let held = decide(
            &vault,
            &conn,
            id,
            &created.etag,
            decision(DecisionOutcome::Hold),
        )
        .unwrap()
        .ticket;
        assert_eq!(held.status, TicketStatus::Held);
        assert!(!normal_reference_allowed(&conn, id).unwrap());
        let reviewed = review(&vault, &conn, id, &held.etag, review_input(), "claude_code")
            .unwrap()
            .ticket;
        assert_eq!(reviewed.status, TicketStatus::Held);
        assert!(!normal_reference_allowed(&conn, id).unwrap());
        let rejected = decide(
            &vault,
            &conn,
            id,
            &reviewed.etag,
            decision(DecisionOutcome::Reject),
        )
        .unwrap()
        .ticket;
        assert_eq!(rejected.status, TicketStatus::Rejected);
        assert!(!normal_reference_allowed(&conn, id).unwrap());
        assert_eq!(rejected.decisions.len(), 2);
    }

    #[test]
    fn proposal_inputs_reject_spoofed_fields_and_invalid_shapes_before_writes() {
        let (_dir, vault, conn) = setup();
        let mut invalid = input();
        invalid.problem.clear();
        assert_error(
            create(&vault, &conn, invalid, "codex"),
            "proposal_invalid_input",
        );
        let mut oversized = input();
        oversized.proposal = "あ".repeat(12_001);
        assert_error(
            create(&vault, &conn, oversized, "codex"),
            "proposal_invalid_input",
        );
        assert_error(
            create(&vault, &conn, input(), crate::OWNER_ACTOR),
            "proposal_invalid_input",
        );
        let mut unknown_tag = input();
        unknown_tag.tags = vec!["unregistered-proposal-tag".into()];
        assert!(create(&vault, &conn, unknown_tag, "codex").is_err());
        let mut spoofed = serde_json::to_value(review_input()).unwrap();
        spoofed["reviewer"] = serde_json::json!("human:owner");
        assert!(serde_json::from_value::<ReviewInput>(spoofed).is_err());
        let mut spoofed = serde_json::to_value(input()).unwrap();
        spoofed["status"] = serde_json::json!("approved");
        assert!(serde_json::from_value::<ProposalInput>(spoofed).is_err());
        let mut spoofed = serde_json::to_value(decision(DecisionOutcome::Approve)).unwrap();
        spoofed["decider"] = serde_json::json!("codex");
        assert!(serde_json::from_value::<DecisionInput>(spoofed).is_err());
        assert!(list(&conn).unwrap().is_empty());
        assert_eq!(note_store::pending_count(&conn).unwrap(), 0);
    }

    #[test]
    fn proposal_generic_writes_and_deletion_cannot_change_enrolled_content() {
        let (_dir, vault, conn) = setup();
        let created = create(&vault, &conn, input(), "codex").unwrap().ticket;
        let id = &created.note_id;
        let before = note_store::read(&conn, id).unwrap();
        let original = document(&conn, id);
        let mut variants = Vec::new();
        let mut changed = before.clone();
        changed.body.push_str("\n採用済み");
        variants.push(changed);
        let mut changed = before.clone();
        changed.front.extra.remove(KEY);
        variants.push(changed);
        let mut changed = before.clone();
        changed.front.title = Some("隠れた改訂".into());
        variants.push(changed);
        let mut changed = before.clone();
        changed.front.authority.as_mut().unwrap().scope = "other/scope".into();
        variants.push(changed);
        for changed in variants {
            assert_error(
                note_store::put(&vault, &conn, id, &changed, "test", "test"),
                "proposal_protected",
            );
            assert_eq!(document(&conn, id), original);
        }
        assert_error(
            note_store::put(
                &vault,
                &conn,
                "notes/forged-ticket",
                &before,
                "test",
                "test",
            ),
            "proposal_protected",
        );
        assert_error(
            vault.agent_removal_candidate(&conn, id),
            "proposal_protected",
        );
        assert_error(
            note_store::delete(&vault, &conn, id, "test", "test"),
            "proposal_protected",
        );
        let mut metadata = before;
        metadata.front.description = Some("関連する提案の概要".into());
        note_store::put(&vault, &conn, id, &metadata, "test", "test").unwrap();
        assert_eq!(get(&conn, id).unwrap().etag, created.etag);
    }

    #[test]
    fn proposal_failed_outbox_insert_rolls_back_document_and_history() {
        let (_dir, vault, conn) = setup();
        let created = create(&vault, &conn, input(), "codex").unwrap().ticket;
        let reviewed = review(
            &vault,
            &conn,
            &created.note_id,
            &created.etag,
            review_input(),
            "claude_code",
        )
        .unwrap()
        .ticket;
        let approved = decide(
            &vault,
            &conn,
            &created.note_id,
            &reviewed.etag,
            decision(DecisionOutcome::Approve),
        )
        .unwrap()
        .ticket;
        let before = document(&conn, &created.note_id);
        conn.execute_batch(
            "CREATE TEMP TRIGGER reject_proposal_export BEFORE INSERT ON note_exports
             BEGIN SELECT RAISE(ABORT, 'test outbox failure'); END;",
        )
        .unwrap();
        assert!(
            revise(
                &vault,
                &conn,
                &created.note_id,
                &approved.etag,
                input(),
                "claude_code"
            )
            .is_err()
        );
        assert_eq!(document(&conn, &created.note_id), before);
        assert_eq!(get(&conn, &created.note_id).unwrap().etag, approved.etag);
        assert!(normal_reference_allowed(&conn, &created.note_id).unwrap());
        assert_eq!(note_store::pending_count(&conn).unwrap(), 0);
    }

    #[test]
    fn proposal_export_failure_reports_committed_success_and_old_etag_cannot_retry() {
        let (_dir, vault, conn) = setup();
        let created = create(&vault, &conn, input(), "codex").unwrap().ticket;
        let log = vault.root.join("log.md");
        std::fs::remove_file(&log).unwrap();
        std::fs::create_dir(&log).unwrap();
        let saved = review(
            &vault,
            &conn,
            &created.note_id,
            &created.etag,
            review_input(),
            "claude_code",
        )
        .unwrap();
        assert!(saved.export_pending);
        assert_eq!(
            get(&conn, &created.note_id).unwrap().etag,
            saved.ticket.etag
        );
        assert_eq!(note_store::pending_count(&conn).unwrap(), 1);
        assert_error(
            review(
                &vault,
                &conn,
                &created.note_id,
                &created.etag,
                review_input(),
                "claude_code",
            ),
            "proposal_stale",
        );
        std::fs::remove_dir(&log).unwrap();
        assert_eq!(vault.flush_note_exports(&conn).unwrap(), 1);
        assert_eq!(get(&conn, &created.note_id).unwrap().reviews.len(), 1);
    }

    #[test]
    fn proposal_restore_preserves_history_and_live_import_only_extends_it() {
        let (dir, vault, conn) = setup();
        let created = create(&vault, &conn, input(), "codex").unwrap().ticket;
        let initial_note = note_store::read(&conn, &created.note_id).unwrap();
        let restored = Vault::create(dir.path().join("restored")).unwrap();
        restored
            .write_note_fixture(&created.note_id, &initial_note)
            .unwrap();
        let restored_conn = index::open_db(&restored).unwrap();
        assert!(
            index::import_markdown_snapshot(&restored, &restored_conn)
                .unwrap()
                .degraded
                .is_empty()
        );
        assert_eq!(
            get(&restored_conn, &created.note_id).unwrap().etag,
            created.etag
        );
        assert!(!normal_reference_allowed(&restored_conn, &created.note_id).unwrap());
        let reviewed = review(
            &vault,
            &conn,
            &created.note_id,
            &created.etag,
            review_input(),
            "claude_code",
        )
        .unwrap()
        .ticket;
        let approved = decide(
            &vault,
            &conn,
            &created.note_id,
            &reviewed.etag,
            decision(DecisionOutcome::Approve),
        )
        .unwrap()
        .ticket;
        let approved_note = note_store::read(&conn, &created.note_id).unwrap();
        restored
            .write_note_fixture(&created.note_id, &approved_note)
            .unwrap();
        assert!(
            index::import_markdown_snapshot(&restored, &restored_conn)
                .unwrap()
                .degraded
                .is_empty()
        );
        assert_eq!(
            get(&restored_conn, &created.note_id).unwrap().etag,
            approved.etag
        );
        assert!(normal_reference_allowed(&restored_conn, &created.note_id).unwrap());
        restored
            .write_note_fixture(&created.note_id, &initial_note)
            .unwrap();
        assert_error(
            index::import_markdown_snapshot(&restored, &restored_conn),
            "proposal_protected",
        );
        assert!(normal_reference_allowed(&restored_conn, &created.note_id).unwrap());
        assert_eq!(
            get(&restored_conn, &created.note_id).unwrap().etag,
            approved.etag
        );
        let mut diverged = approved_note.clone();
        let mut workflow = decode(&diverged).unwrap().unwrap();
        workflow.reviews[0].input.summary = "過去レビューを書き換えた".into();
        store(&mut diverged, &workflow, "codex").unwrap();
        assert_error(
            guard_import(Some(&approved_note), &diverged),
            "proposal_protected",
        );
        std::fs::remove_file(restored.root.join(format!("{}.md", created.note_id))).unwrap();
        assert_error(
            index::import_markdown_snapshot(&restored, &restored_conn),
            "proposal_protected",
        );
        assert_eq!(
            get(&restored_conn, &created.note_id).unwrap().etag,
            approved.etag
        );
    }

    #[test]
    fn proposal_get_distinguishes_normal_missing_and_corrupt_notes() {
        let (_dir, vault, conn) = setup();
        let ordinary: String = conn
            .query_row("SELECT id FROM notes LIMIT 1", [], |row| row.get(0))
            .unwrap();
        assert!(get_optional(&conn, &ordinary).unwrap().is_none());
        assert_error(get_optional(&conn, "notes/missing"), "proposal_not_found");
        let created = create(&vault, &conn, input(), "codex").unwrap().ticket;
        let mut corrupted = note_store::read(&conn, &created.note_id).unwrap();
        corrupted.body.push_str("\n採用済み");
        assert_error(guard_import(None, &corrupted), "proposal_corrupt");
        conn.execute(
            "UPDATE notes SET document=?1 WHERE id=?2",
            rusqlite::params![corrupted.to_file_string().unwrap(), &created.note_id],
        )
        .unwrap();
        assert_error(get(&conn, &created.note_id), "proposal_corrupt");
        assert_error(list(&conn), "proposal_corrupt");
    }

    #[test]
    fn proposal_first_ticket_bootstraps_tags_without_an_unrelated_note() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = index::open_db(&vault).unwrap();
        let created = create(&vault, &conn, input(), "codex").unwrap();
        assert!(!created.export_pending);
        assert_eq!(list(&conn).unwrap().len(), 1);
    }

    #[test]
    fn proposal_decision_review_prefix_rejects_post_decision_review_on_restore() {
        let (_dir, vault, conn) = setup();
        let created = create(&vault, &conn, input(), "codex").unwrap().ticket;
        let reviewed = review(
            &vault,
            &conn,
            &created.note_id,
            &created.etag,
            review_input(),
            "claude_code",
        )
        .unwrap()
        .ticket;
        let approved = decide(
            &vault,
            &conn,
            &created.note_id,
            &reviewed.etag,
            decision(DecisionOutcome::Approve),
        )
        .unwrap()
        .ticket;
        let saved = note_store::read(&conn, &created.note_id).unwrap();
        let mut workflow = decode(&saved).unwrap().unwrap();
        workflow.reviews.push(ProposalReview {
            revision: 1,
            input: review_input(),
            reviewer: "codex".into(),
            created_at: now_iso(),
        });
        assert_eq!(approved.decisions[0].review_count, 1);
        assert_error(workflow.validate(), "proposal_corrupt");
        // 外部バックアップはtyped dataと対応本文を両方書き換えていても拒否する。
        let mut forged = saved.clone();
        forged
            .front
            .extra
            .insert(KEY.into(), serde_yaml::to_value(&workflow).unwrap());
        forged.body = render(&workflow);
        assert_error(guard_import(None, &forged), "proposal_corrupt");
        assert_error(guard_import(Some(&saved), &forged), "proposal_corrupt");
        let mut missing_review = decode(&saved).unwrap().unwrap();
        missing_review.decisions[0].review_count = 0;
        assert_error(missing_review.validate(), "proposal_corrupt");
    }

    #[test]
    fn proposal_import_cannot_add_history_to_an_already_revised_version() {
        let (_dir, vault, conn) = setup();
        let created = create(&vault, &conn, input(), "codex").unwrap().ticket;
        let revised = revise(
            &vault,
            &conn,
            &created.note_id,
            &created.etag,
            input(),
            "codex",
        )
        .unwrap()
        .ticket;
        let before = note_store::read(&conn, &created.note_id).unwrap();
        let mut workflow = decode(&before).unwrap().unwrap();
        workflow.reviews.push(ProposalReview {
            revision: 1,
            input: review_input(),
            reviewer: "claude_code".into(),
            created_at: now_iso(),
        });
        let mut after = before.clone();
        store(&mut after, &workflow, "claude_code").unwrap();
        assert_error(guard_import(Some(&before), &after), "proposal_protected");
        workflow.reviews.clear();
        workflow.decisions.push(ProposalDecision {
            revision: 1,
            review_count: 0,
            input: decision(DecisionOutcome::Hold),
            decider: crate::OWNER_ACTOR.into(),
            created_at: now_iso(),
        });
        store(&mut after, &workflow, crate::OWNER_ACTOR).unwrap();
        assert_error(guard_import(Some(&before), &after), "proposal_protected");
        assert_eq!(get(&conn, &created.note_id).unwrap().etag, revised.etag);
    }

    #[test]
    fn proposal_hold_review_prefix_includes_reviews_of_previous_versions() {
        let (_dir, vault, conn) = setup();
        let created = create(&vault, &conn, input(), "codex").unwrap().ticket;
        let reviewed = review(
            &vault,
            &conn,
            &created.note_id,
            &created.etag,
            review_input(),
            "claude_code",
        )
        .unwrap()
        .ticket;
        let revised = revise(
            &vault,
            &conn,
            &created.note_id,
            &reviewed.etag,
            input(),
            "codex",
        )
        .unwrap()
        .ticket;
        let held = decide(
            &vault,
            &conn,
            &created.note_id,
            &revised.etag,
            decision(DecisionOutcome::Hold),
        )
        .unwrap()
        .ticket;
        assert_eq!(held.decisions[0].revision, 2);
        assert_eq!(held.decisions[0].review_count, 1);
        let mut restored = note_store::read(&conn, &created.note_id).unwrap();
        guard_import(None, &restored).unwrap();
        let mut workflow = decode(&restored).unwrap().unwrap();
        workflow.decisions[0].review_count = 0;
        restored
            .front
            .extra
            .insert(KEY.into(), serde_yaml::to_value(&workflow).unwrap());
        restored.body = render(&workflow);
        assert_error(guard_import(None, &restored), "proposal_corrupt");
    }

    #[test]
    fn proposal_optional_reasons_preserve_decision_guards_and_restore_verbatim() {
        let (dir, vault, conn) = setup();
        let restored = Vault::create(dir.path().join("restored")).unwrap();
        let restored_conn = index::open_db(&restored).unwrap();
        let mut saved_tickets = Vec::new();
        for outcome in [
            DecisionOutcome::Approve,
            DecisionOutcome::Reject,
            DecisionOutcome::Hold,
        ] {
            // 既存の非空理由も同じ保存形式・etagで復元し、理由を正規化しない。
            for reason in ["", " \n\t", " 従来の判断理由を保持する "] {
                let created = create(&vault, &conn, input(), "codex").unwrap().ticket;
                let mut decision_input = decision(outcome);
                decision_input.reason = reason.into();
                let mut etag = created.etag.clone();
                if outcome != DecisionOutcome::Hold {
                    assert_error(
                        decide(
                            &vault,
                            &conn,
                            &created.note_id,
                            &etag,
                            decision_input.clone(),
                        ),
                        "proposal_invalid_state",
                    );
                    let reviewed = review(
                        &vault,
                        &conn,
                        &created.note_id,
                        &etag,
                        review_input(),
                        "claude_code",
                    )
                    .unwrap()
                    .ticket;
                    assert_error(
                        decide(
                            &vault,
                            &conn,
                            &created.note_id,
                            &etag,
                            decision_input.clone(),
                        ),
                        "proposal_stale",
                    );
                    etag = reviewed.etag;
                } else {
                    let mut missing_followup = decision_input.clone();
                    missing_followup.next_action.clear();
                    assert_error(
                        decide(&vault, &conn, &created.note_id, &etag, missing_followup),
                        "proposal_invalid_input",
                    );
                }
                let saved = decide(&vault, &conn, &created.note_id, &etag, decision_input)
                    .unwrap()
                    .ticket;
                assert_eq!(saved.decisions[0].input.reason, reason);
                assert_eq!(saved.decisions[0].input.outcome, outcome);
                assert_eq!(get(&conn, &created.note_id).unwrap().etag, saved.etag);
                let stored_note = note_store::read(&conn, &created.note_id).unwrap();
                restored
                    .write_note_fixture(&created.note_id, &stored_note)
                    .unwrap();
                saved_tickets.push(saved);
            }
        }
        assert!(
            index::import_markdown_snapshot(&restored, &restored_conn)
                .unwrap()
                .degraded
                .is_empty()
        );
        for saved in saved_tickets {
            let imported = get(&restored_conn, &saved.note_id).unwrap();
            assert_eq!(imported.etag, saved.etag);
            assert_eq!(imported.decisions, saved.decisions);
            assert_eq!(imported.reviews, saved.reviews);
            assert_eq!(imported.revisions, saved.revisions);
        }
    }

    #[test]
    fn proposal_optional_reason_keeps_length_and_nul_limits_before_writes() {
        let (_dir, vault, conn) = setup();
        let created = create(&vault, &conn, input(), "codex").unwrap().ticket;
        let before = document(&conn, &created.note_id);
        for outcome in [
            DecisionOutcome::Approve,
            DecisionOutcome::Reject,
            DecisionOutcome::Hold,
        ] {
            for reason in ["あ".repeat(4_001), "理由\0本文".into()] {
                let mut invalid = decision(outcome);
                invalid.reason = reason;
                assert_error(
                    decide(&vault, &conn, &created.note_id, &created.etag, invalid),
                    "proposal_invalid_input",
                );
                assert_eq!(document(&conn, &created.note_id), before);
                assert_eq!(note_store::pending_count(&conn).unwrap(), 0);
            }
        }
    }

    /// 2026-09-06: 旧DBの本文は不変に保ち、通常参照の可否だけを現版から補完する。
    #[test]
    fn proposal_normal_reference_migration_backfills_without_rewriting_documents() {
        let (_dir, vault, conn) = setup();
        let ordinary: String = conn
            .query_row("SELECT id FROM notes LIMIT 1", [], |row| row.get(0))
            .unwrap();
        let mut expected = vec![(ordinary.clone(), document(&conn, &ordinary), true)];
        let legacy_id = vault
            .propose_for_test(
                "旧proposal分類",
                "チケットではない案",
                None,
                &["test".into()],
                "codex",
            )
            .unwrap();
        let mut legacy = note_store::read(&conn, &legacy_id).unwrap();
        legacy.front.authority = Some(authority(&input()));
        note_store::put(&vault, &conn, &legacy_id, &legacy, "test", "test").unwrap();
        vault.flush_note_exports(&conn).unwrap();
        assert!(normal_reference_allowed(&conn, &legacy_id).unwrap());
        expected.push((legacy_id.clone(), document(&conn, &legacy_id), true));
        for status in [
            TicketStatus::ReviewPending,
            TicketStatus::DecisionPending,
            TicketStatus::Approved,
            TicketStatus::Rejected,
            TicketStatus::Held,
        ] {
            let mut ticket = create(&vault, &conn, input(), "codex").unwrap().ticket;
            if matches!(
                status,
                TicketStatus::DecisionPending | TicketStatus::Approved | TicketStatus::Rejected
            ) {
                ticket = review(
                    &vault,
                    &conn,
                    &ticket.note_id,
                    &ticket.etag,
                    review_input(),
                    "claude_code",
                )
                .unwrap()
                .ticket;
            }
            let outcome = match status {
                TicketStatus::Approved => Some(DecisionOutcome::Approve),
                TicketStatus::Rejected => Some(DecisionOutcome::Reject),
                TicketStatus::Held => Some(DecisionOutcome::Hold),
                _ => None,
            };
            if let Some(outcome) = outcome {
                ticket = decide(
                    &vault,
                    &conn,
                    &ticket.note_id,
                    &ticket.etag,
                    decision(outcome),
                )
                .unwrap()
                .ticket;
            }
            assert_eq!(ticket.status, status);
            let stored = document(&conn, &ticket.note_id);
            if status == TicketStatus::Approved {
                let mut corrupted = Note::parse(&stored).unwrap();
                corrupted.body.push_str("\n対応しない本文");
                let corrupt_document = corrupted.to_file_string().unwrap();
                conn.execute(
                    "INSERT INTO notes(id, document) VALUES('notes/corrupt-ticket', ?1)",
                    [&corrupt_document],
                )
                .unwrap();
                expected.push(("notes/corrupt-ticket".into(), corrupt_document, false));
            }
            expected.push((ticket.note_id, stored, status == TicketStatus::Approved));
        }
        for (id, invalid) in [
            ("notes/unparsed", "invalid document"),
            ("notes/unrestored", ""),
        ] {
            conn.execute(
                "INSERT INTO notes(id, document) VALUES(?1, ?2)",
                rusqlite::params![id, invalid],
            )
            .unwrap();
            expected.push((id.into(), invalid.into(), false));
        }
        index::test_support::drop_distillation_schema_for_legacy_fixture(&conn);
        conn.execute_batch(
            "ALTER TABLE notes DROP COLUMN normal_reference_allowed;
             UPDATE meta SET value='8' WHERE key='schema';",
        )
        .unwrap();
        drop(conn);
        // 壊れたfixtureの自己修復とは分離し、schema migrationだけの結果を検証する。
        let migrated = index::open_db_recovery(&vault).unwrap();
        for (id, before, allowed) in expected {
            assert_eq!(document(&migrated, &id), before, "{id}: 本文を変えない");
            assert_eq!(
                normal_reference_allowed(&migrated, &id).unwrap(),
                allowed,
                "{id}"
            );
        }
        assert!(!normal_reference_allowed(&migrated, "notes/missing").unwrap());
        let error = require_normal_reference(&migrated, "notes/missing").unwrap_err();
        assert_eq!(error_code(&error), Some("proposal_not_referenceable"));
        assert_eq!(
            error.to_string(),
            "proposal_not_referenceable: 通常参照の対象ではありません。提案のレビュー・改訂にはget_proposalを使ってください。"
        );
        assert!(
            migrated
                .execute(
                    "UPDATE notes SET normal_reference_allowed=2 WHERE id=?1",
                    [&ordinary]
                )
                .is_err()
        );
        migrated
            .execute("INSERT INTO notes(id) VALUES('notes/underived')", [])
            .unwrap();
        assert!(!normal_reference_allowed(&migrated, "notes/underived").unwrap());
    }
}
