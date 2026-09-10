//! FR-C7 お手入れ(最小形・ローカル完結)。ゼロベース設計 — 旧 KB のルーチン群の
//! 移植ではない(2026-08-10 本人方針)。
//!
//! 検知 → 平易な提案に変換 → 受信箱で「はい / いいえ」。ユーザーは維持作業を計画しない
//! (原則8)。実行は常に承諾経由で、メモ(origin: human)への操作は「つなげる」まで(原則9)。
//! v1 の検知: ①リンク切れ ②タグ契約違反(個数・形・語彙)。
//! **意味的な近接の「つなげますか?」は 2026-08-11 に撤去** — ノート間の関連を決めるのは
//! AI の領分であり、ユーザーに二択で決めさせるのは方針に反する。近さの提示は
//! 「近いノート」パネル(人間の閲覧用)と MCP の get 応答(AI の判断材料)が担う。

use anyhow::Result;
use rusqlite::Connection;

use crate::vault::Vault;

/// 未処理の提案の総数上限(通知疲れの抑制)。処理されて枠が空いたら次を補充する。
const MAX_OPEN_PROPOSALS: usize = 5;

#[derive(Debug, Clone, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CareProposal {
    pub key: String,
    pub kind: String, // "broken" | "untagged" | "invalid-tags" | "glossary"
    pub a: String,
    pub b: String,
    pub detail: String,
}

pub fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS care_proposals(
            key TEXT PRIMARY KEY, kind TEXT, a TEXT, b TEXT, detail TEXT,
            status TEXT DEFAULT 'open'
        );",
    )?;
    Ok(())
}

