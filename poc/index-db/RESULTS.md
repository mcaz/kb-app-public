# PoC 結果: SQLite 1ファイルに FTS5 + sqlite-vec 同居(index-db)

実施日: 2026-08-09 / 実行: `~/.cargo/bin/cargo run --release`(2 回実行、結果は安定)

## 判定: 全体 PASS(5/5)

| # | 検証項目 | 結果 | 実測メモ |
|---|---|---|---|
| 1 | bundled SQLite で DB 作成・WAL 化・FTS5 テーブル | **PASS** | sqlite_version()=3.53.2 / journal_mode=wal / ENABLE_FTS5=true |
| 2 | sqlite-vec を auto_extension 登録、vec0 テーブル | **PASS** | vec_version()=v0.1.9 / vec0(embedding float[8]) 作成成功 |
| 3 | FTS5 MATCH × vec0 KNN の rowid JOIN ハイブリッド検索 | **PASS** | 300 行投入。1 SQL(サブクエリ2つの JOIN)で 5 件、全件が期待行 |
| 4 | 排他: busy_timeout 未設定=即 BUSY / 5s 設定=待って成功 / WAL 併読 | **PASS** | 保持中の読み 0ms で成功 / timeout 無し 0ms で SQLITE_BUSY / timeout 5s は約 2.0s 待って成功 |
| 5 | fail-open: vec 拡張なし接続で FTS5 のみ+劣化フラグ | **PASS** | `no such module: vec0` を catch → FTS5 のみ 5 件+ `degraded: Some(理由)` を返却 |

## 使用 crate / バージョン

- rusqlite **0.40.2**(features=["bundled"])
- libsqlite3-sys **0.38.2**(rusqlite 経由)→ bundled SQLite 本体 **3.53.2**(FTS5 有効ビルド)
- sqlite-vec **0.1.9**(C 拡張を cc でバンドルする Rust crate。`vec_version()` = v0.1.9)
- 追加依存なし(乱数は自前 xorshift、rand 不使用)

## ハマった点と回避策

1. **auto_extension はプロセスグローバル**。`sqlite3_auto_extension(sqlite3_vec_init)` を一度呼ぶと
   以後プロセス内で開く全接続に vec0 が入る。項目 5 の「素の接続」を作るには
   `sqlite3_cancel_auto_extension` で登録解除が必要だった。登録時は
   `std::mem::transmute` で `unsafe extern "C" fn(*mut sqlite3, *mut *mut c_char, *const sqlite3_api_routines) -> c_int`
   へキャストする(sqlite-vec README の定型)。
2. **vec0 の KNN は `k` 制約が仮想テーブルまで届く形で書く**。JOIN で使うときは
   サブクエリ内に `WHERE embedding MATCH ? AND k = 50` を入れる。外側クエリの `LIMIT` は
   vtab に押し下げられないため、内側の `k` を省くとエラー(`A LIMIT or 'k = ?' constraint is required`)。
3. **`PRAGMA journal_mode=WAL` は行を返す**。rusqlite では `execute` ではなく
   `query_row` で受ける(返り値 "wal" の確認も兼ねる)。
4. ベクトルは今回 JSON テキスト(`"[0.1,0.2,...]"`)でバインドした。本実装では
   f32 スライスを LE バイト列(`&[u8]`)で BLOB バインドする方が速くパースも不要。
5. busy_timeout は rusqlite 既定で 0(即 BUSY)。PoC では明示的に `Duration::ZERO` を
   セットして (a) を検証した。

## kb-app 本実装への含意

- **索引 DB は SQLite 1 ファイルで成立**。FTS5 と vec0 が同一 DB・同一 rowid 空間に同居でき、
  ハイブリッド検索は 1 SQL で書ける。別プロセスのベクトルストアは不要。
- **同時アクセス(Claude Code + Desktop が同じ DB を触るケース、要件ノートの未決事項)**:
  WAL + 全接続での `busy_timeout` 設定を接続オープン時の必須手順にすれば、
  読みは書き込み保持中でもブロックされず、書き(索引更新)は直列化されて待つだけで済む。
  書き込みトランザクションは `BEGIN IMMEDIATE` で取り、保持時間を短く保つこと。
- **fail-open は「Result ではなく 結果+劣化情報」を返す検索 API** で自然に書ける
  (`SearchOutcome { hits, degraded: Option<String> }`)。vec 拡張が無い・壊れた環境でも
  FTS5 だけで検索が生き残る。劣化フラグは FR-M2(健全性可視化)の表示ソースにできる。
- **登録はプロセス起動時に一度だけ**。auto_extension のグローバル性ゆえ、接続プールや
  スレッドから接続を開く前に main 冒頭で登録するのが安全(解除 API があるのでテストも書ける)。
- **注意**: sqlite-vec は 0.1.x(pre-1.0)。vec0 の KNN は総当たり(ANN 索引なし)だが、
  個人 KB 規模(数千ノート)では問題にならない見込み。crate 更新で `k` 制約まわりの
  仕様変化に注意。DB ファイル自体は素の SQLite でも読めるため、vec0 テーブルに触らない
  限り外部ツール(sqlite3 CLI 等)との共存も可。
