//! ハイブリッド検索(段0: FTS のみが正常形)。
//! 主経路 = lindera 分かち書き+bm25。レスキュー経路 = trigram / LIKE(部分語)。
//! 検索 API は Result で全滅させず「結果+劣化情報」を返す(fail-open を型で強制、原則4)。

use anyhow::Result;
use rusqlite::Connection;

use crate::tokenize::match_expr;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Hit {
    pub id: String,
    pub title: Option<String>,
    pub status: String,
    pub snippet: String,
    /// "main"(分かち書き bm25)か "rescue"(trigram/LIKE)か
    pub via: &'static str,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchOutcome {
    pub hits: Vec<Hit>,
    /// 上位ヒットから1ホップのリンク先・被リンク(id, title)
    pub related: Vec<(String, Option<String>)>,
    /// 劣化情報(空 = 全経路正常)。UI/クライアントに必ず見せる
    pub degraded: Vec<String>,
}

pub fn search(conn: &Connection, query: &str, limit: usize) -> SearchOutcome {
    let mut hits: Vec<Hit> = Vec::new();
    let mut degraded = Vec::new();

    // 主経路: lindera 分かち書き + bm25
    match main_search(conn, query, limit) {
        Ok(main_hits) => hits.extend(main_hits),
        Err(e) => degraded.push(format!("主索引が利用できない: {e}")),
    }

    // レスキュー経路: 主経路で拾えない部分語・未知語形(常に実行し、差分だけ足す)
    match rescue_search(conn, query, limit) {
        Ok(rescue_hits) => {
            for h in rescue_hits {
                if hits.len() >= limit {
                    break;
                }
                if !hits.iter().any(|x| x.id == h.id) {
                    hits.push(h);
                }
            }
        }
        Err(e) => degraded.push(format!("レスキュー索引が利用できない: {e}")),
    }

    let related = related_of(conn, hits.first().map(|h| h.id.as_str())).unwrap_or_default();
    SearchOutcome { hits, related, degraded }
}

fn main_search(conn: &Connection, query: &str, limit: usize) -> Result<Vec<Hit>> {
    let expr = match_expr(query);
    if expr.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare_cached(
        "SELECT f.id, n.title, n.status,
                snippet(fts_main, 1, '[', ']', '…', 12)
         FROM fts_main f JOIN notes n ON n.id = f.id
         WHERE fts_main MATCH ?1
         ORDER BY rank LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![expr, limit as i64], |r| {
        Ok(Hit {
            id: r.get(0)?,
            title: r.get(1)?,
            status: r.get(2)?,
            snippet: r.get(3)?,
            via: "main",
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// trigram MATCH(3文字以上の語)+ LIKE(2文字以下の語)の AND。
/// trigram 単独は2字語が黙って落ちるため単独では使わない(PoC ② の実測)。
fn rescue_search(conn: &Connection, query: &str, limit: usize) -> Result<Vec<Hit>> {
    let terms: Vec<&str> = query.split_whitespace().collect();
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let mut conds = Vec::new();
    let mut params: Vec<String> = Vec::new();
    for t in &terms {
        if t.chars().count() >= 3 {
            params.push(format!("\"{}\"", t.replace('"', "")));
            conds.push(format!(
                "n.id IN (SELECT id FROM fts_tri WHERE fts_tri MATCH ?{})",
                params.len()
            ));
        } else {
            params.push(format!("%{t}%"));
            conds.push(format!(
                "n.id IN (SELECT id FROM fts_tri WHERE text LIKE ?{})",
                params.len()
            ));
        }
    }
    let sql = format!(
        "SELECT n.id, n.title, n.status, substr(n.body, 1, 80) FROM notes n WHERE {} LIMIT {}",
        conds.join(" AND "),
        limit
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
        Ok(Hit {
            id: r.get(0)?,
            title: r.get(1)?,
            status: r.get(2)?,
            snippet: r.get::<_, String>(3)?.replace('\n', " "),
            via: "rescue",
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// 指定ノートの「つながり」(リンク先+被リンク、最大5件)。
pub fn related_of(conn: &Connection, id: Option<&str>) -> Result<Vec<(String, Option<String>)>> {
    let Some(id) = id else { return Ok(Vec::new()) };
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT other, (SELECT title FROM notes WHERE id = other) FROM (
             SELECT dst AS other FROM links WHERE src = ?1
             UNION SELECT src AS other FROM links WHERE dst = ?1
         ) LIMIT 5",
    )?;
    let rows = stmt.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// 健全性の要約(FR-A2 ホーム表示用)。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Stats {
    pub total: usize,
    pub drafts: usize,
    pub deprecated: usize,
}

pub fn stats(conn: &Connection) -> Result<Stats> {
    let count = |sql: &str| -> Result<usize> {
        Ok(conn.query_row(sql, [], |r| r.get::<_, i64>(0))? as usize)
    };
    Ok(Stats {
        total: count("SELECT count(*) FROM notes")?,
        drafts: count("SELECT count(*) FROM notes WHERE status='draft'")?,
        deprecated: count("SELECT count(*) FROM notes WHERE status='deprecated'")?,
    })
}

/// 直近ノート(generated_at 降順、なければ mtime 降順)。
pub fn recent(conn: &Connection, limit: usize) -> Result<Vec<Hit>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, title, status, coalesce(description, substr(body,1,80))
         FROM notes ORDER BY coalesce(generated_at, datetime(mtime,'unixepoch')) DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |r| {
        Ok(Hit {
            id: r.get(0)?,
            title: r.get(1)?,
            status: r.get(2)?,
            snippet: r.get::<_, String>(3)?.replace('\n', " "),
            via: "recent",
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

#[cfg(test)]
mod tests {
    use crate::index::{open_db, sync};
    use crate::vault::Vault;

    fn setup() -> (tempfile::TempDir, Vault, rusqlite::Connection) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        vault.new_human_note("認証設計メモ", "認証フローの見直しを行った。監査ログも整備する。", "human:o").unwrap();
        vault.new_human_note("運用ノート", "本番環境の運用手順とバックアップのライフサイクルを記録。", "human:o").unwrap();
        vault.new_human_note("無関係", "昨日の打ち合わせ内容を整理する。", "human:o").unwrap();
        let conn = open_db(&vault).unwrap();
        sync(&vault, &conn).unwrap();
        (dir, vault, conn)
    }

    #[test]
    fn two_char_word_hits_via_main() {
        let (_d, _v, conn) = setup();
        let out = super::search(&conn, "認証", 10);
        assert!(out.degraded.is_empty(), "{:?}", out.degraded);
        assert_eq!(out.hits.len(), 1);
        assert_eq!(out.hits[0].via, "main");
    }

    #[test]
    fn keyword_enumeration_hits() {
        let (_d, _v, conn) = setup();
        let out = super::search(&conn, "認証 監査", 10);
        assert_eq!(out.hits.len(), 1, "{:?}", out.hits);
    }

    #[test]
    fn partial_word_hits_via_rescue() {
        let (_d, _v, conn) = setup();
        // 「サイクル」は「ライフサイクル」の内部 — 形態素側は構造的に0件、レスキューで拾う
        let out = super::search(&conn, "サイクル", 10);
        assert_eq!(out.hits.len(), 1, "{:?}", out.hits);
        assert_eq!(out.hits[0].via, "rescue");
    }

    #[test]
    fn recent_returns_all() {
        let (_d, _v, conn) = setup();
        assert_eq!(super::recent(&conn, 10).unwrap().len(), 3);
    }
}
