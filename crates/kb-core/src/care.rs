//! FR-C7 お手入れ(最小形・ローカル完結)。ゼロベース設計 — 旧 KB のルーチン群の
//! 移植ではない(2026-08-10 本人方針)。
//!
//! 検知 → 平易な提案に変換 → 受信箱で「はい / いいえ」。ユーザーは維持作業を計画しない
//! (原則8)。実行は常に承諾経由で、メモ(origin: human)への操作は「つなげる」まで(原則9)。
//! v1 の検知: ①意味的な近接(埋め込み距離)②リンク切れ。鮮度・統合提案は段2(AI)で強化。

use anyhow::Result;
use rusqlite::Connection;

use crate::embed;
use crate::vault::Vault;

/// 「同じ話題に見える」判定のコサイン距離閾値(ノート全文同士)。
/// PoC 実測の関連帯(0.17〜0.43)と無関係帯(0.69〜)の間に置く。
const DUP_DISTANCE: f32 = 0.35;
/// 未処理の提案の総数上限(通知疲れの抑制)。処理されて枠が空いたら次を補充する。
const MAX_OPEN_PROPOSALS: usize = 5;

#[derive(Debug, Clone, serde::Serialize)]
pub struct CareProposal {
    pub key: String,
    pub kind: String, // "connect"(つなげる提案)| "broken"(リンク切れの気づき)
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

    // ① 意味的な近接(埋め込みがある場合のみ — 段0 では黙ってスキップ)
    if embed::model_installed() {
        let notes: Vec<(String, String, Vec<f32>)> = {
            let mut stmt = conn.prepare_cached(
                "SELECT n.id, coalesce(n.title, n.id), v.embedding
                 FROM notes n JOIN note_vecs v ON v.id = n.id AND v.stamp = ?1
                 WHERE n.status != 'deprecated'",
            )?;
            let rows = stmt.query_map([embed::EMBED_STAMP], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Vec<u8>>(2)?))
            })?;
            rows.filter_map(|r| r.ok())
                .map(|(id, t, b)| (id, t, embed::from_blob(&b)))
                .collect()
        };
        'outer: for i in 0..notes.len() {
            for j in (i + 1)..notes.len() {
                if added >= budget {
                    break 'outer;
                }
                let (ida, ta, va) = &notes[i];
                let (idb, tb, vb) = &notes[j];
                if va.len() != vb.len() {
                    continue;
                }
                let sim: f32 = va.iter().zip(vb.iter()).map(|(x, y)| x * y).sum();
                let dist = 1.0 - sim;
                if dist > DUP_DISTANCE {
                    continue;
                }
                // 既にリンク済みのペアは提案しない
                let linked: bool = conn.query_row(
                    "SELECT count(*) FROM links WHERE (src=?1 AND dst=?2) OR (src=?2 AND dst=?1)",
                    [ida, idb],
                    |r| Ok(r.get::<_, i64>(0)? > 0),
                )?;
                if linked {
                    continue;
                }
                let (x, y) = if ida < idb { (ida, idb) } else { (idb, ida) };
                let key = format!("connect:{x}:{y}");
                let detail = format!("「{ta}」と「{tb}」が同じ話題に見えます(近さ {dist:.2})。");
                if insert_new(conn, &key, "connect", x, y, &detail)? {
                    added += 1;
                }
            }
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
            .query_row("SELECT coalesce(title, id) FROM notes WHERE id=?1", [&src], |r| r.get(0))
            .unwrap_or_else(|_| src.clone());
        let detail = format!("「{title}」の中のリンク先「{dst}」がまだありません(未執筆の知識かも)。");
        if insert_new(conn, &key, "broken", &src, &dst, &detail)? {
            added += 1;
        }
    }
    Ok(added)
}

fn insert_new(conn: &Connection, key: &str, kind: &str, a: &str, b: &str, detail: &str) -> Result<bool> {
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
        Ok(CareProposal { key: r.get(0)?, kind: r.get(1)?, a: r.get(2)?, b: r.get(3)?, detail: r.get(4)? })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// 「いいえ(このまま)」— 同じ提案は二度と出ない。
pub fn dismiss(conn: &Connection, key: &str) -> Result<()> {
    conn.execute("UPDATE care_proposals SET status='dismissed' WHERE key=?1", [key])?;
    Ok(())
}

/// 「つなげる」の承諾。片方の本文末尾に関連リンクを1行足す。
/// メモ(human)より AI ノート(agent)側を優先して書き足し、メモの本文改変を最小にする。
/// (メモ側へ書く場合も、承諾済みの「つなげる」は原則9 の許容範囲)
pub fn accept_connect(conn: &Connection, vault: &Vault, key: &str) -> Result<()> {
    let (a, b): (String, String) = conn.query_row(
        "SELECT a, b FROM care_proposals WHERE key=?1 AND kind='connect' AND status='open'",
        [key],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let note_a = vault.read_note(&a)?;
    let note_b = vault.read_note(&b)?;
    // 書き足す側: agent を優先、両方 human / 両方 agent なら a
    let (host, host_note, target, target_note) =
        if note_a.front.origin.as_deref() == Some("agent") || note_b.front.origin.as_deref() != Some("agent") {
            (&a, note_a, &b, &note_b)
        } else {
            (&b, note_b, &a, &note_a)
        };
    let ttitle = target_note.front.title.clone().unwrap_or_else(|| target.to_string());
    let mut updated = host_note;
    updated.body = format!(
        "{}\n\n関連: [{}](/{}.md)\n",
        updated.body.trim_end(),
        ttitle,
        target
    );
    vault.write_note(host, &updated)?;
    vault.append_log(&format!("**Care**: /{host}.md と /{target}.md をつなげた(承諾)。"))?;
    vault.commit_care(host, &format!("care: connect {host} <-> {target}"))?;
    conn.execute("DELETE FROM care_proposals WHERE key=?1", [key])?;
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
            .new_human_note("親", "まだ無い [子ノート](/notes/子ノート.md) を参照。", "human:o")
            .unwrap();
        let conn = open_db(&vault).unwrap();
        sync(&vault, &conn).unwrap();
        let added = detect(&conn, &vault).unwrap();
        assert_eq!(added, 1);
        let open = list_open(&conn).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].kind, "broken");
        // 再検知しても増えない
        assert_eq!(detect(&conn, &vault).unwrap(), 0);
        // 「このまま」→ 消えて、二度と出ない
        dismiss(&conn, &open[0].key).unwrap();
        assert!(list_open(&conn).unwrap().is_empty());
        assert_eq!(detect(&conn, &vault).unwrap(), 0);
    }

    #[test]
    fn connect_acceptance_appends_link() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        let a = vault.new_human_note("読書メモ 昆虫", "昆虫の本のメモ。", "human:o").unwrap();
        let b = vault.propose("昆虫の本まとめ", "AI がまとめた昆虫の本。", None, &[], "c/x").unwrap();
        let conn = open_db(&vault).unwrap();
        sync(&vault, &conn).unwrap();
        init_schema(&conn).unwrap();
        insert_new(&conn, &format!("connect:{a}:{b}"), "connect", &a, &b, "test").unwrap();
        accept_connect(&conn, &vault, &format!("connect:{a}:{b}")).unwrap();
        // agent 側(b)に追記され、human 側(a)は不可侵のまま
        assert!(vault.read_note(&b).unwrap().body.contains("関連: ["));
        assert!(!vault.read_note(&a).unwrap().body.contains("関連: ["));
        sync(&vault, &conn).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM links WHERE src=?1", [&b], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
}