/// 検知を1周回し、新しい提案を受信箱へ積む。戻り値 = 新規提案数。
/// 既知(open/dismissed 問わず)の key は再提案しない — 「いいえ」は尊重される。
pub fn detect(conn: &Connection, vault: &Vault) -> Result<usize> {
    crate::tag_vocabulary_source::ensure_workspace(vault, conn)?;
    init_schema(conn)?;
    let overview = crate::tags::vocabulary_overview(conn)?;
    let source_problem = overview.source_problem();
    let source_key = source_problem.as_ref().map(|_| {
        let identity = overview
            .source
            .as_ref()
            .map(|source| source.revision.as_str())
            .unwrap_or("unconfigured");
        format!(
            "glossary-source:{identity}:{:?}:{}",
            overview.source_status,
            overview.candidates.len()
        )
    });
    // 正本の修復後に、古い修復案内だけが残って操作を促さないようにする。
    conn.execute(
        "UPDATE care_proposals SET status='resolved' WHERE status='open' AND key LIKE 'glossary-source:%' AND key != coalesce(?1, '')",
        [source_key.as_deref()],
    )?;
    let open_now: usize = conn.query_row(
        "SELECT count(*) FROM care_proposals WHERE status='open'",
        [],
        |r| r.get::<_, i64>(0),
    )? as usize;
    let budget = MAX_OPEN_PROPOSALS.saturating_sub(open_now);
    if budget == 0 {
        return Ok(0);
    }
    let mut added = 0;

    // ②' タグ契約違反の可視化。外部編集や旧データは読み取りを止めず、care へ出す。
    let notes: Vec<(String, String, String)> = {
        let mut stmt = conn.prepare_cached(
            "SELECT id, coalesce(title, id), coalesce(tags, '') FROM notes
             WHERE status != 'deprecated' ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    // 正本の欠損を全ノートのタグ違反として水増しせず、修復対象を1件だけ示す。
    let source_detail = match overview.source_status {
        crate::tags::SourceStatus::Unconfigured => {
            "タグ語彙の正本が未指定です。AIに語彙の確認と指定を頼めます。"
        }
        crate::tags::SourceStatus::Missing => {
            "タグ語彙の正本が見つかりません。AIに復元または指定先の確認を頼めます。"
        }
        crate::tags::SourceStatus::Unavailable => {
            "タグ語彙の正本を参照できません。AIに正本の状態や指定先の確認を頼めます。"
        }
        crate::tags::SourceStatus::Pinned => "",
    };
    if let Some(key) = &source_key
        && conn.execute(
            "INSERT INTO care_proposals(key, kind, a, b, detail) VALUES(?1, 'glossary', '', '', ?2)
             ON CONFLICT(key) DO UPDATE SET status='open', detail=excluded.detail WHERE care_proposals.status='resolved'",
            [key.as_str(), source_detail],
        )? != 0
    {
        added += 1;
    }
    let tag_validator = crate::tags::validator(conn)?;
    for (id, title, raw_tags) in notes {
        if added >= budget {
            break;
        }
        let tags: Vec<String> = raw_tags.split_whitespace().map(String::from).collect();
        let validation = if source_problem.is_some() {
            crate::tags::validate_structure(&tags)
        } else {
            tag_validator.validate(&tags, false)
        };
        let Err(error) = validation else {
            continue;
        };
        let (kind, key, detail) = if tags.is_empty() {
            (
                "untagged",
                format!("untagged:{id}"),
                format!("「{title}」にタグがありません(契約: 1〜4個)。Claude に整理を頼めます。"),
            )
        } else {
            let reason = error
                .to_string()
                .lines()
                .next()
                .unwrap_or("タグ契約に違反している")
                .to_string();
            (
                "invalid-tags",
                format!("invalid-tags:{id}"),
                format!("「{title}」のタグが契約違反です({reason})。Claude に整理を頼めます。"),
            )
        };
        if insert_new(conn, &key, kind, &id, "", &detail)? {
            added += 1;
        }
    }

    // ③ 語彙表に読めない行(沈黙しない — fail-open を選んだ経路は劣化を可視化する)。
    // 語彙表の行を黙って捨てると、合意したはずのタグが一覧から消えたまま気づけない。
    if added < budget
        && !overview.skipped.is_empty()
        && let Some(note) = overview.glossary_note.clone()
    {
        let key = format!("glossary:{note}:{}", overview.skipped.len());
        let detail = format!(
            "「タグ運用」ノートの語彙表に、タグとして読めない行が {} 行あります({})。\
形は英小文字・数字・ハイフンです。",
            overview.skipped.len(),
            overview.skipped.join("、")
        );
        if insert_new(conn, &key, "glossary", &note, "", &detail)? {
            added += 1;
        }
    }

    // ② リンク切れ(未執筆の知識の気づき。エラーではない — OKF §6.1)
    let broken: Vec<(String, String)> = {
        let mut stmt = conn.prepare_cached(
            "SELECT l.src, l.dst FROM links l
             LEFT JOIN notes d ON d.id = l.dst
             JOIN notes s ON s.id = l.src AND s.status != 'deprecated'
             WHERE d.id IS NULL",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    for (src, dst) in broken {
        if added >= budget {
            break;
        }
        let key = format!("broken:{src}:{dst}");
        let title: String = conn
            .query_row(
                "SELECT coalesce(title, id) FROM notes WHERE id=?1",
                [&src],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| src.clone());
        let detail =
            format!("「{title}」の中のリンク先「{dst}」がまだありません(未執筆の知識かも)。");
        if insert_new(conn, &key, "broken", &src, &dst, &detail)? {
            added += 1;
        }
    }
    Ok(added)
}

fn insert_new(
    conn: &Connection,
    key: &str,
    kind: &str,
    a: &str,
    b: &str,
    detail: &str,
) -> Result<bool> {
    let n = conn.execute(
        "INSERT OR IGNORE INTO care_proposals(key, kind, a, b, detail) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![key, kind, a, b, detail],
    )?;
    Ok(n > 0)
}

pub fn list_open(conn: &Connection) -> Result<Vec<CareProposal>> {
    init_schema(conn)?;
    let mut stmt = conn.prepare_cached(
        "SELECT key, kind, a, b, detail FROM care_proposals WHERE status='open' ORDER BY key",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(CareProposal {
            key: r.get(0)?,
            kind: r.get(1)?,
            a: r.get(2)?,
            b: r.get(3)?,
            detail: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// 「いいえ(このまま)」— 同じ提案は二度と出ない。
pub fn dismiss(conn: &Connection, key: &str) -> Result<()> {
    conn.execute(
        "UPDATE care_proposals SET status='dismissed' WHERE key=?1",
        [key],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontmatter::{Frontmatter, Note};
    use crate::index::open_db;

    /// 埋め込みを手挿入して検知経路をテスト(モデル不要にするため broken 経路中心)。
    #[test]
    fn broken_link_and_dismiss_flow() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        // 契約違反の既存データを raw fixture で再現し、検知側の自己修復を確かめる。
        let mut front = Frontmatter::new_note("親");
        front.origin = Some("agent".into());
        vault
            .write_note_fixture(
                "notes/親",
                &Note {
                    front,
                    body: "まだ無い [子ノート](/notes/子ノート.md) を参照。".into(),
                },
            )
            .unwrap();
        let conn = open_db(&vault).unwrap();
        let report = crate::index::import_markdown_snapshot(&vault, &conn).unwrap();
        assert!(report.degraded.is_empty());
        // broken(リンク切れ)+ untagged(タグ無し=契約1違反状態)の2件
        let added = detect(&conn, &vault).unwrap();
        assert_eq!(added, 2);
        let open = list_open(&conn).unwrap();
        let kinds: Vec<&str> = open.iter().map(|p| p.kind.as_str()).collect();
        assert!(
            kinds.contains(&"broken") && kinds.contains(&"untagged"),
            "{kinds:?}"
        );
        // 再検知しても増えない
        assert_eq!(detect(&conn, &vault).unwrap(), 0);
        // 「このまま」→ 消えて、二度と出ない
        for p in &open {
            dismiss(&conn, &p.key).unwrap();
        }
        assert!(list_open(&conn).unwrap().is_empty());
        assert_eq!(detect(&conn, &vault).unwrap(), 0);
    }

    #[test]
    fn all_existing_tag_contract_violations_are_visible() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let glossary_id = vault
            .propose_for_test(
                "タグ運用 — 合意の置き場",
                "## 語彙\n\n| タグ | 説明 |\n|---|---|\n| kb-app | 主題 |\n| knowledge-base | 主題 |\n| governance | 活動 |\n| ops | 活動 |\n",
                None,
                &["kb-app".into()],
                "test/client",
            )
            .unwrap();

        for (id, title, tags) in [
            (
                "notes/多すぎる",
                "多すぎる",
                vec!["kb-app", "knowledge-base", "governance", "ops", "extra"],
            ),
            ("notes/形が不正", "形が不正", vec!["日本語"]),
            ("notes/語彙外", "語彙外", vec!["stray"]),
        ] {
            let mut front = Frontmatter::new_note(title);
            front.origin = Some("agent".into());
            front.tags = tags.into_iter().map(String::from).collect();
            vault
                .write_note_fixture(
                    id,
                    &Note {
                        front,
                        body: "本文".into(),
                    },
                )
                .unwrap();
        }

        let conn = open_db(&vault).unwrap();
        let report = crate::index::import_markdown_snapshot(&vault, &conn).unwrap();
        assert!(report.degraded.is_empty());
        crate::tag_vocabulary_source::pin_for_test(&vault, &conn, &glossary_id).unwrap();
        assert_eq!(detect(&conn, &vault).unwrap(), 3);
        let open = list_open(&conn).unwrap();
        assert_eq!(
            open.iter()
                .filter(|proposal| proposal.kind == "invalid-tags")
                .count(),
            3
        );
        assert!(open.iter().any(|proposal| proposal.detail.contains("5 個")));
        assert!(
            open.iter()
                .any(|proposal| proposal.detail.contains("日本語"))
        );
        assert!(
            open.iter()
                .any(|proposal| proposal.detail.contains("stray"))
        );
    }

    /// 2026-09-08: 正本未指定を全ノートの語彙違反へ誤変換せず、修復対象を1件だけ示す。
    #[test]
    fn missing_source_is_one_care_item_without_false_tag_violations() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        vault
            .propose_for_test(
                "既存ノート",
                "本文",
                None,
                &["kb-app".into()],
                "test/client",
            )
            .unwrap();
        let glossary_id = vault
            .propose_for_test(
                "タグ運用",
                "## 語彙\n| kb-app | 主題 |\n",
                None,
                &["kb-app".into()],
                "test/client",
            )
            .unwrap();
        let conn = open_db(&vault).unwrap();
        assert_eq!(detect(&conn, &vault).unwrap(), 1);
        let open = list_open(&conn).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].kind, "glossary");
        assert!(open[0].detail.contains("未指定"));
        assert_eq!(detect(&conn, &vault).unwrap(), 0);
        crate::tag_vocabulary_source::pin_for_test(&vault, &conn, &glossary_id).unwrap();
        assert_eq!(detect(&conn, &vault).unwrap(), 0);
        assert!(list_open(&conn).unwrap().is_empty());
    }

    /// 2026-09-08: 同じ正本の問題が再発したら、修復済みの案内だけを再開し黙らない。
    #[test]
    fn repaired_source_problem_is_shown_again_when_it_recurs() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let id = vault
            .propose_for_test(
                "タグ運用",
                "## 語彙\n| kb-app | 主題 |\n",
                None,
                &["kb-app".into()],
                "test/client",
            )
            .unwrap();
        let conn = open_db(&vault).unwrap();
        crate::tag_vocabulary_source::pin_for_test(&vault, &conn, &id).unwrap();
        for (status, expected) in [("deprecated", 1), ("stable", 0), ("deprecated", 1)] {
            let mut note = crate::note_store::read(&conn, &id).unwrap();
            note.front.status = Some(status.into());
            conn.execute(
                "UPDATE notes SET document=?1, status=?2 WHERE id=?3",
                rusqlite::params![note.to_file_string().unwrap(), status, id],
            )
            .unwrap();
            assert_eq!(detect(&conn, &vault).unwrap(), expected);
            assert_eq!(list_open(&conn).unwrap().len(), expected);
        }
        let open = list_open(&conn).unwrap();
        dismiss(&conn, &open[0].key).unwrap();
        assert_eq!(detect(&conn, &vault).unwrap(), 0);
        assert!(list_open(&conn).unwrap().is_empty());
    }
}
