# PoC レポート — ADR-0001 判定条件の実測(2026-08-09)

[ADR-0001](adr/0001-core-language.md) の kill criteria 3本をすべて実測。**3/3 PASS —
Rust 案は確定、sidecar 案の再評価は不要。** 再現コードと詳細は各 `poc/*/RESULTS.md`。

## 判定サマリ

| PoC | 判定 | 要点 |
|---|---|---|
| ③ 索引 DB([poc/index-db](../poc/index-db/RESULTS.md)) | **PASS 5/5** | SQLite 3.53.2(rusqlite bundled)1ファイルに FTS5+sqlite-vec v0.1.9 同居。ハイブリッド検索(rowid JOIN)成立。WAL 併読 OK・busy_timeout で待って成功・vec 不在時は FTS のみ+劣化フラグの fail-open が成立 |
| ② 日本語 FTS([poc/ja-tokenize](../poc/ja-tokenize/RESULTS.md)) | **PASS** | 旧欠陥(trigram の2文字語空振り)を再現した上で、lindera 分かち書き+unicode61 で再現率 0 → **1.000**。5,000 件でレイテンシ 0.01–0.03ms |
| ① 埋め込み内蔵([poc/embed-onnx](../poc/embed-onnx/RESULTS.md)) | **PASS**(品質/速度/サイズ) | ort 2.0rc+bge-m3 int8(Xenova/bge-m3、542MB+tokenizer 16MB=**約586MB**)。関連/無関係が完全分離。ollama F16 とのパリティ cos 類似度中央値 **0.984** |

## 本実装に持ち込む決定事項(PoC 由来)

1. **トークナイズは二本立て**: lindera(`embed-ipadic`)分かち書き+unicode61 を主索引
   (bm25 有効・全語クラス再現 1.000)、trigram 索引を併設して部分語クエリ
   (「サイクル」⊂「ライフサイクル」等、形態素方式が構造的に 0 件になる逆側の穴)の
   レスキュー経路にする。併設コストは実測で無視できる(+27ms/+2.1MB per 1MB コーパス)。
   trigram 単独の混在クエリは**2字語が黙って落ちて誤結果を返す**(0件より悪質)ことも
   実測 — 単独採用は禁忌
2. **埋め込みは bge-m3 int8 を同梱**: 「数百MB」要件クリア(586MB)。閾値 0.95 は
   パリティ実測(|Δ距離| max 0.029)込みでそのまま移植可、厳格側 0.85 のみ移行時に
   再確認。**ollama 由来の既存ベクトルとの混在は不可 — 移行は全件再埋め込み**
   (複合バージョンスタンプ `書式|provider:model:dim` の既存知見どおり)
3. **接続規律**: sqlite-vec の登録(auto_extension)はプロセスグローバルで接続前に
   1回。全接続で busy_timeout 必須。書きは `BEGIN IMMEDIATE`+短時間保持。
   ベクトルは f32 LE バイト列 BLOB でバインド
4. **検索 API は「結果+劣化情報」を返す形**(Result で全滅させない)— fail-open を
   型で強制し、劣化は UI に必ず出す(原則4)
5. **要注視(未解決)**: 埋め込み常駐時のピーク RSS **1.8GB**(モデルの約3倍)。
   遅延ロード/アイドル時アンロード/アリーナ設定を本実装で検討。lindera 辞書 55MB は
   バイナリ同梱か `file://`+mmap 外出しかを配布サイズと相談で決める

## 計測環境

M1 Pro / rustc 1.97.1 / rusqlite 0.40.2 / sqlite-vec 0.1.9 / lindera 5.0.2 /
ort 2.0.0-rc.13(ONNX Runtime 1.28.0)/ tokenizers 0.23.1。
モデル(poc/embed-onnx/models/、586MB)は .gitignore 済みで git に含まれない。
