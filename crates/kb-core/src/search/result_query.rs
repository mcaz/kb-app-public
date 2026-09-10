//! 完了・過去結果の照会だけに使う順位根拠。状態の真偽やauthorityは変更しない。

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::Connection;

use super::{AuthorityValues, FieldValues, Hit, field_terms};

const JAPANESE_QUESTIONS: &[&str] = &[
    "完了したか",
    "完了したのか",
    "完了した？",
    "完了した?",
    "完了しましたか",
    "完了しているか",
    "完了している？",
    "完了している?",
    "完了していますか",
    "終わったか",
    "終わった？",
    "終わった?",
    "終わりましたか",
    "済んだか",
    "済んだ？",
    "済んだ?",
    "対応済みか",
    "未完了か",
    "完了状況",
    "どう分類した",
    "どう処理した",
    "どう対応した",
    "何を実施した",
    "何を変更した",
    "何が起きた",
    "実施結果",
    "処理結果",
    "調査結果",
];

/// 質問の助詞・時制を対象名の一致に数えると、無関係な完了記録が上がる。
const QUESTION_TERMS: &[&str] = &[
    "完了",
    "未完了",
    "状況",
    "結果",
    "どう",
    "何",
    "した",
    "する",
    "いる",
    "いま",
    "現在",
    "最新",
    "現行",
    "当時",
    "過去",
    "以前",
    "その",
    "この",
    "です",
    "ます",
    "ました",
    "でした",
    "について",
    "the",
    "a",
    "an",
    "of",
    "for",
    "in",
    "on",
    "to",
    "with",
    "is",
    "was",
    "were",
    "are",
    "has",
    "have",
    "had",
    "did",
    "been",
    "it",
    "what",
    "how",
    "when",
    "whether",
    "done",
    "completed",
    "finished",
    "classified",
    "resolved",
    "happened",
    "result",
    "results",
    "outcome",
    "outcomes",
    "current",
    "latest",
    "previous",
    "historical",
];

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct ResultScore {
    subject_matches: usize,
    recorded_result: bool,
    title_matches: usize,
}

pub(super) struct ResultRanking {
    terms: Vec<String>,
    scores: HashMap<String, ResultScore>,
}

