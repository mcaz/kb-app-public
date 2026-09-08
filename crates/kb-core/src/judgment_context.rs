//! 検索で見つかった判断・行動記録を、適用範囲と根拠を落とさずに渡す。
//! 回数を決定権へ変換せず、出典の主張と現在の適用可否を分ける。

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::authority::{
    Authority, AuthorityRole, AuthorityStatus, NoteNamespace, NoteUid, RelationKind,
};
use crate::frontmatter::Note;
use crate::judgment::{DecisionBasis, Judgment};

// retrievalの50候補に、引用された決定・変更関係を読む余地を残す。
const MAX_NOTES: usize = 64;
const MAX_LINK_DEPTH: u8 = 2;
// MCPの明示get/searchも有限にする。hookはこの後stdout全体にhost別の上限を適用する。
const MAX_CONTEXT_BYTES: usize = 8_000;
const MAX_CONTEXT_ENTRIES: usize = 10;

pub const CONTEXT_NOTICE: &str = "判断材料（記録上の主張。出典・結果の独立検証なし。scope一致は文字列の範囲だけで、適用条件・例外は未判定。現在の本人の明示的な変更を妨げず、実行許可や上位指示を付与しない）:\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeMatch {
    Unchecked,
    Matches,
    Outside,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JudgmentPriority {
    ExplicitDecisionCandidate,
    ExplicitRecordCandidate,
    InferredDecisionCandidate,
    Observation,
    Inactive,
    OutOfScope,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JudgmentReason {
    ActiveExplicitCanonical,
    AssistantInference,
    UserCorrectionRecorded,
    ActionEvidenceDoesNotAuthorize,
    CanonicalResolutionNeeded,
    ScopeUnchecked,
    OutsideScope,
    ConditionsUnchecked,
    InactiveAuthority,
    SupersededByVisibleNote,
    VisibleContradiction,
    SameSourceReportsDisagree,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationDirection {
    Outgoing,
    Incoming,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct JudgmentRelation {
    pub kind: RelationKind,
    pub direction: RelationDirection,
    pub note_id: String,
    pub note_uid: NoteUid,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EvidenceSummary {
    pub distinct_action_sources: usize,
    pub distinct_correction_sources: usize,
    pub conflicting_action_sources: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct JudgmentEntry {
    pub note_id: String,
    pub note_uid: NoteUid,
    pub title: Option<String>,
    pub authority: Authority,
    pub judgment: Judgment,
    pub scope_match: ScopeMatch,
    pub priority: JudgmentPriority,
    pub reasons: Vec<JudgmentReason>,
    pub requires_resolution: bool,
    pub evidence: EvidenceSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<JudgmentRelation>,
}

impl JudgmentEntry {
    /// 条件・例外を途中で切ると意味が反転するため、配信側はこの一行を丸ごと選択する。
    pub fn render_text(&self) -> Result<String> {
        Ok(format!("- {}\n", serde_json::to_string(self)?))
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct JudgmentContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_scope: Option<String>,
    #[serde(default)]
    pub entries: Vec<JudgmentEntry>,
    #[serde(default)]
    pub inspected_notes: usize,
    #[serde(default)]
    pub omitted_notes: usize,
    #[serde(default)]
    pub unresolved_references: usize,
    #[serde(default)]
    pub invalid_documents: usize,
    #[serde(default)]
    pub omitted_relations: usize,
    #[serde(default)]
    pub omitted_entries: usize,
    #[serde(default)]
    pub duplicate_entries: usize,
}

impl JudgmentContext {
    pub fn has_material(&self) -> bool {
        !self.entries.is_empty()
            || self.omitted_entries > 0
            || self.invalid_documents > 0
            || self.unresolved_references > 0
            || self.omitted_relations > 0
            || self.omitted_notes > 0
    }

    pub fn render_text(&self) -> Result<String> {
        if self.entries.is_empty() {
            return Ok(if self.has_material() {
                self.omission_text()
            } else {
                String::new()
            });
        }
        let mut text = CONTEXT_NOTICE.to_string();
        for entry in &self.entries {
            text.push_str(&entry.render_text()?);
        }
        text.push_str(&self.omission_text());
        Ok(text)
    }

    pub fn omission_text(&self) -> String {
        if self.omitted_notes == 0
            && self.unresolved_references == 0
            && self.invalid_documents == 0
            && self.omitted_relations == 0
            && self.omitted_entries == 0
            && self.duplicate_entries == 0
        {
            return String::new();
        }
        format!(
            "判断材料の探索: ノート省略{}件 / 未解決参照{}件 / 読取不能{}件 / 関係省略{}件以上 / 要約省略{}件 / 同一根拠の重複{}件。\n",
            self.omitted_notes,
            self.unresolved_references,
            self.invalid_documents,
            self.omitted_relations,
            self.omitted_entries,
            self.duplicate_entries
        )
    }
}

struct LoadedNote {
    id: String,
    note: Note,
    relations: Vec<JudgmentRelation>,
}

/// 呼び出し元のDB snapshotで検索候補と引用先を読み、本文から決定を推測しない。
pub fn context_for_notes(
    conn: &Connection,
    ranked_note_ids: &[String],
    context_scope: Option<&str>,
) -> Result<JudgmentContext> {
    if let Some(scope) = context_scope {
        Authority {
            namespace: NoteNamespace::Decisions,
            role: AuthorityRole::Canonical,
            status: AuthorityStatus::Active,
            scope: scope.into(),
        }
        .validate()?;
    }
    let mut context = JudgmentContext {
        context_scope: context_scope.map(str::to_string),
        ..JudgmentContext::default()
    };
    let mut queue = VecDeque::new();
    let mut seen = HashSet::new();
    for id in ranked_note_ids {
        enqueue(id.clone(), 0, &mut queue, &mut seen, &mut context);
    }
    let mut loaded = Vec::new();
    while let Some((id, depth)) = queue.pop_front() {
        let document: Option<String> = conn
            .query_row(
                "SELECT document FROM notes WHERE id = ?1 AND status != 'deprecated'
                   AND normal_reference_allowed = 1",
                [&id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(document) = document else {
            continue;
        };
        context.inspected_notes += 1;
        // metadataを持たない既存ノートや不正な文書で、通常retrievalを失敗させない。
        let Ok(note) = Note::parse(&document) else {
            context.invalid_documents += 1;
            continue;
        };
        let relations = if let Some(uid) = &note.front.note_uid {
            let (relations, omitted) = visible_relations(conn, uid)?;
            context.omitted_relations += omitted;
            relations
        } else {
            Vec::new()
        };
        if depth < MAX_LINK_DEPTH {
            if let Some(Judgment::Action { decision_refs, .. }) = &note.front.judgment {
                for uid in decision_refs {
                    let target: Option<String> = conn
                        .query_row(
                            "SELECT id FROM notes WHERE note_uid = ?1 AND status != 'deprecated'
                               AND normal_reference_allowed = 1",
                            [uid.as_str()],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if let Some(target) = target {
                        enqueue(target, depth + 1, &mut queue, &mut seen, &mut context);
                    } else {
                        context.unresolved_references += 1;
                    }
                }
            }
            for relation in &relations {
                enqueue(
                    relation.note_id.clone(),
                    depth + 1,
                    &mut queue,
                    &mut seen,
                    &mut context,
                );
            }
        }
        loaded.push(LoadedNote {
            id,
            note,
            relations,
        });
    }

    let evidence = evidence_by_decision(&loaded, context_scope);
    for item in &loaded {
        let (Some(uid), Some(authority), Some(judgment)) = (
            &item.note.front.note_uid,
            &item.note.front.authority,
            &item.note.front.judgment,
        ) else {
            continue;
        };
        let scope_match = match_scope(&authority.scope, context_scope);
        let mut reasons = vec![JudgmentReason::ConditionsUnchecked];
        let mut requires_resolution = false;
        let mut priority = match judgment {
            Judgment::Decision { basis, .. } if authority.is_active_canonical() => match basis {
                DecisionBasis::UserDecision | DecisionBasis::UserCorrection => {
                    reasons.push(JudgmentReason::ActiveExplicitCanonical);
                    JudgmentPriority::ExplicitDecisionCandidate
                }
                DecisionBasis::AssistantInference => {
                    reasons.push(JudgmentReason::AssistantInference);
                    JudgmentPriority::InferredDecisionCandidate
                }
            },
            Judgment::Decision { basis, .. }
                if authority.status == AuthorityStatus::Active
                    && authority.role == AuthorityRole::Record =>
            {
                reasons.push(JudgmentReason::CanonicalResolutionNeeded);
                if *basis == DecisionBasis::AssistantInference {
                    reasons.push(JudgmentReason::AssistantInference);
                }
                requires_resolution = true;
                if *basis == DecisionBasis::AssistantInference {
                    JudgmentPriority::Observation
                } else {
                    JudgmentPriority::ExplicitRecordCandidate
                }
            }
            Judgment::Action { .. } if authority.status == AuthorityStatus::Active => {
                reasons.push(JudgmentReason::ActionEvidenceDoesNotAuthorize);
                reasons.push(JudgmentReason::CanonicalResolutionNeeded);
                requires_resolution = true;
                JudgmentPriority::Observation
            }
            _ => {
                reasons.push(JudgmentReason::InactiveAuthority);
                JudgmentPriority::Inactive
            }
        };
        let summary = evidence.get(uid).cloned().unwrap_or_default();
        if matches!(
            judgment,
            Judgment::Decision {
                basis: DecisionBasis::UserCorrection,
                ..
            }
        ) || matches!(
            judgment,
            Judgment::Action {
                correction: Some(_),
                ..
            }
        ) || summary.distinct_correction_sources > 0
        {
            reasons.push(JudgmentReason::UserCorrectionRecorded);
        }
        if summary.conflicting_action_sources > 0 {
            reasons.push(JudgmentReason::SameSourceReportsDisagree);
            requires_resolution = true;
        }
        for relation in &item.relations {
            let peer = loaded.iter().find(|peer| peer.id == relation.note_id);
            let peer_authority = peer.and_then(|peer| peer.note.front.authority.as_ref());
            if relation.kind == RelationKind::Supersedes
                && relation.direction == RelationDirection::Incoming
                && peer_authority.is_some_and(|peer| {
                    peer.is_active_canonical()
                        && peer.scope == authority.scope
                        && peer.namespace == authority.namespace
                })
            {
                priority = JudgmentPriority::Inactive;
                reasons.push(JudgmentReason::SupersededByVisibleNote);
            }
            if relation.kind == RelationKind::Contradicts
                && authority.status == AuthorityStatus::Active
                && peer_authority.is_some_and(|peer| peer.status == AuthorityStatus::Active)
            {
                requires_resolution = true;
                reasons.push(JudgmentReason::VisibleContradiction);
            }
        }
        match scope_match {
            ScopeMatch::Unchecked => reasons.push(JudgmentReason::ScopeUnchecked),
            ScopeMatch::Outside => {
                reasons.push(JudgmentReason::OutsideScope);
                priority = JudgmentPriority::OutOfScope;
            }
            ScopeMatch::Matches => {}
        }
        context.entries.push(JudgmentEntry {
            note_id: item.id.clone(),
            note_uid: uid.clone(),
            title: item.note.front.title.clone(),
            authority: authority.clone(),
            judgment: judgment.clone(),
            scope_match,
            priority,
            reasons,
            requires_resolution,
            evidence: summary,
            // 実績が増えるほど正本の要約が巨大化して省略されることを避ける。
            // 通常の引用はaction内のdecision_refsに残し、ここには変更・衝突の手掛かりを出す。
            relations: item
                .relations
                .iter()
                .filter(|relation| {
                    matches!(
                        relation.kind,
                        RelationKind::Supersedes | RelationKind::Contradicts
                    )
                })
                .cloned()
                .collect(),
        });
    }
    // stable sortで同じ種類の検索順を保つ。訂正は注意を促すが決定権の種類を変えない。
    context.entries.sort_by_key(|entry| {
        (
            entry.priority,
            !entry
                .reasons
                .contains(&JudgmentReason::UserCorrectionRecorded),
        )
    });
    let mut seen_events = HashSet::new();
    let candidates = std::mem::take(&mut context.entries);
    for entry in candidates {
        let identity = format!(
            "{}\n{:?}\n{}",
            entry.authority.scope,
            entry.priority,
            serde_json::to_string(&entry.judgment)?
        );
        if !seen_events.insert(identity) {
            context.duplicate_entries += 1;
            continue;
        }
        if context.entries.len() >= MAX_CONTEXT_ENTRIES {
            context.omitted_entries += 1;
            continue;
        }
        context.entries.push(entry);
        // 最後に増える省略件数の桁数用に小さな余白を残す。
        if serde_json::to_vec(&context)?.len() > MAX_CONTEXT_BYTES - 64 {
            context.entries.pop();
            context.omitted_entries += 1;
        }
    }
    Ok(context)
}

fn enqueue(
    id: String,
    depth: u8,
    queue: &mut VecDeque<(String, u8)>,
    seen: &mut HashSet<String>,
    context: &mut JudgmentContext,
) {
    if seen.contains(&id) {
        return;
    }
    if seen.len() >= MAX_NOTES {
        context.omitted_notes += 1;
        return;
    }
    seen.insert(id.clone());
    queue.push_back((id, depth));
}

fn match_scope(authority_scope: &str, context_scope: Option<&str>) -> ScopeMatch {
    match context_scope {
        None => ScopeMatch::Unchecked,
        Some(scope)
            if scope == authority_scope
                || scope
                    .strip_prefix(authority_scope)
                    .is_some_and(|rest| rest.starts_with('/')) =>
        {
            ScopeMatch::Matches
        }
        Some(_) => ScopeMatch::Outside,
    }
}

fn visible_relations(conn: &Connection, uid: &NoteUid) -> Result<(Vec<JudgmentRelation>, usize)> {
    let mut statement = conn.prepare_cached(
        "SELECT relation.kind, peer.id, peer.note_uid, relation.src_uid = ?1 AS outgoing,
                CASE WHEN relation.kind = 'mentions' THEN peer.document ELSE NULL END
         FROM note_relations relation
         JOIN notes peer ON peer.note_uid = CASE WHEN relation.src_uid = ?1
             THEN relation.target_uid ELSE relation.src_uid END
         WHERE (relation.src_uid = ?1 OR relation.target_uid = ?1)
           AND (relation.kind != 'mentions' OR relation.target_uid = ?1)
           AND peer.status != 'deprecated' AND peer.normal_reference_allowed = 1
         ORDER BY CASE relation.kind WHEN 'supersedes' THEN 0 WHEN 'contradicts' THEN 1 ELSE 2 END,
                  peer.id, relation.kind LIMIT 65",
    )?;
    let rows = statement.query_map([uid.as_str()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, bool>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    let mut relations = Vec::new();
    let mut fetched = 0;
    for row in rows {
        let (kind, note_id, other_uid, outgoing, document) = row?;
        fetched += 1;
        if fetched > MAX_NOTES {
            break;
        }
        // decision_refsはmentionsでも保存できる。一般の言及を実績にせず、引用の一致を確かめる。
        if kind == "mentions" && !document.as_deref().and_then(|text| Note::parse(text).ok())
            .is_some_and(|note| matches!(note.front.judgment, Some(Judgment::Action { decision_refs, .. }) if decision_refs.contains(uid)))
        {
            continue;
        }
        let Ok(kind) = serde_json::from_value::<RelationKind>(serde_json::Value::String(kind))
        else {
            continue;
        };
        let Ok(note_uid) = other_uid.parse() else {
            continue;
        };
        relations.push(JudgmentRelation {
            kind,
            direction: if outgoing {
                RelationDirection::Outgoing
            } else {
                RelationDirection::Incoming
            },
            note_id,
            note_uid,
        });
    }
    let omitted = fetched.saturating_sub(MAX_NOTES);
    Ok((relations, omitted))
}

fn evidence_by_decision(
    loaded: &[LoadedNote],
    context_scope: Option<&str>,
) -> BTreeMap<NoteUid, EvidenceSummary> {
    let mut events: BTreeMap<NoteUid, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
    let mut corrections: BTreeMap<NoteUid, BTreeSet<String>> = BTreeMap::new();
    for item in loaded {
        let Some(authority) = &item.note.front.authority else {
            continue;
        };
        if authority.role != AuthorityRole::Record
            || authority.status != AuthorityStatus::Active
            || match_scope(&authority.scope, context_scope) == ScopeMatch::Outside
        {
            continue;
        }
        let Some(Judgment::Action {
            source,
            action,
            outcome,
            decision_refs,
            correction,
            ..
        }) = &item.note.front.judgment
        else {
            continue;
        };
        for uid in decision_refs {
            events
                .entry(uid.clone())
                .or_default()
                .entry(source.reference.clone())
                .or_default()
                .insert(format!("{action}\n{outcome:?}"));
            if let Some(correction) = correction {
                corrections
                    .entry(uid.clone())
                    .or_default()
                    .insert(correction.source.reference.clone());
            }
        }
    }
    events
        .into_iter()
        .map(|(uid, sources)| {
            let summary = EvidenceSummary {
                distinct_action_sources: sources.len(),
                distinct_correction_sources: corrections.get(&uid).map_or(0, BTreeSet::len),
                conflicting_action_sources: sources
                    .values()
                    .filter(|reports| reports.len() > 1)
                    .count(),
            };
            (uid, summary)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{NoteNamespace, NoteRelation};
    use crate::frontmatter::Frontmatter;
    use crate::judgment::{ActionEvidence, ActionOutcome, SourceEvidence, UserCorrection};

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE notes(id TEXT PRIMARY KEY, note_uid TEXT, status TEXT,
                document TEXT, normal_reference_allowed INTEGER DEFAULT 1);
             CREATE TABLE note_relations(src_uid TEXT, kind TEXT, target_uid TEXT);",
        )
        .unwrap();
        conn
    }

    fn source(event: &str) -> SourceEvidence {
        SourceEvidence {
            reference: format!("conversation:fixture/{event}"),
            excerpt: "本人がTerminalで実行する".into(),
        }
    }

    fn decision(scope: &str, basis: DecisionBasis) -> Note {
        let mut front = Frontmatter::new_note("反映時の実行担当");
        front.note_uid = Some(NoteUid::new());
        front.authority = Some(Authority {
            namespace: NoteNamespace::Decisions,
            role: AuthorityRole::Canonical,
            status: AuthorityStatus::Active,
            scope: scope.into(),
        });
        front.judgment = Some(Judgment::Decision {
            basis,
            source: source("decision"),
            applies_when: "アプリ反映を依頼された場合".into(),
            action: "検証済みコマンドを提示する".into(),
            exceptions: vec!["本人が実行担当の変更を明示した場合は見直す".into()],
        });
        Note {
            front,
            body: "これは合成テストの記録。".into(),
        }
    }

    fn action(target: &NoteUid, event: &str) -> Note {
        let mut note = decision("fixture/deployment", DecisionBasis::UserDecision);
        let authority = note.front.authority.as_mut().unwrap();
        authority.namespace = NoteNamespace::Records;
        authority.role = AuthorityRole::Record;
        note.front.relations = vec![NoteRelation {
            kind: RelationKind::Supports,
            target: target.clone(),
        }];
        note.front.judgment = Some(Judgment::Action {
            source: source(event),
            situation: "反映を依頼された".into(),
            action: "AIが反映を実行".into(),
            outcome: ActionOutcome::Succeeded,
            evidence: ActionEvidence::AssistantReport,
            decision_refs: vec![target.clone()],
            correction: Some(UserCorrection {
                source: source("correction"),
                action: "実行は本人がする".into(),
            }),
        });
        note
    }

    fn insert(conn: &Connection, id: &str, note: &Note) {
        conn.execute(
            "INSERT INTO notes(id,note_uid,status,document) VALUES (?1,?2,'stable',?3)",
            rusqlite::params![
                id,
                note.front.note_uid.as_ref().map(NoteUid::as_str),
                note.to_file_string().unwrap()
            ],
        )
        .unwrap();
        for relation in &note.front.relations {
            conn.execute(
                "INSERT INTO note_relations VALUES (?1,?2,?3)",
                rusqlite::params![
                    note.front.note_uid.as_ref().unwrap().as_str(),
                    relation.kind.as_str(),
                    relation.target.as_str()
                ],
            )
            .unwrap();
        }
    }

    /// 2026-09-08: 既存決定を読んでも適用されなかった事故に対し、条件を判定済みと偽らない。
    #[test]
    fn scope_checks_boundaries_and_always_preserves_conditions_and_exceptions() {
        let conn = setup();
        let note = decision("fixture/deployment", DecisionBasis::UserDecision);
        insert(&conn, "notes/decision", &note);
        for (scope, expected) in [
            (None, ScopeMatch::Unchecked),
            (Some("fixture/deployment"), ScopeMatch::Matches),
            (Some("fixture/deployment/macos"), ScopeMatch::Matches),
            (Some("fixture/deployment-other"), ScopeMatch::Outside),
            (Some("fixture"), ScopeMatch::Outside),
        ] {
            let context = context_for_notes(&conn, &["notes/decision".into()], scope).unwrap();
            let entry = &context.entries[0];
            assert_eq!(entry.scope_match, expected);
            assert!(entry.reasons.contains(&JudgmentReason::ConditionsUnchecked));
            assert_eq!(entry.judgment, note.front.judgment.clone().unwrap());
            let text = context.render_text().unwrap();
            assert!(text.contains("実行担当の変更を明示"));
            assert!(text.contains("独立検証なし"));
            if expected == ScopeMatch::Outside {
                assert_eq!(entry.priority, JudgmentPriority::OutOfScope);
            }
        }
    }

    #[test]
    fn reading_action_resolves_canonical_and_duplicate_successes_never_authorize() {
        let conn = setup();
        let canonical = decision("fixture/deployment", DecisionBasis::UserDecision);
        insert(&conn, "notes/decision", &canonical);
        let target = canonical.front.note_uid.as_ref().unwrap();
        for id in ["notes/action-a", "notes/action-b"] {
            insert(&conn, id, &action(target, "same-action"));
        }
        let context = context_for_notes(
            &conn,
            &["notes/action-a".into()],
            Some("fixture/deployment"),
        )
        .unwrap();
        assert_eq!(context.entries[0].note_id, "notes/decision");
        assert_eq!(
            context.entries[0].priority,
            JudgmentPriority::ExplicitDecisionCandidate
        );
        assert_eq!(context.entries[0].evidence.distinct_action_sources, 1);
        assert_eq!(context.entries[0].evidence.distinct_correction_sources, 1);
        assert!(
            context.entries[0]
                .reasons
                .contains(&JudgmentReason::UserCorrectionRecorded)
        );
        assert_eq!(context.duplicate_entries, 1);
        let record = context
            .entries
            .iter()
            .find(|entry| entry.note_id == "notes/action-a")
            .unwrap();
        assert_eq!(record.priority, JudgmentPriority::Observation);
        assert!(record.requires_resolution);
    }

    #[test]
    fn decision_get_follows_only_mentions_that_are_action_decision_references() {
        let conn = setup();
        let canonical = decision("fixture/deployment", DecisionBasis::UserDecision);
        insert(&conn, "notes/decision", &canonical);
        let target = canonical.front.note_uid.as_ref().unwrap();
        let mut recorded = action(target, "execution");
        recorded.front.relations[0].kind = RelationKind::Mentions;
        insert(&conn, "notes/action", &recorded);
        let mut unrelated = decision("fixture/unrelated", DecisionBasis::UserDecision);
        unrelated.front.relations.push(NoteRelation {
            kind: RelationKind::Mentions,
            target: target.clone(),
        });
        insert(&conn, "notes/unrelated", &unrelated);
        let context = context_for_notes(
            &conn,
            &["notes/decision".into()],
            Some("fixture/deployment"),
        )
        .unwrap();
        assert_eq!(context.entries[0].evidence.distinct_action_sources, 1);
        assert!(
            context
                .entries
                .iter()
                .any(|entry| entry.note_id == "notes/action")
        );
        assert!(
            !context
                .entries
                .iter()
                .any(|entry| entry.note_id == "notes/unrelated")
        );
    }

    #[test]
    fn many_action_records_cannot_push_the_explicit_decision_out_of_the_context_budget() {
        let conn = setup();
        let canonical = decision("fixture/deployment", DecisionBasis::UserDecision);
        insert(&conn, "notes/decision", &canonical);
        for index in 0..40 {
            let id = format!("notes/action-{index:02}");
            insert(
                &conn,
                &id,
                &action(
                    canonical.front.note_uid.as_ref().unwrap(),
                    &format!("execution-{index}"),
                ),
            );
        }
        let context = context_for_notes(
            &conn,
            &["notes/decision".into()],
            Some("fixture/deployment"),
        )
        .unwrap();
        assert_eq!(context.entries[0].note_id, "notes/decision");
        assert_eq!(context.entries[0].evidence.distinct_action_sources, 40);
        assert!(context.omitted_entries > 0);
        assert!(serde_json::to_vec(&context).unwrap().len() <= MAX_CONTEXT_BYTES);
    }

    #[test]
    fn explicit_record_precedes_inference_without_becoming_canonical() {
        let conn = setup();
        let inference = decision("fixture/deployment", DecisionBasis::AssistantInference);
        insert(&conn, "notes/inferred", &inference);
        let mut correction = decision("fixture/deployment", DecisionBasis::UserCorrection);
        let authority = correction.front.authority.as_mut().unwrap();
        authority.namespace = NoteNamespace::Records;
        authority.role = AuthorityRole::Record;
        correction.front.relations.push(NoteRelation {
            kind: RelationKind::Contradicts,
            target: inference.front.note_uid.clone().unwrap(),
        });
        insert(&conn, "notes/correction", &correction);
        let context = context_for_notes(&conn, &["notes/inferred".into()], None).unwrap();
        assert_eq!(context.entries[0].note_id, "notes/correction");
        assert_eq!(
            context.entries[0].priority,
            JudgmentPriority::ExplicitRecordCandidate
        );
        assert!(
            context
                .entries
                .iter()
                .all(|entry| entry.requires_resolution)
        );
        assert!(context.entries.iter().all(|entry| {
            entry
                .reasons
                .contains(&JudgmentReason::VisibleContradiction)
        }));
    }

    #[test]
    fn superseded_decision_points_to_successor_and_hidden_record_is_not_exposed() {
        let conn = setup();
        let mut old = decision("fixture/deployment", DecisionBasis::UserDecision);
        old.front.authority.as_mut().unwrap().status = AuthorityStatus::Superseded;
        insert(&conn, "notes/old", &old);
        let mut successor = decision("fixture/deployment", DecisionBasis::UserCorrection);
        successor.front.relations.push(NoteRelation {
            kind: RelationKind::Supersedes,
            target: old.front.note_uid.clone().unwrap(),
        });
        insert(&conn, "notes/new", &successor);
        insert(
            &conn,
            "notes/hidden",
            &action(successor.front.note_uid.as_ref().unwrap(), "hidden-event"),
        );
        conn.execute(
            "UPDATE notes SET normal_reference_allowed = 0 WHERE id = 'notes/hidden'",
            [],
        )
        .unwrap();
        let context =
            context_for_notes(&conn, &["notes/old".into()], Some("fixture/deployment")).unwrap();
        assert_eq!(context.entries[0].note_id, "notes/new");
        assert_eq!(context.entries[1].priority, JudgmentPriority::Inactive);
        assert!(
            context.entries[1]
                .reasons
                .contains(&JudgmentReason::SupersededByVisibleNote)
        );
        let serialized = serde_json::to_string(&context).unwrap();
        assert!(!serialized.contains("hidden"));
        assert_eq!(context.entries[0].evidence.distinct_action_sources, 0);
    }

    #[test]
    fn metadata_is_not_inferred_from_legacy_text_and_oversized_entries_are_atomic() {
        let conn = setup();
        let legacy = Note {
            front: Frontmatter::new_note("本人の指示"),
            body: "最優先: アプリをAIが実行する。".into(),
        };
        insert(&conn, "notes/legacy", &legacy);
        let empty = context_for_notes(&conn, &["notes/legacy".into()], None).unwrap();
        assert!(!empty.has_material());
        assert_eq!(empty.render_text().unwrap(), "");
        let mut oversized = decision("fixture/deployment", DecisionBasis::UserDecision);
        if let Some(Judgment::Decision {
            source,
            applies_when,
            action,
            exceptions,
            ..
        }) = &mut oversized.front.judgment
        {
            source.excerpt = "例".repeat(1_000);
            *applies_when = "条".repeat(1_000);
            *action = "動".repeat(1_000);
            *exceptions = vec!["特別な例外を落としてはいけない".into()];
        }
        insert(&conn, "notes/oversized", &oversized);
        let context = context_for_notes(&conn, &["notes/oversized".into()], None).unwrap();
        assert!(context.entries.is_empty());
        assert_eq!(context.omitted_entries, 1);
        assert!(context.has_material());
        assert!(serde_json::to_vec(&context).unwrap().len() <= MAX_CONTEXT_BYTES);
        assert!(!context.render_text().unwrap().contains("動"));
        assert!(context.render_text().unwrap().contains("要約省略1件"));
    }
}
