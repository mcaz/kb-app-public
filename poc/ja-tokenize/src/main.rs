//! PoC: 日本語 FTS の「2文字語の空振り」欠陥を殺せる方式の確定
//!
//! 方式:
//!   A  : FTS5 tokenize='trigram' + MATCH        (既存実装相当)
//!   A' : 同じ trigram テーブル + LIKE '%語%'     (正しさのフォールバック)
//!   B  : lindera(IPADIC embedded)分かち書き + FTS5 unicode61 + MATCH
//!
//! 合成コーパス 5,000 件にターゲット語を既知の件数だけ埋め込み、
//! 再現率 / 誤ヒット / クエリレイテンシ中央値 / 構築時間 / DB サイズを測る。

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::time::Instant;

use lindera::dictionary::load_dictionary;
use lindera::mode::Mode;
use lindera::segmenter::Segmenter;
use rusqlite::Connection;

const NUM_DOCS: usize = 5000;
const WARMUP: usize = 5;
const RUNS: usize = 31;

struct Target {
    word: &'static str,
    class: &'static str,
    count: usize,
    sentence: &'static str,
}

/// ターゲット語(埋め込む文はその語を1回だけ含み、他のターゲット語を含まない)
const TARGETS: &[Target] = &[
    Target { word: "認証", class: "2字", count: 300, sentence: "認証フローの見直しを行った。" },
    Target { word: "設計", class: "2字", count: 280, sentence: "モジュール設計の方針を再検討した。" },
    Target { word: "運用", class: "2字", count: 260, sentence: "本番環境の運用手順を更新した。" },
    Target { word: "索引", class: "2字", count: 240, sentence: "検索用の索引を再構築した。" },
    Target { word: "統治", class: "2字", count: 60,  sentence: "データ統治のポリシーを定めた。" },
    Target { word: "埋め込み", class: "3字+", count: 220, sentence: "埋め込みベクトルの次元数を変更した。" },
    Target { word: "ライフサイクル", class: "3字+", count: 120, sentence: "リソースのライフサイクルを管理する仕組みを導入した。" },
    Target { word: "バックアップ", class: "3字+", count: 180, sentence: "バックアップの取得スケジュールを見直した。" },
    Target { word: "AI",  class: "英2字", count: 200, sentence: "AI エージェントの挙動を検証した。" },
    Target { word: "MCP", class: "英3字", count: 150, sentence: "MCP サーバーとの接続を確認した。" },
];

/// フィラー文(どのターゲット語も部分文字列として含まない。ASCII も含まない)
const FILLERS: &[&str] = &[
    "このノートは開発メモとして残す。",
    "昨日の打ち合わせで決まった内容を整理する。",
    "パフォーマンス計測の結果を記録した。",
    "次のリリースまでに検証を終える予定だ。",
    "エラーの再現手順をまとめておく。",
    "キャッシュの無効化タイミングを見直した。",
    "ログの出力形式を統一する必要がある。",
    "テストの実行時間が徐々に長くなってきた。",
    "依存クレートの更新は慎重に進める。",
    "古いスクリプトを整理して削除した。",
    "レビューで指摘された箇所を修正した。",
    "メモリ使用量の推移を観察している。",
    "デプロイ手順の自動化を検討中だ。",
    "障害の切り分けに時間がかかった。",
    "ドキュメントの構成を見直すことにした。",
    "リファクタリングの方針を共有した。",
    "クエリの応答時間を改善したい。",
    "検索結果の並び順を調整した。",
    "権限管理の仕様を確認した。",
    "監視ダッシュボードに新しい項目を追加した。",
    "型定義の重複を解消した。",
    "手元の環境でだけ再現する問題を調べている。",
    "リトライ処理の間隔を調整する。",
    "ビルド時間短縮のため依存を削減した。",
];

