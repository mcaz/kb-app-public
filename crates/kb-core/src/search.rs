//! ハイブリッド検索(段0: FTS のみが正常形)。
//! 主経路 = lindera 分かち書き+bm25。レスキュー経路 = trigram / LIKE(部分語)。
//! 検索 API は Result で全滅させず「結果+劣化情報」を返す(fail-open を型で強制、原則4)。

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

use crate::degradation::Degradation;
use crate::tokenize::match_expr;

#[derive(Debug, Clone, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Hit {
    pub id: String,
    pub title: Option<String>,
    pub status: String,
    pub snippet: String,
    /// "main"(分かち書き bm25)/ "vec"(意味検索)/ "rescue"(trigram/LIKE)
    pub via: &'static str,
    /// 意味検索のコサイン距離(関連判定は RRF でなく生距離で — 旧 KB の実測教訓)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance: Option<f32>,
    /// 所有(原則9): "human" | "agent"
    pub origin: Option<String>,
    /// 分類タグ(空白区切りを分解済み)
    pub tags: Vec<String>,
    /// 作成日時・最終更新(一覧でも見えるように)
    pub created: Option<String>,
    pub updated: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SearchOutcome {
    pub hits: Vec<Hit>,
    /// 上位ヒットから1ホップのリンク先・被リンク(id, title)
    pub related: Vec<(String, Option<String>)>,
    /// 劣化情報(空 = 全経路正常)。UI/クライアントに必ず見せる
    pub degraded: Vec<Degradation>,
}

pub fn search(conn: &Connection, query: &str, limit: usize) -> SearchOutcome {
    search_mode(conn, query, limit, false)
}

/// `any = true` で語を OR 結合(フックの前出しなど、文まるごとを投げる用途。
/// bm25 が多く当たった文書を上位に出す)。false は従来どおり AND。
pub fn search_mode(conn: &Connection, query: &str, limit: usize, any: bool) -> SearchOutcome {
    let mut hits: Vec<Hit> = Vec::new();
    let mut degraded = Vec::new();

    // 主経路: lindera 分かち書き + bm25
    match main_search(conn, query, limit, any) {
        Ok(main_hits) => hits.extend(main_hits),
        Err(error) => degraded.push(Degradation::MainSearch {
            detail: error.to_string(),
        }),
    }

    // 意味検索(段1)。モデル未導入なら黙って全文のみ(段0 の正常形)。
    // 導入済みで失敗した場合は必ず劣化として見せる(沈黙停止の教訓)。
    match vec_search(conn, query, limit) {
        Ok(Some(vec_hits)) => hits = fuse(hits, vec_hits, limit),
        Ok(None) => {}
        Err(error) => degraded.push(Degradation::SemanticSearch {
            detail: error.to_string(),
        }),
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
        Err(error) => degraded.push(Degradation::RescueSearch {
            detail: error.to_string(),
        }),
    }

    let related = match related_of(conn, hits.first().map(|hit| hit.id.as_str())) {
        Ok(related) => related,
        Err(error) => {
            degraded.push(Degradation::RelatedNotes {
                detail: error.to_string(),
            });
            Vec::new()
        }
    };
    SearchOutcome {
        hits,
        related,
        degraded,
    }
}