impl ResultRanking {
    pub(super) fn from_query(query: &str) -> Option<Self> {
        let lower = query.to_lowercase();
        let words: Vec<_> = lower
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|word| !word.is_empty())
            .collect();
        let question_end = lower.trim_end_matches(['？', '?', '。', '.', ' ']);
        let explicit_result_question = JAPANESE_QUESTIONS.iter().any(|marker| {
            (marker.ends_with('か') || marker.ends_with(['？', '?']))
                && question_end.ends_with(marker.trim_end_matches(['？', '?']))
        }) || words.last().is_some_and(|last| {
            ["done", "completed", "finished", "classified", "resolved"].contains(last)
        });
        // 結果という名詞を含む操作手順もある。対象名の「手順」自体は除外しない。
        if !explicit_result_question
            && ([
                "の方法",
                "の手順",
                "分類方法",
                "処理方法",
                "完了条件",
                "受入条件",
                "やり方",
            ]
            .iter()
            .any(|marker| lower.contains(marker))
                || lower.contains("how to ")
                || words
                    .iter()
                    .any(|word| ["procedure", "procedures", "criteria"].contains(word)))
        {
            return None;
        }
        let english = (words
            .iter()
            .any(|w| ["is", "has", "have", "was", "were", "did"].contains(w))
            && words
                .iter()
                .any(|w| ["done", "completed", "finished", "classified", "resolved"].contains(w)))
            || (words.iter().any(|w| ["what", "how"].contains(w))
                && words
                    .iter()
                    .any(|w| ["result", "results", "outcome", "outcomes", "happened"].contains(w)));
        let japanese = JAPANESE_QUESTIONS
            .iter()
            .any(|marker| lower.contains(marker));
        if !japanese && !english {
            return None;
        }
        let mut subject = lower;
        for marker in JAPANESE_QUESTIONS {
            subject = subject.replace(marker, " ");
        }
        let terms: Vec<_> = field_terms(&subject)
            .into_iter()
            .filter(|term| {
                (term.chars().count() >= 2 || term.chars().all(|c| c.is_ascii_digit()))
                    && !QUESTION_TERMS.contains(&term.as_str())
            })
            .collect();
        if terms.is_empty() {
            return None;
        }
        Some(Self {
            terms,
            scores: HashMap::new(),
        })
    }

    pub(super) fn match_expr(&self, any: bool) -> String {
        self.terms
            .iter()
            .map(|term| format!("\"{}\"", term.replace('"', "")))
            .collect::<Vec<_>>()
            .join(if any { " OR " } else { " AND " })
    }

    pub(super) fn remember(
        &mut self,
        id: &str,
        fields: &FieldValues<'_>,
        authority: AuthorityValues<'_>,
    ) -> ResultScore {
        let score = self.score(fields, authority);
        self.scores.insert(id.to_owned(), score);
        score
    }

    pub(super) fn for_hit(&self, hit: &Hit) -> ResultScore {
        self.scores.get(&hit.id).copied().unwrap_or_default()
    }

    fn score(&self, fields: &FieldValues<'_>, authority: AuthorityValues<'_>) -> ResultScore {
        let title = fields.title.unwrap_or_default().to_lowercase();
        let description = fields.description.unwrap_or_default().to_lowercase();
        let scope = fields.scope.unwrap_or_default().to_lowercase();
        let subject_matches = self
            .terms
            .iter()
            .filter(|term| {
                contains_term(&title, term)
                    || contains_term(&description, term)
                    || contains_term(&scope, term)
            })
            .count();
        if subject_matches == 0 {
            return ResultScore::default();
        }
        let is_record = authority.role == Some("record")
            || (authority.namespace == Some("initiatives") && authority.role == Some("canonical"));
        ResultScore {
            subject_matches,
            recorded_result: is_record
                && matches!(authority.status, Some("active" | "historical"))
                && has_result_evidence(fields.body),
            title_matches: self
                .terms
                .iter()
                .filter(|term| contains_term(&title, term))
                .count(),
        }
    }

    /// anchor/semantic/rescueだけで見つかった候補にも、短いsnippetでなくDB本文を使う。
    pub(super) fn load_missing(&mut self, conn: &Connection, hits: &[Hit]) -> Result<()> {
        let mut stmt = conn.prepare_cached(
            "SELECT title,description,authority_scope,body FROM notes WHERE id=?1",
        )?;
        for hit in hits {
            if self.scores.contains_key(&hit.id) {
                continue;
            }
            let (title, description, scope, body) = stmt.query_row([&hit.id], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            self.remember(
                &hit.id,
                &FieldValues {
                    title: title.as_deref(),
                    description: description.as_deref(),
                    tags: None,
                    namespace: hit.namespace.as_deref(),
                    scope: scope.as_deref(),
                    body: &body,
                },
                AuthorityValues {
                    namespace: hit.namespace.as_deref(),
                    role: hit.authority_role.as_deref(),
                    status: hit.authority_status.as_deref(),
                },
            );
        }
        Ok(())
    }
}

fn has_result_evidence(body: &str) -> bool {
    // 結果見出しや仮定だけは根拠にしない。実施記録と次の予定が共存するため文ごとに見る。
    // 成功だけを好むと「終わったか」に未完了・失敗という回答を返せなくなる。
    body.to_lowercase()
        .split_inclusive(['。', '.', '\n', '！', '!', '？', '?'])
        .any(|sentence| {
            let sentence = sentence.trim();
            if sentence.ends_with(['？', '?'])
                || ["is ", "has ", "have ", "was ", "were ", "did "]
                    .iter()
                    .any(|prefix| sentence.starts_with(prefix))
            {
                return false;
            }
            let future_plan = sentence.contains("予定")
                && !["予定どおり", "予定通り", "予定より"]
                    .iter()
                    .any(|marker| sentence.contains(marker));
            if future_plan
                || [
                    "場合",
                    "なら",
                    "を条件",
                    "追記する",
                    "記載する",
                    "記録する",
                    "とする",
                    "か確認",
                    "かを確認",
                ]
                .iter()
                .any(|marker| sentence.contains(marker))
                || [
                    "if", "once", "should", "will", "would", "must", "whether", "check", "confirm",
                ]
                .iter()
                .any(|marker| contains_term(sentence, marker))
            {
                return false;
            }
            [
                "完了した",
                "完了しました",
                "完了済み",
                "対応済み",
                "終わった",
                "済んだ",
                "実施済み",
                "分類した",
                "未完了",
                "未実施",
                "未着手",
                "進行中",
                "結果不明",
                "結果は不明",
                "結果未確認",
                "失敗した",
                "中止した",
                "保留中",
            ]
            .iter()
            .any(|marker| sentence.contains(marker))
                || [
                    "completed",
                    "done",
                    "finished",
                    "resolved",
                    "classified",
                    "failed",
                    "pending",
                    "unknown",
                    "cancelled",
                ]
                .iter()
                .any(|marker| contains_term(sentence, marker))
        })
}

