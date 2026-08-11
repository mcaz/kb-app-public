//! FR-C7 お手入れ(最小形・ローカル完結)。ゼロベース設計 — 旧 KB のルーチン群の
//! 移植ではない(2026-08-10 本人方針)。
//!
//! 検知 → 平易な提案に変換 → 受信箱で「はい / いいえ」。ユーザーは維持作業を計画しない
//! (原則8)。実行は常に承諾経由で、メモ(origin: human)への操作は「つなげる」まで(原則9)。
//! v1 の検知: ①リンク切れ ②タグ無し(契約違反状態)。
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
    pub kind: String, // "broken"(リンク切れ)| "untagged"(タグ無し=契約違反状態)
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
pub fn detect(conn: &Connection, _vault: &Vault) -> Result<usize> {
    init_schema(conn)?;
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

    // ②' タグ無し(契約1違反状態の可視化 — 修復は Claude への依頼で)
    let untagged: Vec<(String, String)> = {
        let mut stmt = conn.prepare_cached(
            "SELECT id, coalesce(title, id) FROM notes
             WHERE status != 'deprecated' AND (tags IS NULL OR tags = '')",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.filter_map(|r| r.ok()).collect()
    };
    for (id, title) in untagged {
        if added >= budget {
            break;
        }
        let key = format!("untagged:{id}");
        let detail =
            format!("「{title}」にタグがありません(契約: 1〜4個)。Claude に整理を頼めます。");
        if insert_new(conn, &key, "untagged", &id, "", &detail)? {
            added += 1;
        }
    }

    // ③ 語彙表に読めない行(沈黙しない — fail-open を選んだ経路は劣化を可視化する)。
    // 語彙表の行を黙って捨てると、合意したはずのタグが一覧から消えたまま気づけない。
    if added < budget
        && let Ok(g) = crate::tags::glossary(conn)
        && !g.skipped.is_empty()
        && let Some(note) = g.note_id.clone()
    {
        let key = format!("glossary:{note}:{}", g.skipped.len());
        let detail = format!(
            "「タグ運用」ノートの語彙表に、タグとして読めない行が {} 行あります({})。\
形は英小文字・数字・ハイフンです。",
            g.skipped.len(),
            g.skipped.join("、")
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
        rows.filter_map(|r| r.ok()).collect()
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
    use crate::index::{open_db, sync};

    /// 埋め込みを手挿入して検知経路をテスト(モデル不要にするため broken 経路中心)。
    #[test]
    fn broken_link_and_dismiss_flow() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        vault
            .new_human_note(
                "親",
                "まだ無い [子ノート](/notes/子ノート.md) を参照。",
                "human:o",
            )
            .unwrap();
        let conn = open_db(&vault).unwrap();
        sync(&vault, &conn).unwrap();
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
}
