//! タグ語彙の統治 — 契約1の形式検証と、語彙外タグの拒否。
//!
//! **なぜコアに置くか**(2026-08-12 本人決定)。「どのタグを使うか」は運用であり
//! KB の「タグ運用」ノートで合意する可変の領域だが、**語彙を膨らませないための強制は
//! 機構の仕事**。実測で 60 ノートに対しユニークタグ 112 語・うち 61% が1回限りまで
//! 増殖しており、運用ルール(常駐文章)では止まらないことが確認された。
//! 強制の階段(KB「AI 協働アプリの規律は文章でなく機構で確定させる」)に従い、
//! 最弱の「常駐文章」から「スキーマ・コア検証」へ引き上げる。
//!
//! GUI はノートを作らない。MCP・CLI・旧 Markdown import の全書き込み経路をここへ
//! 合流させ、入口ごとの検証忘れを作らない。

use crate::write_rejection::WriteRejection;
use anyhow::Result;
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};

/// タグの最大長(従来の契約検証を踏襲)。
const MAX_LEN: usize = 20;

/// 提示する語彙の上限。現状36語なので実質全量が出る — 増殖の構造的原因は
/// 「起票時に頻度上位20語しか見えず、既存語に気づかず新語を作る」ことだった。
const VOCAB_SHOWN: usize = 60;

/// 契約1: ノートはタグを1〜4個持つ。語彙判定より前に構造を確定する。
pub fn validate_structure(tags: &[String]) -> Result<()> {
    if tags.is_empty() || tags.len() > 4 {
        return Err(WriteRejection::TagCount.validation(format!(
            "契約: ノートにはタグを1〜4個付ける(いまは {} 個)。既存の語彙に揃えること",
            tags.len()
        )));
    }
    for tag in tags {
        validate_shape(tag)?;
    }
    Ok(())
}

/// 契約1: タグは英小文字・数字・ハイフンのみ、先頭は英数。
///
/// 表記ゆれ(大文字・日本語・単複)が増殖の主因だったため形を固定する
/// (実例: `ai-agent`/`ai-agents`、`skill`/`skills`、`知見管理`/`knowledge-management`)。
pub fn validate_shape(tag: &str) -> Result<()> {
    if tag.is_empty() {
        return Err(WriteRejection::TagShape.validation("契約: 空のタグは付けられない"));
    }
    if tag.chars().count() > MAX_LEN {
        return Err(WriteRejection::TagShape
            .validation(format!("契約: タグは{MAX_LEN}文字以内(不正: 「{tag}」)")));
    }
    let first_ok = tag
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest_ok = tag
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !first_ok || !rest_ok {
        return Err(WriteRejection::TagShape.validation(format!(
            "契約: タグは英小文字・数字・ハイフンのみで先頭は英数(不正: 「{tag}」)。\
日本語・大文字・空白は使わない"
        )));
    }
    Ok(())
}

/// 「タグ運用」ノートから読み取った語彙。
#[derive(Debug, Default)]
pub struct Glossary {
    /// 語彙ノートの ID(見つからなければ None)。
    pub note_id: Option<String>,
    /// タグ → 説明。
    pub entries: BTreeMap<String, String>,
    /// 語彙表の中で形式検証に落ちた行(そのまま捨てず可視化する)。
    pub skipped: Vec<String>,
}

