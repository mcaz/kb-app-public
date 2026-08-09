//! PoC: kb-app index DB — SQLite 1 ファイルに FTS5 + sqlite-vec (vec0) を同居させ、
//! busy_timeout と fail-open が成立するかの技術検証。
//!
//! 実行: cargo run --release
//! 各項目の PASS/FAIL を stdout に出し、全 PASS なら exit 0。

use rusqlite::ffi;
use rusqlite::{Connection, ErrorCode};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const DIM: usize = 8;
const ROWS: i64 = 300;
const DB_PATH: &str = "poc.db";

/// 決定的な疑似乱数(依存 crate を増やさないため xorshift64)
struct Rng(u64);
impl Rng {
    fn next_f32(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % 10_000) as f32 / 10_000.0
    }
}

fn vec_to_json(v: &[f32]) -> String {
    let items: Vec<String> = v.iter().map(|x| format!("{x:.6}")).collect();
    format!("[{}]", items.join(","))
}

/// fail-open デモ用: 検索結果は Result ではなく「結果 + 劣化情報」で返す
struct SearchOutcome {
    hits: Vec<(i64, String, Option<f64>)>, // (rowid, body, distance)
    degraded: Option<String>,              // Some(理由) = ベクトル検索なしの劣化モード
}

fn hybrid_query(
    conn: &Connection,
    fts_query: &str,
    query_vec_json: &str,
    k: usize,
) -> rusqlite::Result<Vec<(i64, String, Option<f64>)>> {
    let sql = "
        SELECT f.rowid, f.body, v.distance
        FROM (SELECT rowid, body FROM notes_fts WHERE notes_fts MATCH ?1) AS f
        JOIN (SELECT rowid, distance FROM notes_vec
              WHERE embedding MATCH ?2 AND k = 50) AS v
          ON v.rowid = f.rowid
        ORDER BY v.distance
        LIMIT ?3";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(
        rusqlite::params![fts_query, query_vec_json, k as i64],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, Some(r.get::<_, f64>(2)?))),
    )?;
    rows.collect()
}

fn fts_only_query(
    conn: &Connection,
    fts_query: &str,
    k: usize,
) -> rusqlite::Result<Vec<(i64, String, Option<f64>)>> {
    let mut stmt = conn.prepare(
        "SELECT rowid, body FROM notes_fts WHERE notes_fts MATCH ?1 ORDER BY rank LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![fts_query, k as i64], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, None))
    })?;
    rows.collect()
}

/// vec0 が使えなければ FTS5 のみに劣化して返す(パニックも Err も伝播させない)
fn search_fail_open(
    conn: &Connection,
    fts_query: &str,
    query_vec_json: &str,
    k: usize,
) -> SearchOutcome {
    match hybrid_query(conn, fts_query, query_vec_json, k) {
        Ok(hits) => SearchOutcome { hits, degraded: None },
        Err(e) => {
            let hits = fts_only_query(conn, fts_query, k).unwrap_or_default();
            SearchOutcome {
                hits,
                degraded: Some(format!("vector search unavailable: {e}")),
            }
        }
    }
}