/// xorshift64 — 依存を増やさない決定的 RNG
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn gen_range(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn build_corpus() -> (Vec<String>, BTreeMap<&'static str, BTreeSet<i64>>) {
    let mut rng = Rng(0x243F_6A88_85A3_08D3);
    let mut docs: Vec<Vec<&str>> = (0..NUM_DOCS)
        .map(|_| {
            let n = 2 + rng.gen_range(4); // 2..=5 文
            (0..n).map(|_| FILLERS[rng.gen_range(FILLERS.len())]).collect()
        })
        .collect();

    let mut truth: BTreeMap<&'static str, BTreeSet<i64>> = BTreeMap::new();
    for t in TARGETS {
        let mut chosen: BTreeSet<usize> = BTreeSet::new();
        while chosen.len() < t.count {
            chosen.insert(rng.gen_range(NUM_DOCS));
        }
        for &i in &chosen {
            let pos = rng.gen_range(docs[i].len() + 1);
            docs[i].insert(pos, t.sentence);
        }
        truth.insert(t.word, chosen.iter().map(|&i| i as i64 + 1).collect());
    }

    let texts: Vec<String> = docs.iter().map(|s| s.concat()).collect();

    // 検証: 埋め込み計画と実際の部分文字列出現が完全一致すること
    // (フィラー文が偶然ターゲット語を含んでいたらここで落ちる)
    for t in TARGETS {
        let planned = &truth[t.word];
        let mut actual = BTreeSet::new();
        for (i, text) in texts.iter().enumerate() {
            let hit = if t.word.is_ascii() {
                text.to_ascii_uppercase().contains(t.word)
            } else {
                text.contains(t.word)
            };
            if hit {
                actual.insert(i as i64 + 1);
            }
        }
        assert_eq!(planned, &actual, "corpus verification failed for {}", t.word);
    }
    (texts, truth)
}

fn wakati(seg: &Segmenter, text: &str) -> String {
    seg.segment(Cow::Borrowed(text))
        .expect("lindera segment failed")
        .iter()
        .map(|tok| tok.surface.as_ref())
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// (構築時間ms, DBサイズbytes)
fn build_fts(path: &str, tokenize: &str, bodies: &[String]) -> (f64, u64) {
    let _ = fs::remove_file(path);
    let mut conn = Connection::open(path).unwrap();
    let t = Instant::now();
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE notes USING fts5(body, tokenize='{tokenize}');"
    ))
    .unwrap();
    let tx = conn.transaction().unwrap();
    {
        let mut stmt = tx
            .prepare("INSERT INTO notes(rowid, body) VALUES (?1, ?2)")
            .unwrap();
        for (i, body) in bodies.iter().enumerate() {
            stmt.execute(rusqlite::params![i as i64 + 1, body]).unwrap();
        }
    }
    tx.commit().unwrap();
    conn.execute("INSERT INTO notes(notes) VALUES('optimize')", [])
        .unwrap();
    let build_ms = t.elapsed().as_secs_f64() * 1000.0;
    drop(conn);
    (build_ms, fs::metadata(path).unwrap().len())
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// クエリを WARMUP+RUNS 回実行し、(結果 or エラー, 中央値ms) を返す
fn timed_query(conn: &Connection, sql: &str, params: &[String]) -> (Result<BTreeSet<i64>, String>, f64) {
    let run = || -> Result<BTreeSet<i64>, String> {
        let mut stmt = conn.prepare_cached(sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |r| {
                r.get::<_, i64>(0)
            })
            .map_err(|e| e.to_string())?;
        let mut set = BTreeSet::new();
        for r in rows {
            set.insert(r.map_err(|e| e.to_string())?);
        }
        Ok(set)
    };
    for _ in 0..WARMUP {
        let _ = run();
    }
    let mut times = Vec::with_capacity(RUNS);
    let mut last = Err("no run".to_string());
    for _ in 0..RUNS {
        let t = Instant::now();
        last = run();
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    (last, median(times))
}

struct Measurement {
    hits: usize,
    recall: f64,
    false_pos: usize,
    median_ms: f64,
    err: Option<String>,
}

fn evaluate(result: Result<BTreeSet<i64>, String>, ms: f64, truth: &BTreeSet<i64>) -> Measurement {
    match result {
        Ok(hits) => {
            let tp = hits.intersection(truth).count();
            Measurement {
                hits: hits.len(),
                recall: tp as f64 / truth.len() as f64,
                false_pos: hits.difference(truth).count(),
                median_ms: ms,
                err: None,
            }
        }
        Err(e) => Measurement {
            hits: 0,
            recall: 0.0,
            false_pos: 0,
            median_ms: ms,
            err: Some(e),
        },
    }
}

fn main() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let trigram_db = dir.join("trigram.db");
    let lindera_db = dir.join("lindera.db");
    let trigram_db = trigram_db.to_str().unwrap();
    let lindera_db = lindera_db.to_str().unwrap();

    println!("# ja-tokenize PoC 実行結果 (raw)\n");
    println!("- SQLite (bundled): {}", rusqlite::version());
    println!("- corpus: {NUM_DOCS} docs, warmup {WARMUP}, timed runs {RUNS} (median)\n");

    // ---- corpus ----
    let t = Instant::now();
    let (texts, truth) = build_corpus();
    println!(
        "corpus built+verified in {:.1} ms (total bytes: {})\n",
        t.elapsed().as_secs_f64() * 1000.0,
        texts.iter().map(|s| s.len()).sum::<usize>()
    );

    // ---- lindera ----
    let t = Instant::now();
    let dictionary = load_dictionary("embedded://ipadic").expect("load ipadic");
    let dict_ms = t.elapsed().as_secs_f64() * 1000.0;
    let segmenter = Segmenter::new(Mode::Normal, dictionary, None);
    println!("lindera embedded ipadic loaded in {dict_ms:.1} ms\n");

    println!("## lindera 分かち書きサンプル(クエリ側と同じ処理)\n");
    for t in TARGETS {
        println!("- `{}` -> `{}`", t.word, wakati(&segmenter, t.word));
    }
    for s in [
        "埋め込みベクトルの次元数を変更した。",
        "リソースのライフサイクルを管理する仕組みを導入した。",
        "認証フローの見直しを行った。",
    ] {
        println!("- `{s}` -> `{}`", wakati(&segmenter, s));
    }
    println!();

    // ---- build indexes ----
    let (a_build_ms, a_size) = build_fts(trigram_db, "trigram", &texts);
    println!("A/A' (trigram) build: {a_build_ms:.1} ms, db size: {a_size} bytes");

    let t = Instant::now();
    let tokenized: Vec<String> = texts.iter().map(|s| wakati(&segmenter, s)).collect();
    let tokenize_ms = t.elapsed().as_secs_f64() * 1000.0;
    let (b_insert_ms, b_size) = build_fts(lindera_db, "unicode61", &tokenized);
    println!(
        "B (lindera+unicode61) build: tokenize {tokenize_ms:.1} ms + insert {b_insert_ms:.1} ms = {:.1} ms, db size: {b_size} bytes\n",
        tokenize_ms + b_insert_ms
    );

    // ---- measure ----
    let conn_a = Connection::open(trigram_db).unwrap();
    let conn_b = Connection::open(lindera_db).unwrap();

    let match_sql = "SELECT rowid FROM notes WHERE notes MATCH ?1";
    let like_sql = "SELECT rowid FROM notes WHERE body LIKE ?1";

    println!("## 測定表\n");
    println!("| 語 | クラス | 正解数 | A hits | A 再現率 | A ms | A' hits | A' 再現率 | A' ms | B hits | B 再現率 | B ms | 誤ヒット A/A'/B |");
    println!("|---|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|---|");

    let mut errors: Vec<String> = Vec::new();
    let mut measure_row = |word: &str, class: &str, tset: &BTreeSet<i64>| {
        // A: trigram MATCH(語ごとにフレーズとして引用、空白区切りは AND)
        let q_a: String = word
            .split_whitespace()
            .map(|w| format!("\"{w}\""))
            .collect::<Vec<_>>()
            .join(" ");
        let (res, ms) = timed_query(&conn_a, match_sql, &[q_a]);
        let a = evaluate(res, ms, tset);

        // A': 同じテーブルに LIKE(語ごとに AND)
        let words: Vec<&str> = word.split_whitespace().collect();
        let like_sql_n = if words.len() == 1 {
            like_sql.to_string()
        } else {
            let conds: Vec<String> = (1..=words.len())
                .map(|i| format!("body LIKE ?{i}"))
                .collect();
            format!("SELECT rowid FROM notes WHERE {}", conds.join(" AND "))
        };
        let like_params: Vec<String> = words.iter().map(|w| format!("%{w}%")).collect();
        let (res, ms) = timed_query(&conn_a, &like_sql_n, &like_params);
        let ap = evaluate(res, ms, tset);

        // B: クエリ側も lindera で分かち書きしてフレーズ MATCH(空白区切りは AND)
        let q_b: String = word
            .split_whitespace()
            .map(|w| format!("\"{}\"", wakati(&segmenter, w)))
            .collect::<Vec<_>>()
            .join(" ");
        let (res, ms) = timed_query(&conn_b, match_sql, &[q_b]);
        let b = evaluate(res, ms, tset);

        for (label, m) in [("A", &a), ("A'", &ap), ("B", &b)] {
            if let Some(e) = &m.err {
                errors.push(format!("{label} `{word}`: {e}"));
            }
        }

        println!(
            "| {} | {} | {} | {} | {:.3} | {:.3} | {} | {:.3} | {:.3} | {} | {:.3} | {:.3} | {}/{}/{} |",
            word, class, tset.len(),
            a.hits, a.recall, a.median_ms,
            ap.hits, ap.recall, ap.median_ms,
            b.hits, b.recall, b.median_ms,
            a.false_pos, ap.false_pos, b.false_pos,
        );
    };

    for target in TARGETS {
        measure_row(target.word, target.class, &truth[target.word]);
    }

    // ---- 追加測定 ----
    // 正解集合は「最終テキストの部分文字列出現」で定義(ケース非依存 ASCII)
    let scan_truth = |needle: &str| -> BTreeSet<i64> {
        let words: Vec<String> = needle
            .split_whitespace()
            .map(|w| w.to_ascii_uppercase())
            .collect();
        texts
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                let upper = t.to_ascii_uppercase();
                words.iter().all(|w| upper.contains(w.as_str()))
            })
            .map(|(i, _)| i as i64 + 1)
            .collect()
    };

    println!("\n## 追加測定(部分語クエリと、キーワード列挙クエリ)\n");
    println!("正解集合は部分文字列出現(複数語は AND)で定義。");
    println!("「サイクル」は単一トークン「ライフサイクル」の内部、「アップ」は「バックアップ」の内部。\n");
    println!("| 語 | クラス | 正解数 | A hits | A 再現率 | A ms | A' hits | A' 再現率 | A' ms | B hits | B 再現率 | B ms | 誤ヒット A/A'/B |");
    println!("|---|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|---|");
    for (word, class) in [
        ("サイクル", "部分語"),
        ("アップ", "部分語"),
        ("認証 設計", "キーワード列挙"),
        ("運用 バックアップ", "キーワード列挙"),
    ] {
        measure_row(word, class, &scan_truth(word));
    }

    if !errors.is_empty() {
        println!("\n## クエリエラー(ヒット0として計上)\n");
        for e in errors {
            println!("- {e}");
        }
    }

    println!("\n## 構築サマリ\n");
    println!("| 方式 | 構築時間 (ms) | DB サイズ (bytes) | 備考 |");
    println!("|---|--:|--:|---|");
    println!("| A/A' trigram | {a_build_ms:.1} | {a_size} | 原文を索引・格納 |");
    println!(
        "| B lindera+unicode61 | {:.1} | {b_size} | 分かち書き {tokenize_ms:.1} ms + 挿入 {b_insert_ms:.1} ms。分かち書きテキストを格納 |",
        tokenize_ms + b_insert_ms
    );
}