fn contains_term(text: &str, term: &str) -> bool {
    if term.is_ascii() {
        text.split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| word == term)
    } else {
        text.contains(term)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-09-08: 状態照会と手順照会を分け、語だけで成功や過去優先を推測しない。
    #[test]
    fn only_explicit_result_questions_with_a_subject_enable_ranking() {
        for query in [
            "Lumen移行は完了した？",
            "Vega調査で3項目をどう分類したか",
            "Is lumen migration completed?",
            "How were vega entries classified?",
            "lumen移行は未完了か",
            "Lumen手順修正は完了したか",
            "Lumenの手順修正は完了したか",
            "Lumenの完了条件の見直しは完了した？",
            "Has the migration procedure revision completed?",
            "What happened to lumen migration?",
        ] {
            assert!(ResultRanking::from_query(query).is_some(), "{query}");
        }
        for query in [
            "完了したか",
            "lumen移行の完了条件",
            "vega項目をどう分類するか",
            "現在の移行手順",
            "migration completion criteria",
            "completed migration procedure",
            "lumen移行が完了した場合の手順",
            "現在のLumen調査結果の分類方法",
            "What is the procedure for completed migrations?",
            "How to review migration results",
        ] {
            assert!(ResultRanking::from_query(query).is_none(), "{query}");
        }
    }

    #[test]
    fn subject_matching_does_not_match_an_ascii_substring() {
        assert!(!contains_term("pattern results", "pat"));
        assert!(contains_term("pat / results", "pat"));
        assert!(contains_term("lumen移行", "lumen"));
        assert!(contains_term("移行lumenは完了した", "lumen"));
    }

    /// 2026-09-08: 完了語を含む予定・条件を、実際の完了報告と取り違えない。
    #[test]
    fn result_evidence_distinguishes_observation_from_plans_in_each_sentence() {
        for body in [
            "実施結果を追記する予定。",
            "完了した場合に担当者へ通知する。",
            "完了したか確認する予定。",
            "If the migration completed, notify the owner.",
            "The migration will be completed tomorrow.",
            "## 調査結果\nここに結果を記載する。",
            "Outcome: to be recorded.",
            "Lumen移行は完了した？",
            "Lumen移行が完了したか確認する。",
            "Lumen移行が完了したかを確認する。",
            "Is lumen migration completed?",
            "Check whether lumen migration completed.",
        ] {
            assert!(!has_result_evidence(body), "{body}");
        }
        for body in [
            "移行は完了した。次の監査は実施予定。",
            "移行は未完了。完了した場合に担当者へ通知する。",
            "The migration completed. The next review will start tomorrow.",
            "3項目を分類した。",
            "Lumen移行は予定どおり完了した。",
            "Lumen移行は完了条件を満たして完了した。",
        ] {
            assert!(has_result_evidence(body), "{body}");
        }
    }

    /// 対象の一致を結果語より優先し、題名だけの結果や計画を実施の証拠にしない。
    #[test]
    fn result_evidence_requires_body_and_cannot_override_subject_relevance() {
        let ranking = ResultRanking::from_query("Is lumen migration completed?").unwrap();
        let score = |title, body, role, status| {
            ranking.score(
                &FieldValues {
                    title: Some(title),
                    description: None,
                    tags: None,
                    namespace: Some("records"),
                    scope: None,
                    body,
                },
                AuthorityValues {
                    namespace: Some("records"),
                    role: Some(role),
                    status: Some(status),
                },
            )
        };
        let current = score(
            "Lumen migration procedure",
            "Migration criteria",
            "canonical",
            "active",
        );
        let unrelated = score(
            "Orion migration result",
            "Migration completed.",
            "record",
            "active",
        );
        assert!(current > unrelated);
        let metadata_only = score(
            "Lumen migration completed",
            "Migration criteria",
            "record",
            "active",
        );
        assert!(!metadata_only.recorded_result);
        let heading_only = score(
            "Lumen migration",
            "完了判定の条件を確認する。",
            "record",
            "active",
        );
        assert!(!heading_only.recorded_result);
        for body in [
            "Migration completed.",
            "Migration done.",
            "Migration resolved.",
            "Migration failed.",
            "Migration pending.",
            "Result unknown.",
            "移行は未完了。",
            "移行の結果は不明。",
        ] {
            let result = score("Lumen migration", body, "record", "active");
            assert!(result.recorded_result, "{body}");
            assert!(result > current, "{body}");
        }
        assert!(
            !score(
                "Lumen migration",
                "Migration completed.",
                "proposal",
                "active"
            )
            .recorded_result
        );
        assert!(
            !score(
                "Lumen migration",
                "Migration completed.",
                "record",
                "superseded"
            )
            .recorded_result
        );
        assert!(
            score(
                "Lumen migration",
                "Migration completed.",
                "record",
                "historical"
            )
            .recorded_result
        );
    }
}