fn main() {
    let mut results: Vec<(&str, bool, String)> = Vec::new();
    let pass = |name: &'static str, ok: bool, detail: String, results: &mut Vec<(&str, bool, String)>| {
        println!("[{}] {} — {}", if ok { "PASS" } else { "FAIL" }, name, detail);
        results.push((name, ok, detail));
    };

    // 前回の残骸を掃除
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{DB_PATH}{suffix}"));
    }

    // ------------------------------------------------------------------
    // 1. bundled SQLite で DB 作成、WAL 化、FTS5 仮想テーブル
    // ------------------------------------------------------------------
    let step1 = (|| -> rusqlite::Result<String> {
        let conn = Connection::open(DB_PATH)?;
        let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        let ver: String = conn.query_row("SELECT sqlite_version()", [], |r| r.get(0))?;
        let fts5_opt: bool =
            conn.query_row("SELECT sqlite_compileoption_used('ENABLE_FTS5')", [], |r| r.get(0))?;
        conn.execute_batch(
            "CREATE VIRTUAL TABLE notes_fts USING fts5(body);
             CREATE TABLE plain_meta(id INTEGER PRIMARY KEY, note TEXT);",
        )?;
        if mode != "wal" {
            return Err(rusqlite::Error::InvalidQuery); // WAL にならなかった
        }
        Ok(format!(
            "sqlite_version()={ver}, journal_mode={mode}, ENABLE_FTS5={fts5_opt}, FTS5 テーブル作成成功"
        ))
    })();
    match &step1 {
        Ok(d) => pass("1. bundled SQLite + WAL + FTS5", true, d.clone(), &mut results),
        Err(e) => pass("1. bundled SQLite + WAL + FTS5", false, format!("{e}"), &mut results),
    }

    // ------------------------------------------------------------------
    // 2. sqlite-vec を auto_extension で登録し vec0 テーブル作成
    // ------------------------------------------------------------------
    unsafe {
        ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut ffi::sqlite3,
                *mut *mut std::os::raw::c_char,
                *const ffi::sqlite3_api_routines,
            ) -> std::os::raw::c_int,
        >(sqlite_vec::sqlite3_vec_init as *const ())));
    }
    let step2 = (|| -> rusqlite::Result<String> {
        let conn = Connection::open(DB_PATH)?; // 登録後に開いた接続には vec0 が入る
        let vec_ver: String = conn.query_row("SELECT vec_version()", [], |r| r.get(0))?;
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE notes_vec USING vec0(embedding float[{DIM}]);"
        ))?;
        Ok(format!("vec_version()={vec_ver}, vec0(float[{DIM}]) テーブル作成成功"))
    })();
    match &step2 {
        Ok(d) => pass("2. sqlite-vec 登録 + vec0 テーブル", true, d.clone(), &mut results),
        Err(e) => pass("2. sqlite-vec 登録 + vec0 テーブル", false, format!("{e}"), &mut results),
    }

    // ------------------------------------------------------------------
    // 3. トイデータ投入 + ハイブリッド検索(FTS5 MATCH × vec0 KNN を rowid JOIN)
    // ------------------------------------------------------------------
    let vocab = ["note", "index", "sqlite", "vault", "search", "memo", "draft", "link"];
    let query_vec: Vec<f32> = {
        let mut v = vec![0.05_f32; DIM];
        v[0] = 1.0;
        v
    };
    let step3 = (|| -> rusqlite::Result<String> {
        let conn = Connection::open(DB_PATH)?;
        let tx = conn.unchecked_transaction()?;
        let mut rng = Rng(0x5eed_5eed_5eed_5eed);
        for i in 1..=ROWS {
            // 7 の倍数行にキーワード "zettelkasten" を入れ、ベクトルも query_vec の近傍にする
            let keyed = i % 7 == 0;
            let mut words: Vec<&str> = (0..12)
                .map(|_| vocab[(rng.next_f32() * 8.0) as usize % 8])
                .collect();
            if keyed {
                words.push("zettelkasten");
            }
            let body = format!("doc {i}: {}", words.join(" "));
            let emb: Vec<f32> = (0..DIM)
                .map(|d| {
                    if keyed {
                        query_vec[d] + (rng.next_f32() - 0.5) * 0.1 // 近傍
                    } else {
                        rng.next_f32() * 2.0 - 1.0 // ランダム
                    }
                })
                .collect();
            tx.execute(
                "INSERT INTO notes_fts(rowid, body) VALUES (?1, ?2)",
                rusqlite::params![i, body],
            )?;
            tx.execute(
                "INSERT INTO notes_vec(rowid, embedding) VALUES (?1, ?2)",
                rusqlite::params![i, vec_to_json(&emb)],
            )?;
        }
        tx.commit()?;

        let hits = hybrid_query(&conn, "zettelkasten", &vec_to_json(&query_vec), 5)?;
        if hits.is_empty() {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        let all_keyed = hits.iter().all(|(rowid, body, dist)| {
            rowid % 7 == 0 && body.contains("zettelkasten") && dist.is_some()
        });
        if !all_keyed {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let top = &hits[0];
        Ok(format!(
            "{ROWS} 行投入。ハイブリッド検索 {} 件ヒット(全件 rowid%7==0 かつ本文一致)。top: rowid={} distance={:.4}",
            hits.len(),
            top.0,
            top.2.unwrap()
        ))
    })();
    match &step3 {
        Ok(d) => pass("3. FTS5×vec0 ハイブリッド検索(rowid JOIN)", true, d.clone(), &mut results),
        Err(e) => pass("3. FTS5×vec0 ハイブリッド検索(rowid JOIN)", false, format!("{e}"), &mut results),
    }

    // ------------------------------------------------------------------
    // 4. 排他: 書き込み保持中の SQLITE_BUSY / busy_timeout / WAL 読み
    // ------------------------------------------------------------------
    let step4 = (|| -> Result<String, String> {
        let (tx_locked, rx_locked) = mpsc::channel::<()>();
        let writer = thread::spawn(move || -> rusqlite::Result<()> {
            let conn = Connection::open(DB_PATH)?;
            conn.execute_batch(
                "BEGIN IMMEDIATE;
                 INSERT INTO plain_meta(note) VALUES ('held by writer A');",
            )?;
            tx_locked.send(()).ok(); // 書きロック取得を通知
            thread::sleep(Duration::from_secs(2)); // 2 秒保持
            conn.execute_batch("COMMIT;")?;
            Ok(())
        });
        rx_locked
            .recv_timeout(Duration::from_secs(5))
            .map_err(|e| format!("writer がロックを取れなかった: {e}"))?;

        // (c) WAL: 書き込み保持中でも別コネクションの読みは通る
        let reader = Connection::open(DB_PATH).map_err(|e| e.to_string())?;
        let t = Instant::now();
        let n: i64 = reader
            .query_row("SELECT count(*) FROM notes_fts", [], |r| r.get(0))
            .map_err(|e| format!("保持中の読みが失敗(WAL で通るはず): {e}"))?;
        let read_ms = t.elapsed().as_millis();

        // (a) busy_timeout 未設定(0)→ 即 SQLITE_BUSY
        let b1 = Connection::open(DB_PATH).map_err(|e| e.to_string())?;
        b1.busy_timeout(Duration::ZERO).map_err(|e| e.to_string())?;
        let t = Instant::now();
        let r1 = b1.execute("INSERT INTO plain_meta(note) VALUES ('B no timeout')", []);
        let busy_ms = t.elapsed().as_millis();
        let immediate_busy = match &r1 {
            Err(e) if e.sqlite_error_code() == Some(ErrorCode::DatabaseBusy) => busy_ms < 500,
            _ => false,
        };
        if !immediate_busy {
            return Err(format!(
                "busy_timeout 未設定で即 SQLITE_BUSY にならなかった: result={r1:?}, elapsed={busy_ms}ms"
            ));
        }

        // (b) busy_timeout(5s) → 待って成功
        let b2 = Connection::open(DB_PATH).map_err(|e| e.to_string())?;
        b2.busy_timeout(Duration::from_secs(5)).map_err(|e| e.to_string())?;
        let t = Instant::now();
        b2.execute("INSERT INTO plain_meta(note) VALUES ('B with timeout')", [])
            .map_err(|e| format!("busy_timeout(5s) でも書けなかった: {e}"))?;
        let wait_ms = t.elapsed().as_millis();

        writer
            .join()
            .map_err(|_| "writer thread panicked".to_string())?
            .map_err(|e| format!("writer 側エラー: {e}"))?;
        if wait_ms < 200 {
            return Err(format!(
                "busy_timeout(5s) 側が待たずに成功({wait_ms}ms)— ロック保持を検証できていない"
            ));
        }
        Ok(format!(
            "保持中の読み OK({n} 行, {read_ms}ms)/ timeout 無し: 即 BUSY({busy_ms}ms)/ timeout 5s: {wait_ms}ms 待って成功"
        ))
    })();
    match &step4 {
        Ok(d) => pass("4. 排他(SQLITE_BUSY / busy_timeout / WAL 併読)", true, d.clone(), &mut results),
        Err(e) => pass("4. 排他(SQLITE_BUSY / busy_timeout / WAL 併読)", false, e.clone(), &mut results),
    }

    // ------------------------------------------------------------------
    // 5. fail-open: vec 拡張なしの素の接続で FTS5 のみに劣化
    // ------------------------------------------------------------------
    unsafe {
        // auto_extension は プロセスグローバルなので、素の接続を作るには登録解除が必要
        ffi::sqlite3_cancel_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut ffi::sqlite3,
                *mut *mut std::os::raw::c_char,
                *const ffi::sqlite3_api_routines,
            ) -> std::os::raw::c_int,
        >(sqlite_vec::sqlite3_vec_init as *const ())));
    }
    let step5 = (|| -> Result<String, String> {
        let plain = Connection::open(DB_PATH).map_err(|e| e.to_string())?;
        // vec0 が本当に居ないことを確認(直接叩けばエラーになる)
        let direct = plain.query_row("SELECT count(*) FROM notes_vec", [], |r| r.get::<_, i64>(0));
        if direct.is_ok() {
            return Err("素の接続でも vec0 が読めてしまった(登録解除に失敗)".into());
        }
        let out = search_fail_open(&plain, "zettelkasten", &vec_to_json(&query_vec), 5);
        match out.degraded {
            Some(reason) if !out.hits.is_empty() => Ok(format!(
                "vec0 直叩きは想定通りエラー → 劣化モードで FTS5 のみ {} 件返却。degraded={reason:?}",
                out.hits.len()
            )),
            Some(_) => Err("劣化フラグは立ったが FTS5 結果が空".into()),
            None => Err("素の接続なのに劣化フラグが立たなかった".into()),
        }
    })();
    match &step5 {
        Ok(d) => pass("5. fail-open(FTS5 のみ+劣化フラグ)", true, d.clone(), &mut results),
        Err(e) => pass("5. fail-open(FTS5 のみ+劣化フラグ)", false, e.clone(), &mut results),
    }

    // ------------------------------------------------------------------
    println!("----------------------------------------------------------");
    let all_ok = results.iter().all(|(_, ok, _)| *ok);
    println!(
        "OVERALL: {} ({}/{} passed)",
        if all_ok { "PASS" } else { "FAIL" },
        results.iter().filter(|(_, ok, _)| *ok).count(),
        results.len()
    );
    std::process::exit(if all_ok { 0 } else { 1 });
}