/// 「タグ運用」ノートの `## 語彙` 節にある表だけを語彙として読む。
///
/// **本文全体を舐めてはいけない**。以前の実装は全行から `|` の表と `- 語: 説明` 形式を
/// 拾っていたため、普通の箇条書き・見出し・URL(`https:` が区切りとして割れる)が
/// 偽タグとして UI のタグ一覧に出た(2026-08-12 に実際に発生、16語混入)。
/// 読む範囲を節で限定し、さらに形式検証を通すのが再発防止の要。
pub fn glossary(conn: &Connection) -> Result<Glossary> {
    let found: Option<(String, String)> = conn
        .query_row(
            "SELECT id, body FROM notes
             WHERE status != 'deprecated' AND normal_reference_allowed = 1
               AND (title LIKE '%タグ運用%' OR title LIKE '%タグの運用%')
             LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let Some((note_id, body)) = found else {
        return Ok(Glossary::default());
    };

    let mut g = Glossary {
        note_id: Some(note_id),
        ..Default::default()
    };
    let mut in_section = false;
    for line in body.lines() {
        let line = line.trim();
        if let Some(heading) = line.strip_prefix('#') {
            // 見出しに入るたびに節を判定し直す(語彙節を出たら読むのをやめる)
            in_section = heading
                .trim_start_matches('#')
                .trim_start()
                .starts_with("語彙");
            continue;
        }
        if !in_section || !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() < 2 {
            continue;
        }
        let (tag, desc) = (cells[0], cells[1]);
        // 区切り行(|---|---|)と見出し行は語彙ではない
        if tag.is_empty()
            || desc.is_empty()
            || tag.chars().all(|c| c == '-' || c == ':')
            || matches!(tag, "タグ" | "tag" | "名前")
        {
            continue;
        }
        match validate_shape(tag) {
            Ok(()) => {
                g.entries.insert(tag.to_string(), desc.to_string());
            }
            // 語彙表の中にある以上は「タグのつもりで書かれた行」— 黙って捨てず数える
            Err(_) => g.skipped.push(tag.to_string()),
        }
    }
    Ok(g)
}

/// 現在の語彙。「タグ運用」ノートがあれば、その語彙表だけを正本にする。
///
/// 語彙ノートがまだ無い新規 vault だけは、現に使われているタグからブートストラップする。
/// 語彙表と現用タグを無条件に合成すると、外部編集や旧 import で一度混入した語が自動的に
/// 正式語彙へ昇格してしまうため、合意済みの語彙表がある場合は fallback を使わない。
pub fn vocabulary(conn: &Connection) -> Result<BTreeSet<String>> {
    Ok(validator(conn)?.vocabulary)
}

/// 1回の検知・書き込みで使うタグ契約のスナップショット。
///
/// 語彙表が存在するかを語彙集合とは別に持つ。語彙表に有効な行が0件でも、それを
/// 「新規 vault なので語彙検証をしない」と誤認しないため。
pub(crate) struct TagValidator {
    vocabulary: BTreeSet<String>,
    enforce_vocabulary: bool,
}

pub(crate) fn validator(conn: &Connection) -> Result<TagValidator> {
    let glossary = glossary(conn)?;
    if glossary.note_id.is_some() {
        return Ok(TagValidator {
            vocabulary: glossary.entries.into_keys().collect(),
            enforce_vocabulary: true,
        });
    }
    let vocabulary: BTreeSet<String> = crate::search::tag_counts(conn, 1000)?
        .into_iter()
        .map(|(tag, _)| tag)
        .collect();
    let enforce_vocabulary = !vocabulary.is_empty();
    Ok(TagValidator {
        vocabulary,
        enforce_vocabulary,
    })
}

/// タグ契約を一括検証する唯一の入口(個数・形・語彙)。
///
/// - `allow_new` が真なら通す。「新語は2本目のノートが見えたときだけ作る」という
///   合意を、明示のフラグとして機構化したもの(運用ノート v1・2026-08-12)
/// - 語彙が空(新規 vault・索引が空)のときは素通し。立ち上げを塞がないため
pub fn validate(conn: &Connection, tags: &[String], allow_new: bool) -> Result<()> {
    validator(conn)?.validate(tags, allow_new)
}

impl TagValidator {
    pub(crate) fn validate(&self, tags: &[String], allow_new: bool) -> Result<()> {
        validate_structure(tags)?;
        if allow_new || !self.enforce_vocabulary {
            return Ok(());
        }
        let vocab = &self.vocabulary;
        let unknown: Vec<&String> = tags.iter().filter(|t| !vocab.contains(*t)).collect();
        if unknown.is_empty() {
            return Ok(());
        }

        let hints: Vec<String> = unknown
            .iter()
            .map(|t| {
                let near = nearest(vocab, t, 3);
                if near.is_empty() {
                    format!("「{t}」")
                } else {
                    format!("「{t}」→ 近い既存語: {}", near.join(" / "))
                }
            })
            .collect();
        let shown: Vec<&str> = vocab.iter().take(VOCAB_SHOWN).map(String::as_str).collect();
        let more = vocab.len().saturating_sub(shown.len());
        let tail = if more > 0 {
            format!("(ほか{more}語)")
        } else {
            String::new()
        };
        Err(WriteRejection::TagVocabulary.validation(format!(
            "契約: 語彙にないタグは使えない。{}\n現在の語彙({}語): {}{}\n\
既存語で8割合うならそれを使う。どうしても新語が要るなら allow_new_tags を true にして\
呼び直す(合意は KB の「タグ運用」ノート)",
            hints.join(" / "),
            vocab.len(),
            shown.join(" / "),
            tail
        )))
    }
}

/// 編集距離の近い順に上位 n 件(表記ゆれの吸収が目的なので単純な実装で足りる)。
fn nearest(vocab: &BTreeSet<String>, tag: &str, n: usize) -> Vec<String> {
    let mut scored: Vec<(usize, &String)> = vocab
        .iter()
        .map(|v| (distance(tag, v), v))
        .filter(|(d, v)| {
            // 遠すぎる語を並べても迷わせるだけ。距離の閾値か、意味のある部分一致を採る。
            // 部分一致に長さ下限を置くのは、短い語が偶然埋まっているだけの候補
            // (`k-now-ledge` に対する `now`)を出さないため
            let contained = |a: &str, b: &str| a.len() >= 4 && b.contains(a);
            *d <= (tag.chars().count().max(v.chars().count()) / 2).max(2)
                || contained(tag, v)
                || contained(v, tag)
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(b.1)));
    scored.into_iter().take(n).map(|(_, v)| v.clone()).collect()
}

/// レーベンシュタイン距離。
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontmatter::{Frontmatter, Note};
    use crate::index::{open_db, sync};
    use crate::vault::Vault;

    fn setup() -> (tempfile::TempDir, Vault, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let conn = open_db(&vault).unwrap();
        (dir, vault, conn)
    }

    #[test]
    fn shape_rejects_stray_forms() {
        assert!(validate_shape("knowledge-base").is_ok());
        assert!(validate_shape("two-tier").is_ok());
        assert!(validate_shape("知見管理").is_err()); // 日本語
        assert!(validate_shape("AI協働").is_err()); // 大文字+日本語
        assert!(validate_shape("open questions").is_err()); // 空白
        assert!(validate_shape("-lead").is_err()); // 先頭がハイフン
        assert!(validate_shape("").is_err());
    }

    #[test]
    fn cardinality_is_enforced_even_when_new_tags_are_allowed() {
        let (_d, _v, conn) = setup();
        assert!(validate(&conn, &[], true).is_err());
        assert!(
            validate(
                &conn,
                &["a".into(), "b".into(), "c".into(), "d".into(), "e".into()],
                true,
            )
            .is_err()
        );
    }

    /// 2026-08-12 の事故の再現テスト。語彙節の外にある普通の本文
    /// (箇条書き・URL・別の表)を語彙として拾ってはいけない。
    #[test]
    fn glossary_reads_only_the_vocabulary_section() {
        let (_d, vault, conn) = setup();
        let body = "\
## このノートの位置づけ

- 契約本体は [docs/contract.md](https://github.com/mcaz/kb-app) 側にあり、このノートはその外側
- **統合後**: 36 語 / 1 回きり 5 語(いずれも軸語彙として意図的に残置)

| 軸 | 語 |
|---|---|
| 主題 | kb-app / knowledge-base |

## 語彙(36 語)

| タグ | 説明 |
|---|---|
| kb-app | 主題: このアプリ自体 |
| knowledge-base | 主題: ナレッジベース一般 |
| 知見管理 | 主題: 日本語タグは語彙に入れない |

## 実施記録

- ops: 日々の運用 — これは語彙表の外なので拾わない
";
        vault
            .propose_for_test(
                "タグ運用 — 合意の置き場",
                body,
                None,
                &["kb-app".into()],
                "test/client",
            )
            .unwrap();
        sync(&vault, &conn).unwrap();

        let g = glossary(&conn).unwrap();
        assert_eq!(
            g.entries.keys().collect::<Vec<_>>(),
            vec!["kb-app", "knowledge-base"]
        );
        assert_eq!(g.skipped, vec!["知見管理".to_string()]);
        assert!(g.note_id.is_some());

        // 語彙表がある vault では、外部編集で混入した現用タグを正式語彙へ昇格させない。
        let mut front = Frontmatter::new_note("混入");
        front.origin = Some("agent".into());
        front.tags = vec!["stray".into()];
        vault
            .write_note_fixture(
                "notes/混入",
                &Note {
                    front,
                    body: "本文".into(),
                },
            )
            .unwrap();
        sync(&vault, &conn).unwrap();
        assert_eq!(
            vocabulary(&conn).unwrap(),
            BTreeSet::from(["kb-app".to_string(), "knowledge-base".to_string()])
        );
        assert!(validate(&conn, &["stray".into()], false).is_err());
    }

    /// 2026-09-06: 未採用票内の語彙案が、通常書込のタグ正本やエラーの既存語一覧へ昇格しない。
    #[test]
    fn unapproved_tag_proposal_cannot_become_the_glossary() {
        let (_d, vault, conn) = setup();
        let ticket = crate::proposal_workflow::create(
            &vault,
            &conn,
            crate::proposal_workflow::ProposalInput {
                title: "タグ運用の改訂提案".into(),
                problem: "語彙を見直したい".into(),
                proposal:
                    "## 語彙\n\n| タグ | 説明 |\n|---|---|\n| unapproved-term | 未採用の語彙案 |\n"
                        .into(),
                impact: "既存ノートのタグ選択に影響する".into(),
                acceptance: "本人のレビューと採否を経て反映する".into(),
                tags: vec!["kb-app".into()],
                scope: "kb-app/tags".into(),
            },
            "test/client",
        )
        .unwrap();
        assert_eq!(
            ticket.ticket.status,
            crate::proposal_workflow::TicketStatus::ReviewPending
        );
        assert!(glossary(&conn).unwrap().note_id.is_none());
        // 語彙正本がないときの使用済みタグfallbackは維持し、本文の提案語彙だけを除く。
        assert_eq!(
            vocabulary(&conn).unwrap(),
            BTreeSet::from(["kb-app".into()])
        );

        let glossary_id = vault
            .propose_for_test(
                "タグ運用 — 合意の置き場",
                "## 語彙\n\n| タグ | 説明 |\n|---|---|\n| kb-app | 合意済みの語彙 |\n",
                None,
                &["kb-app".into()],
                "test/client",
            )
            .unwrap();
        sync(&vault, &conn).unwrap();
        assert_eq!(glossary(&conn).unwrap().note_id, Some(glossary_id));
        assert_eq!(
            vocabulary(&conn).unwrap(),
            BTreeSet::from(["kb-app".into()])
        );
        validate(&conn, &["kb-app".into()], false).unwrap();
        let error = validate(&conn, &["unknown-term".into()], false).unwrap_err();
        assert!(!error.to_string().contains("unapproved-term"));
    }

    #[test]
    fn unknown_tag_is_rejected_with_suggestions() {
        let (_d, vault, conn) = setup();
        vault
            .propose_for_test(
                "既存ノート",
                "本文",
                None,
                &["knowledge-base".into(), "kb-app".into()],
                "test",
            )
            .unwrap();
        sync(&vault, &conn).unwrap();

        // 既存語に近い新語は拒否され、近い語が示される
        let err = validate(&conn, &["knowledge-bases".into()], false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("語彙にないタグは使えない"), "{msg}");
        assert!(msg.contains("knowledge-base"), "{msg}");
        // 既存語なら通る
        validate(&conn, &["kb-app".into()], false).unwrap();
        // 明示フラグがあれば新語も通る
        validate(&conn, &["brand-new".into()], true).unwrap();
    }

    #[test]
    fn empty_vocabulary_does_not_block_bootstrap() {
        let (_d, _v, conn) = setup();
        validate(&conn, &["anything".into()], false).unwrap();
    }

    #[test]
    fn empty_glossary_still_enforces_the_vocabulary_boundary() {
        let (_d, vault, conn) = setup();
        vault
            .propose_for_test(
                "タグ運用 — 合意の置き場",
                "## 語彙\n\n| タグ | 説明 |\n|---|---|\n",
                None,
                &["kb-app".into()],
                "test/client",
            )
            .unwrap();
        sync(&vault, &conn).unwrap();

        assert!(vocabulary(&conn).unwrap().is_empty());
        assert!(validate(&conn, &["anything".into()], false).is_err());
        validate(&conn, &["anything".into()], true).unwrap();
    }

    #[test]
    fn shape_is_enforced_even_when_new_tags_are_allowed() {
        let (_d, _v, conn) = setup();
        assert!(validate(&conn, &["日本語".into()], true).is_err());
    }
}
