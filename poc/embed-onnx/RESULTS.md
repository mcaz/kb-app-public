# PoC 結果: ONNX Runtime (ort) + bge-m3 int8 内蔵埋め込み

検証日: 2026-08-09 / 環境: Apple M1 Pro, 34GB RAM, macOS (Darwin 25.5.0), rustc 1.97.1

**判定: 品質 PASS / 速度 PASS / サイズ PASS(条件付き: 実行時メモリは要注視)**

## モデル出典

| 項目 | 値 |
|---|---|
| リポジトリ | [Xenova/bge-m3](https://huggingface.co/Xenova/bge-m3)(transformers.js 向け公式 ONNX 変換) |
| モデル | `onnx/model_int8.onnx` — **568,456,694 bytes(542 MiB)**。外部データ不要の単一ファイル |
| tokenizer | `tokenizer.json` — 17,082,821 bytes(16.3 MiB) |
| 同梱合計 | **約 586 MB** →「数百MB」要件を満たす |
| 入出力 | 入力 `input_ids` + `attention_mask`(token_type_ids 不要)/ 出力 `last_hidden_state` のみ |
| プーリング | `sentence_embedding` 出力は**無い** → 最終層 [CLS](先頭トークン)+ L2 正規化を自前実装(bge-m3 dense の公式仕様どおり) |

参考: 同リポジトリの fp32 `model.onnx` は外部データ 2,266.8 MB が別途必要で同梱不可。
`model_uint8`(568.5 MB)/ `model_quantized`(569.7 MB)/ `model_q4f16`(699.9 MB)も候補として存在。

## 品質(コサイン距離 = 1 − cos類似度、ort int8)

テスト: 日本語中心 5 関連ペア(うち日英クロス2)+ 5 無関係ペア(うち日英クロス2)。

| 種別 | 距離帯 |
|---|---|
| 関連ペア | 0.166 – 0.427(日英クロスが最も近い 0.166/0.196) |
| 無関係ペア | 0.692 – 0.752 |
| 分離 | **完全分離、マージン 0.266**(関連max 0.427 < 無関係min 0.692) |

既存較正帯(関連 0.72–0.88 / 無関係 1.08–1.14)との**絶対値は一致しない**が、これは
較正帯が「短いクエリ ↔ ノート全文」の本番検索経路での実測、本 PoC が「文 ↔ 文」ペア
であるための差(ollama 側で同じペアを測っても 0.14–0.74 で同様)。較正移植の判断は
絶対値でなく下のパリティで行う。

## パリティ(ollama bge-m3 F16 ↔ ort int8、同一テキスト 22 本)

ollama /api/embed(bge-m3:latest, GGUF F16)実施可。

| 指標 | 値 |
|---|---|
| 同一テキストの cos 類似度 | **min 0.9769 / 中央値 0.9844 / max 0.9887**(n=22) |
| ペア距離の差 \|Δ\|(10ペア) | 中央値 0.011 / **max 0.029** |
| 差の傾向 | 関連(近い)ペアで系統的に +0.02〜+0.03(int8 側が僅かに遠くなる)、無関係ペアはほぼ一致(±0.01) |

**較正移植の見込み**: 距離のずれは最大 +0.03。既存帯に当てはめると
関連上限 0.88 → 最悪 0.91 で前出し閾値 0.95 を割らず、無関係下限 1.08 → 最悪 1.05 で
0.95 を超えたまま。**閾値 0.95 はそのまま移植可能**。安全網の 0.85 は設計上
関連帯(0.72–0.88)の内側にある厳格閾値のため、±0.03 のずれで境界ノートの通過が
入れ替わる可能性があり、**移行時に 0.85 側のみ軽い再確認を推奨**。

## 性能・サイズ

| 項目 | 値 |
|---|---|
| モデルロード | 0.43–0.78 秒(2回実測。初回推論 +14–18ms) |
| 短文(36字 / 27 tok) | **中央値 28ms**(min 24 / 2回の実行で 27.8 / 28.2ms と安定) |
| 長文(406字 / 244 tok) | **中央値 172ms** |
| ピーク RSS | **1.80 GB**(/usr/bin/time -l、全検証込みのプロセス全体) |
| 実行 EP | CPU(CoreML EP はバイナリに同梱されるが未使用) |

レイテンシはほぼトークン数線形(~0.7ms/token)。ノート千件規模の全再埋め込みでも
数分オーダーで実用範囲。

## 使用 crate

- `ort = 2.0.0-rc.13`(features: `download-binaries`, `ndarray`。ONNX Runtime **1.28.0** の静的バイナリを build 時に CDN 取得、aarch64-apple-darwin+coreml)
- `tokenizers = 0.23.1`(default-features off + `onig`)
- `ndarray = 0.17.2` / `ureq = 2`(ollama 呼び出し)/ `anyhow`, `serde_json`

## ハマった点

1. **ort rc.13 の `Error<R>` は Send/Sync でない**(失敗時に builder 等の状態 R を返す設計)。
   anyhow へ `?` 変換できず E0277 が 9 箇所。Display 経由で変換するヘルパ trait
   (`src/main.rs` の `OrtCtx`)で解決。
2. docs.rs の ort トップにはコード例が無い。**リポジトリの tag `v2.0.0-rc.13` 配下
   `examples/sentence-transformers/semantic-similarity.rs`** が現行 API の正
   (`Session::builder()` → `commit_from_file`、`TensorRef::from_array_view(([N,L], &*vec))`、
   `ort::inputs!["name" => tensor]`、`try_extract_array::<f32>()`)。
3. Xenova/bge-m3 の fp32 は外部データ形式(.onnx + 2.3GB の .onnx_data)。量子化版は
   単一ファイルなのでパス管理が楽。
4. encode_batch のパディング挙動に依存しないよう **batch=1 で1文ずつ推論**(kb-app の
   用途はノート単位埋め込みなので実運用と同型)。
5. 較正帯の絶対値照合は「文↔文」ペアでは再現できない(上記)。パリティ(同一テキストの
   ランタイム間類似度)で判断するのが正しい比較。

## kb-app 本実装への含意

- **同梱要件**: モデル+tokenizer で約 586 MB。「数百MB」要件を満たす。ollama 依存を
  外せる。
- **較正移植**: 閾値 0.95 はそのまま可。0.85(厳格側)のみ移行時に実測再確認。
  ただし **ollama 由来の既存ベクトルと ort 由来ベクトルの混在はさせない**こと
  (同一テキストでも類似度 0.984 の差 = 距離に ±0.02〜0.03 のノイズ)。移行時は
  インデックス全体を ort で再埋め込みする(千件規模なら数分)。
- **速度**: 短文 28ms / 長文 172ms(M1 Pro CPU)。対話的検索・フック用途とも実用十分。
  ollama 経由の HTTP 往復も消える。
- **要注視**: ピーク RSS 1.8 GB(モデル 568MB に対し約3倍)。常駐プロセスに載せる場合は
  セッションのメモリアリーナ設定(`with_memory_pattern` 無効化等)や、必要時ロード/
  アイドル時解放の検討余地あり。q4f16 版(700MB)や CoreML EP の評価は未実施。
- 再現手順: `cd poc/embed-onnx && cargo build --release && ./target/release/embed-onnx-poc`
  (モデルは `models/` に配置済み。フル出力は `run-full.log`)。