fn main_search(conn: &Connection, query: &str, limit: usize, any: bool) -> Result<Vec<Hit>> {
    let expr = if any {
        crate::tokenize::match_expr_any(query)
    } else {
        match_expr(query)
    };
    if expr.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare_cached(
        "SELECT f.id, n.title, n.status,
                snippet(fts_main, 1, '[', ']', '…', 12), n.origin, n.tags, n.created, n.generated_at
         FROM fts_main f JOIN notes n ON n.id = f.id
         WHERE fts_main MATCH ?1 AND n.status != 'deprecated'
         ORDER BY rank LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![expr, limit as i64], |r| {
        Ok(Hit {
            id: r.get(0)?,
            title: r.get(1)?,
            status: r.get(2)?,
            snippet: r.get(3)?,
            via: "main",
            distance: None,
            origin: r.get(4)?,
            tags: split_tags(r.get::<_, Option<String>>(5)?),
            created: r.get(6)?,
            updated: r.get(7)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// 意味検索(埋め込み KNN)。モデル未導入なら Ok(None)。
/// 関連判定は生コサイン距離 ≤ RELATED_DISTANCE(RRF スコアでは判定しない)。
fn vec_search(conn: &Connection, query: &str, limit: usize) -> Result<Option<Vec<(Hit, f32)>>> {
    use crate::embed;
    if !embed::model_installed() {
        return Ok(None); // 段0 の正常形
    }
    let qv = embed::embed_text(query)?; // 失敗は呼び側で劣化表示
    let neighbors = embed::knn(conn, &qv, limit * 2)?;
    let mut out = Vec::new();
    let mut stmt = conn.prepare_cached(
        "SELECT title, status, coalesce(description, substr(body,1,80)), origin, tags, created, generated_at
         FROM notes WHERE id = ?1 AND status != 'deprecated'",
    )?;
    for (id, dist) in neighbors {
        if dist > embed::RELATED_DISTANCE {
            break; // 近い順なので以降は全て閾値外
        }
        let row = stmt.query_row([&id], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        });
        if let Ok((title, status, snippet, origin, tags, created, updated)) = row {
            out.push((
                Hit {
                    id,
                    title,
                    status,
                    snippet: snippet.replace('\n', " "),
                    via: "vec",
                    distance: Some(dist),
                    origin,
                    tags: split_tags(tags),
                    created,
                    updated,
                },
                dist,
            ));
        }
    }
    Ok(Some(out))
}

/// FTS(bm25 順)と意味検索(距離順)を RRF で融合。距離は Hit に残す。
fn fuse(fts: Vec<Hit>, vec_hits: Vec<(Hit, f32)>, limit: usize) -> Vec<Hit> {
    let rrf = |rank: usize| 1.0f32 / (60.0 + rank as f32);
    let mut score: std::collections::HashMap<String, f32> = std::collections::HashMap::new();
    let mut byid: std::collections::HashMap<String, Hit> = std::collections::HashMap::new();
    for (i, h) in fts.into_iter().enumerate() {
        *score.entry(h.id.clone()).or_default() += rrf(i);
        byid.insert(h.id.clone(), h);
    }
    for (j, (h, dist)) in vec_hits.into_iter().enumerate() {
        *score.entry(h.id.clone()).or_default() += rrf(j);
        byid.entry(h.id.clone())
            .and_modify(|e| {
                e.distance = Some(dist);
                e.via = "both";
            })
            .or_insert(h);
    }
    let mut ranked: Vec<(String, f32)> = score.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked
        .into_iter()
        .take(limit)
        .filter_map(|(id, _)| byid.remove(&id))
        .collect()
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
        "SELECT n.id, n.title, n.status, substr(n.body, 1, 80), n.origin, n.tags, n.created, n.generated_at FROM notes n
         WHERE n.status != 'deprecated' AND {} LIMIT {}",
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
            distance: None,
            origin: r.get(4)?,
            tags: split_tags(r.get::<_, Option<String>>(5)?),
            created: r.get(6)?,
            updated: r.get(7)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn split_tags(t: Option<String>) -> Vec<String> {
    t.unwrap_or_default()
        .split_whitespace()
        .map(String::from)
        .collect()
}

/// タグの使用数(deprecated 除く・多い順)。一覧のフィルタチップ用。
pub fn tag_counts(conn: &Connection, limit: usize) -> Result<Vec<(String, usize)>> {
    let mut stmt = conn.prepare_cached("SELECT tags FROM notes WHERE status != 'deprecated'")?;
    let rows = stmt.query_map([], |r| r.get::<_, Option<String>>(0))?;
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for row in rows {
        for t in split_tags(row?) {
            *counts.entry(t).or_default() += 1;
        }
    }
    let mut v: Vec<(String, usize)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.truncate(limit);
    Ok(v)
}

/// タグの一覧(使用数+説明)。**説明はアプリが持たず KB の「タグ運用」ノートから読む**
/// (タグの意味づけは AI とユーザーの会話で決まる — 2026-08-10 方針)。
/// 表(| タグ | 説明 |)と箇条書き(- タグ — 説明 / - タグ: 説明)の両方を拾う。
#[derive(Debug, Clone, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct TagInfo {
    pub tag: String,
    pub count: usize,
    pub description: Option<String>,
}

pub fn tag_overview(conn: &Connection) -> Result<(Vec<TagInfo>, Option<String>)> {
    // 読み取りは tags::glossary が正本(`## 語彙` 節の表だけ・形式検証つき)。
    // 以前はこの関数が本文全体を舐めており、普通の箇条書きや URL が偽タグとして
    // 一覧に出た(2026-08-12)。
    let glossary = crate::tags::glossary(conn)?;
    let (desc, note_id) = (glossary.entries, glossary.note_id);
    let counts = tag_counts(conn, 500)?;
    let mut out: Vec<TagInfo> = counts
        .into_iter()
        .map(|(tag, count)| {
            let description = desc.get(&tag).cloned();
            TagInfo {
                tag,
                count,
                description,
            }
        })
        .collect();
    // 合意済みだがまだ使われていないタグも見せる(語彙として存在するため)
    for (tag, d) in desc {
        if !out.iter().any(|t| t.tag == tag) {
            out.push(TagInfo {
                tag,
                count: 0,
                description: Some(d),
            });
        }
    }
    out.sort_by(|a, b| b.count.cmp(&a.count).then(a.tag.cmp(&b.tag)));
    Ok((out, note_id))
}

/// 指定ノートと意味が近いノート(自分自身・リンク済み・退役は除く)。
/// リンクされていない関連 = Obsidian の unlinked mentions に相当し、埋め込みならではの発見。
pub fn similar_notes(
    conn: &Connection,
    id: &str,
    limit: usize,
) -> Result<Vec<(String, Option<String>, f32)>> {
    use crate::embed;
    let blob: Option<Vec<u8>> = conn
        .query_row(
            "SELECT embedding FROM note_vecs WHERE id = ?1 AND stamp = ?2",
            rusqlite::params![id, embed::EMBED_STAMP],
            |r| r.get(0),
        )
        .optional()?;
    let Some(blob) = blob else {
        return Ok(Vec::new());
    }; // 未埋め込み・段0 は空
    let me = embed::from_blob(&blob);
    let linked: std::collections::HashSet<String> = {
        let mut stmt = conn.prepare_cached(
            "SELECT dst FROM links WHERE src = ?1 UNION SELECT src FROM links WHERE dst = ?1",
        )?;
        let rows = stmt.query_map([id], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let mut out = Vec::new();
    let mut stmt =
        conn.prepare_cached("SELECT title FROM notes WHERE id = ?1 AND status != 'deprecated'")?;
    for (nid, dist) in embed::knn(conn, &me, limit + linked.len() + 5)? {
        if out.len() >= limit {
            break;
        }
        if nid == id || linked.contains(&nid) || dist > embed::RELATED_DISTANCE {
            continue;
        }
        if let Ok(title) = stmt.query_row([&nid], |r| r.get::<_, Option<String>>(0)) {
            out.push((nid, title, dist));
        }
    }
    Ok(out)
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

/// サイドバーに出すディレクトリ。count は直下だけでなく子孫ノートを含む。
#[derive(Debug, Clone, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct NoteCategory {
    pub path: String,
    pub name: String,
    pub count: usize,
}

/// カテゴリ別一覧の1行。本文全体を画面へ運ばないための軽量表現。
#[derive(Debug, Clone, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct NoteSummary {
    pub id: String,
    pub title: Option<String>,
    pub description: String,
    pub tags: Vec<String>,
    pub created: Option<String>,
    pub updated: Option<String>,
    /// リンク先と被リンクを合わせた、現存するノートの件数。
    pub linked_count: usize,
    /// 現行の埋め込みがなければ None。あれば、未リンクの近いノートがあるか。
    pub has_similar: Option<bool>,
    /// 台帳ファイルと旧添付の合計。ファイルシステム由来なので呼び出し層で補う。
    pub file_count: usize,
}

/// ID順のcursor page。全ノートを一度に画面へ渡さない。
#[derive(Debug, Clone, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct NoteListPage {
    pub notes: Vec<NoteSummary>,
    pub total: usize,
    pub next_cursor: Option<String>,
    pub degraded: Vec<crate::degradation::Degradation>,
}

pub fn note_categories(conn: &Connection) -> Result<Vec<NoteCategory>> {
    let mut stmt =
        conn.prepare_cached("SELECT id FROM notes WHERE status != 'deprecated' ORDER BY id")?;
    let ids = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut counts = std::collections::BTreeMap::<String, usize>::new();

    for id in ids {
        let mut segments: Vec<&str> = id.split('/').filter(|part| !part.is_empty()).collect();
        segments.pop();
        if segments.is_empty() {
            *counts.entry(String::new()).or_default() += 1;
            continue;
        }
        for index in 0..segments.len() {
            let path = segments[..=index].join("/");
            *counts.entry(path).or_default() += 1;
        }
    }

    Ok(counts
        .into_iter()
        .map(|(path, count)| NoteCategory {
            name: path.rsplit('/').next().unwrap_or("").to_string(),
            path,
            count,
        })
        .collect())
}

pub fn notes_in_category(
    conn: &Connection,
    category: &str,
    after: Option<&str>,
    limit: usize,
) -> Result<NoteListPage> {
    let limit = limit.clamp(1, 100);
    let after = after.unwrap_or("");
    let (total, mut notes) = if category.is_empty() {
        let total = conn.query_row(
            "SELECT count(*) FROM notes WHERE status != 'deprecated' AND instr(id, '/') = 0",
            [],
            |r| r.get::<_, i64>(0),
        )? as usize;
        let mut stmt = conn.prepare_cached(
            "SELECT id, title, coalesce(description, substr(body,1,120)), tags, created, generated_at
             FROM notes
             WHERE status != 'deprecated' AND instr(id, '/') = 0 AND id > ?1
             ORDER BY id LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![after, (limit + 1) as i64], note_summary)?;
        (total, rows.collect::<std::result::Result<Vec<_>, _>>()?)
    } else {
        let total = conn.query_row(
            "SELECT count(*) FROM notes
             WHERE status != 'deprecated' AND substr(id, 1, length(?1) + 1) = ?1 || '/'",
            [category],
            |r| r.get::<_, i64>(0),
        )? as usize;
        let mut stmt = conn.prepare_cached(
            "SELECT id, title, coalesce(description, substr(body,1,120)), tags, created, generated_at
             FROM notes
             WHERE status != 'deprecated'
               AND substr(id, 1, length(?1) + 1) = ?1 || '/'
               AND id > ?2
             ORDER BY id LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![category, after, (limit + 1) as i64],
            note_summary,
        )?;
        (total, rows.collect::<std::result::Result<Vec<_>, _>>()?)
    };

    let has_more = notes.len() > limit;
    if has_more {
        notes.truncate(limit);
    }
    populate_note_relations(conn, &mut notes)?;
    let next_cursor = has_more
        .then(|| notes.last().map(|note| note.id.clone()))
        .flatten();
    Ok(NoteListPage {
        notes,
        total,
        next_cursor,
        degraded: Vec::new(),
    })
}

fn note_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<NoteSummary> {
    Ok(NoteSummary {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get::<_, String>(2)?.replace('\n', " "),
        tags: split_tags(row.get::<_, Option<String>>(3)?),
        created: row.get(4)?,
        updated: row.get(5)?,
        linked_count: 0,
        has_similar: None,
        file_count: 0,
    })
}

/// 一覧ページにだけ必要な関係メタデータを補う。
///
/// リンク件数は逆引き用 index を使う。近いノートは全ベクトルを一度だけ読み、
/// ページ内の各ノートについて最初の候補が見つかった時点で打ち切る。
fn populate_note_relations(conn: &Connection, notes: &mut [NoteSummary]) -> Result<()> {
    let mut count_links = conn.prepare_cached(
        "SELECT count(*) FROM (
             SELECT l.dst AS other
             FROM links l JOIN notes n ON n.id = l.dst AND n.status != 'deprecated'
             WHERE l.src = ?1
             UNION
             SELECT l.src AS other
             FROM links l JOIN notes n ON n.id = l.src AND n.status != 'deprecated'
             WHERE l.dst = ?1
         )",
    )?;
    for note in notes.iter_mut() {
        note.linked_count = count_links.query_row([&note.id], |row| row.get::<_, i64>(0))? as usize;
    }

    if notes.is_empty() {
        return Ok(());
    }

    use crate::embed;
    let vectors: std::collections::HashMap<String, Vec<f32>> = {
        let mut stmt = conn.prepare_cached(
            "SELECT v.id, v.embedding
             FROM note_vecs v JOIN notes n ON n.id = v.id
             WHERE v.stamp = ?1 AND n.status != 'deprecated'",
        )?;
        let rows = stmt.query_map([embed::EMBED_STAMP], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|(id, blob)| (id, embed::from_blob(&blob)))
            .collect()
    };
    if vectors.is_empty() {
        return Ok(());
    }

    let linked: std::collections::HashMap<String, std::collections::HashSet<String>> = {
        let mut stmt = conn.prepare_cached("SELECT src, dst FROM links")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut by_note =
            std::collections::HashMap::<String, std::collections::HashSet<String>>::new();
        for row in rows {
            let (src, dst): (String, String) = row?;
            by_note.entry(src.clone()).or_default().insert(dst.clone());
            by_note.entry(dst).or_default().insert(src);
        }
        by_note
    };

    for note in notes.iter_mut() {
        let Some(me) = vectors.get(&note.id) else {
            continue;
        };
        let note_links = linked.get(&note.id);
        note.has_similar = Some(vectors.iter().any(|(other_id, other)| {
            if other_id == &note.id
                || note_links.is_some_and(|ids| ids.contains(other_id))
                || other.len() != me.len()
            {
                return false;
            }
            let similarity: f32 = me.iter().zip(other).map(|(a, b)| a * b).sum();
            1.0 - similarity <= embed::RELATED_DISTANCE
        }));
    }
    Ok(())
}

/// 健全性の要約(FR-A2 ホーム表示用)。
#[derive(Debug, Clone, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Stats {
    pub total: usize,
    pub deprecated: usize,
    /// メモ(origin: human)/ AI ノート(origin: agent)の内訳(deprecated 除く)
    pub memos: usize,
    pub agent_notes: usize,
    /// つながり(リンク)の本数
    pub links: usize,
    /// かしこい検索(段1)が導入済みか
    pub embed_enabled: bool,
    /// 現行スタンプで埋め込み済みのノート数(欠損の可視化 — 沈黙停止の教訓)
    pub embedded: usize,
}

pub fn stats(conn: &Connection) -> Result<Stats> {
    let count = |sql: &str| -> Result<usize> {
        Ok(conn.query_row(sql, [], |r| r.get::<_, i64>(0))? as usize)
    };
    let embedded = conn
        .query_row(
            "SELECT count(*) FROM note_vecs WHERE stamp = ?1",
            [crate::embed::EMBED_STAMP],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0) as usize;
    Ok(Stats {
        total: count("SELECT count(*) FROM notes")?,
        deprecated: count("SELECT count(*) FROM notes WHERE status='deprecated'")?,
        memos: count(
            "SELECT count(*) FROM notes WHERE status != 'deprecated' AND coalesce(origin,'human') != 'agent'",
        )?,
        agent_notes: count(
            "SELECT count(*) FROM notes WHERE status != 'deprecated' AND origin = 'agent'",
        )?,
        links: count("SELECT count(*) FROM links")?,
        embed_enabled: crate::embed::model_installed(),
        embedded,
    })
}

/// 直近ノート(generated_at 降順、なければ mtime 降順)。
pub fn recent(conn: &Connection, limit: usize) -> Result<Vec<Hit>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, title, status, coalesce(description, substr(body,1,80)), origin, tags, created, generated_at
         FROM notes WHERE status != 'deprecated'
         ORDER BY coalesce(generated_at, created, '') DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |r| {
        Ok(Hit {
            id: r.get(0)?,
            title: r.get(1)?,
            status: r.get(2)?,
            snippet: r.get::<_, String>(3)?.replace('\n', " "),
            via: "recent",
            distance: None,
            origin: r.get(4)?,
            tags: split_tags(r.get::<_, Option<String>>(5)?),
            created: r.get(6)?,
            updated: r.get(7)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::frontmatter::{Frontmatter, Generated, Note};
    use crate::index::{open_db, sync};
    use crate::vault::Vault;

    const PERFORMANCE_NOTE_COUNT: usize = 10_000;
    const PERFORMANCE_CATEGORY_COUNT: usize = 100;
    const PERFORMANCE_VECTOR_DIM: usize = 1_024;
    const PERFORMANCE_TARGET: usize = 9_876;

    fn setup() -> (tempfile::TempDir, Vault, rusqlite::Connection) {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        vault
            .propose_for_test(
                "認証設計メモ",
                "認証フローの見直しを行った。監査ログも整備する。",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        vault
            .propose_for_test(
                "運用ノート",
                "本番環境の運用手順とバックアップのライフサイクルを記録。",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
        vault
            .propose_for_test(
                "無関係",
                "昨日の打ち合わせ内容を整理する。",
                None,
                &["test".into()],
                "test/client",
            )
            .unwrap();
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

    /// 2026-08-16までは関連取得のDB失敗が空配列になり、本当の0件と区別できなかった。
    #[test]
    fn related_failure_keeps_search_hits_and_adds_a_typed_degradation() {
        let (_d, _v, conn) = setup();
        conn.execute_batch("DROP TABLE links").unwrap();

        let out = super::search(&conn, "認証", 10);
        assert_eq!(out.hits.len(), 1);
        assert!(out.related.is_empty());
        assert!(
            out.degraded
                .iter()
                .any(|item| matches!(item, crate::degradation::Degradation::RelatedNotes { .. }))
        );
    }

    /// 未埋め込みは正常な空だが、埋め込み表そのものの故障は部分失敗として返す。
    #[test]
    fn similar_notes_distinguishes_missing_data_from_a_broken_index() {
        let (_d, _v, conn) = setup();
        assert!(
            super::similar_notes(&conn, "notes/認証設計メモ", 5)
                .unwrap()
                .is_empty()
        );
        conn.execute_batch("DROP TABLE note_vecs").unwrap();
        assert!(super::similar_notes(&conn, "notes/認証設計メモ", 5).is_err());
    }

    #[test]
    fn recent_returns_all() {
        let (_d, _v, conn) = setup();
        assert_eq!(super::recent(&conn, 10).unwrap().len(), 3);
    }

    #[test]
    fn categories_count_descendant_notes_and_skip_deprecated() {
        let (_d, _v, conn) = setup();
        for (id, status) in [
            ("research/ai/検索", "stable"),
            ("research/概要", "stable"),
            ("入口", "stable"),
            ("research/旧版", "deprecated"),
        ] {
            conn.execute(
                "INSERT INTO notes(id,title,status,body,tags) VALUES (?1,?1,?2,'','')",
                rusqlite::params![id, status],
            )
            .unwrap();
        }
        let categories = super::note_categories(&conn).unwrap();
        let counts: std::collections::BTreeMap<_, _> = categories
            .into_iter()
            .map(|category| (category.path, category.count))
            .collect();
        assert_eq!(counts.get("research"), Some(&2));
        assert_eq!(counts.get("research/ai"), Some(&1));
        assert_eq!(counts.get(""), Some(&1));
    }

    #[test]
    fn category_list_is_cursor_paginated() {
        let (_d, _v, conn) = setup();
        for id in ["research/ai/検索", "research/概要"] {
            conn.execute(
                "INSERT INTO notes(id,title,status,body,tags) VALUES (?1,?1,'stable','','')",
                [id],
            )
            .unwrap();
        }
        let first = super::notes_in_category(&conn, "research", None, 1).unwrap();
        assert_eq!(first.total, 2);
        assert_eq!(first.notes.len(), 1);
        let second =
            super::notes_in_category(&conn, "research", first.next_cursor.as_deref(), 1).unwrap();
        assert_eq!(second.notes.len(), 1);
        assert!(second.next_cursor.is_none());
        assert_ne!(first.notes[0].id, second.notes[0].id);
    }

    #[test]
    fn category_list_includes_link_and_similar_presence() {
        let (_d, _v, conn) = setup();
        for id in ["signals/a", "signals/b", "signals/c", "signals/d"] {
            conn.execute(
                "INSERT INTO notes(id,title,status,body,tags) VALUES (?1,?1,'stable','','')",
                [id],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO links(src,dst) VALUES ('signals/a','signals/b')",
            [],
        )
        .unwrap();
        for (id, vector) in [
            ("signals/a", vec![1.0, 0.0]),
            ("signals/c", vec![1.0, 0.0]),
            ("signals/d", vec![0.0, 1.0]),
        ] {
            conn.execute(
                "INSERT INTO note_vecs(id,stamp,embedding) VALUES (?1,?2,?3)",
                rusqlite::params![
                    id,
                    crate::embed::EMBED_STAMP,
                    crate::embed::to_blob(&vector)
                ],
            )
            .unwrap();
        }

        let page = super::notes_in_category(&conn, "signals", None, 10).unwrap();
        let by_id: std::collections::HashMap<_, _> = page
            .notes
            .into_iter()
            .map(|note| (note.id.clone(), note))
            .collect();
        assert_eq!(by_id["signals/a"].linked_count, 1);
        assert_eq!(by_id["signals/a"].has_similar, Some(true));
        assert_eq!(by_id["signals/b"].has_similar, None);
        assert_eq!(by_id["signals/d"].has_similar, Some(false));
    }

    /// 10k規模の目標が文章だけだったため、2026-08-16から専用release CIで退行を止める。
    #[test]
    #[ignore = "release buildの専用CIで10k fixtureを測る"]
    fn ten_thousand_note_performance_gate() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::create(dir.path().join("v")).unwrap();
        write_performance_fixture(&vault);
        let (conn, rebuild) = timed(|| open_db(&vault).unwrap());
        assert_eq!(super::stats(&conn).unwrap().total, PERFORMANCE_NOTE_COUNT);
        seed_performance_vectors(&conn);

        let (categories, category_list) =
            timed(|| repeat_last(20, || super::note_categories(&conn).unwrap()));
        assert_eq!(categories.len(), PERFORMANCE_CATEGORY_COUNT + 1);

        let (page, note_list) = timed(|| {
            repeat_last(5, || {
                super::notes_in_category(&conn, "notes/topic-042", None, 50).unwrap()
            })
        });
        assert_eq!(
            page.total,
            PERFORMANCE_NOTE_COUNT / PERFORMANCE_CATEGORY_COUNT
        );
        assert_eq!(page.notes.len(), 50);
        assert!(page.next_cursor.is_some());

        let target_id = performance_note_id(PERFORMANCE_TARGET);
        let ((main, rescue), keyword_search) = timed(|| {
            repeat_last(100, || {
                (
                    super::main_search(&conn, "検索番兵オーロラ", 20, false).unwrap(),
                    super::rescue_search(&conn, "番兵オーロラ", 20).unwrap(),
                )
            })
        });
        assert!(main.iter().any(|hit| hit.id == target_id));
        assert!(rescue.iter().any(|hit| hit.id == target_id));

        let mut query = vec![0.0f32; PERFORMANCE_VECTOR_DIM];
        query[PERFORMANCE_TARGET % PERFORMANCE_VECTOR_DIM] = 1.0;
        let (neighbors, semantic_search) =
            timed(|| repeat_last(10, || crate::embed::knn(&conn, &query, 20).unwrap()));
        assert_eq!(neighbors.len(), 20);
        assert!(neighbors.iter().any(|(id, _)| id == &target_id));

        let (_, note_detail) = timed(|| {
            repeat_last(5, || {
                let note = crate::note_store::read(&conn, &target_id).unwrap();
                let related = super::related_of(&conn, Some(&target_id)).unwrap();
                let similar = super::similar_notes(&conn, &target_id, 6).unwrap();
                assert!(note.body.contains("検索番兵オーロラ"));
                assert!(!related.is_empty());
                assert_eq!(similar.len(), 6);
            })
        });

        let measurements = [
            ("index_rebuild", rebuild, Duration::from_secs(30)),
            (
                "category_list_x20",
                category_list,
                Duration::from_millis(500),
            ),
            ("note_list_x5", note_list, Duration::from_secs(2)),
            (
                "keyword_search_x100",
                keyword_search,
                Duration::from_millis(500),
            ),
            (
                "semantic_search_x10",
                semantic_search,
                Duration::from_secs(1),
            ),
            ("note_detail_x5", note_detail, Duration::from_secs(2)),
        ];
        for (name, elapsed, budget) in measurements {
            eprintln!(
                "performance_gate {name}: {} ms (budget {} ms)",
                elapsed.as_millis(),
                budget.as_millis()
            );
            assert!(
                elapsed <= budget,
                "{name} took {} ms; budget is {} ms",
                elapsed.as_millis(),
                budget.as_millis()
            );
        }
    }

    fn timed<T>(operation: impl FnOnce() -> T) -> (T, Duration) {
        let started = Instant::now();
        let value = operation();
        (value, started.elapsed())
    }

    fn repeat_last<T>(times: usize, mut operation: impl FnMut() -> T) -> T {
        let mut last = None;
        for _ in 0..times {
            last = Some(operation());
        }
        last.expect("性能fixtureは最低1回実行する")
    }

    fn performance_note_id(index: usize) -> String {
        format!(
            "notes/topic-{:03}/note-{index:05}",
            index % PERFORMANCE_CATEGORY_COUNT
        )
    }

    fn write_performance_fixture(vault: &Vault) {
        for index in 0..PERFORMANCE_NOTE_COUNT {
            let id = performance_note_id(index);
            let previous_link = if index > 0 {
                format!("\n\n[前のノート](/{}.md)", performance_note_id(index - 1))
            } else {
                String::new()
            };
            let marker = if index == PERFORMANCE_TARGET {
                "検索番兵オーロラ"
            } else {
                "標準知識"
            };
            let mut front = Frontmatter::new_note(&format!("性能fixture {index:05}"));
            front.description = Some(format!("10k回帰測定 category {}", index % 100));
            front.tags = vec!["performance".into(), format!("group-{}", index % 10)];
            front.origin = Some("agent".into());
            front.created = Some("2026-08-16T00:00:00Z".into());
            front.generated = Some(Generated {
                by: "test/performance-gate".into(),
                at: "2026-08-16T00:00:00Z".into(),
            });
            let note = Note {
                front,
                body: format!(
                    "{marker}。合成ナレッジ {index:05} の本文。性能回帰と検索品質を検査する。{previous_link}"
                ),
            };
            vault.write_note_fixture(&id, &note).unwrap();
        }
    }

    fn seed_performance_vectors(conn: &rusqlite::Connection) {
        let blobs: Vec<Vec<u8>> = (0..PERFORMANCE_VECTOR_DIM)
            .map(|axis| {
                let mut vector = vec![0.0f32; PERFORMANCE_VECTOR_DIM];
                vector[axis] = 1.0;
                crate::embed::to_blob(&vector)
            })
            .collect();
        let transaction = conn.unchecked_transaction().unwrap();
        {
            let mut insert = transaction
                .prepare("INSERT INTO note_vecs(id,stamp,embedding) VALUES (?1,?2,?3)")
                .unwrap();
            for index in 0..PERFORMANCE_NOTE_COUNT {
                insert
                    .execute(rusqlite::params![
                        performance_note_id(index),
                        crate::embed::EMBED_STAMP,
                        &blobs[index % PERFORMANCE_VECTOR_DIM]
                    ])
                    .unwrap();
            }
        }
        transaction.commit().unwrap();
    }
}
