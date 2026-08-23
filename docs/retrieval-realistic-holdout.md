# 実運用風holdoutでの全施策比較

- 実施日: 2026-08-23
- 改善前: `codex/google-retrieval-eval` (`4a6acad`)
- 全施策ON: `codex/retrieval-dedup-diversification` (`2669cc1`)
- fixture: `schemas/examples/retrieval-realistic-holdout.example.json`

## 目的

施策ごとの初期benchmarkとは語彙・題材を分離し、6施策を同時に有効化したときの汎化と干渉を測る。
実KBや実会話は使わず、運用・security・finance・incident・migrationを模した55件の合成ノートを
隔離した一時Vaultへ作る。5 controlと7 challengeをCodex・Claude Code・ChatGPTの3 surface、
合計36 surfaceで評価する。

```bash
kb eval retrieval-benchmark \
  --suite schemas/examples/retrieval-realistic-holdout.example.json
```

holdoutには完全タイトル、日本語短語、通常link、現行canonicalに加えて、次を含めた。

- 同じテンプレートだが東京・大阪で固有事項が異なる記録
- タイトルと本文反復が競合する請求手順
- 現行文書と履歴recordが競合する移行理由
- 本文に現れない別名anchor
- 9件の`mentions`と1件の`supports`が競合する一次証拠
- 51,000 token規模の長文復旧手順
- 同一digest群に埋もれた異なるrisk facet
- anchor textと履歴intentを同時に必要とする複合query

## 総合結果

`linked_v1`の同一設定で比較した。

| 36 surface総合 | 改善前 | 全施策ON | 差分 |
| --- | ---: | ---: | ---: |
| gate PASS | 18 / 36 | 36 / 36 | +18 |
| candidate recall | 66.7% | 100.0% | +33.3pt |
| selected recall | 50.0% | 100.0% | +50.0pt |
| selected precision | 41.1% | 40.6% | -0.5pt |
| 平均selected documents | 4.00 | 4.92 | +0.92 |
| 平均推定token | 9,627 | 1,486 | -8,141 (-84.6%) |
| spill | 6 | 0 | -6 |
| budget exhausted | 6 | 0 | -6 |

| 集合 | 指標 | 改善前 | 全施策ON |
| --- | --- | ---: | ---: |
| control 15 surface | selected recall | 80.0% | 100.0% |
| control 15 surface | selected precision | 50.0% | 55.7% |
| control 15 surface | 平均token | 10,794 | 1,059 |
| challenge 21 surface | selected recall | 28.6% | 100.0% |
| challenge 21 surface | selected precision | 34.8% | 29.8% |
| challenge 21 surface | 平均token | 8,793 | 1,792 |

改善前は18 surfaceがFAILした。長文の一般語反復が地域別記録より上位になってtoken予算を使い切る
ケースもあり、検索順位とpassage縮約の組み合わせで回収できた。全施策ONでは、東京・大阪の近似
テンプレートを誤って同一clusterへ潰さず、全required本文を取得した。

## 読み取り

recall、失敗surface、token予算超過の改善は別語彙のholdoutでも再現した。一方、challenge precisionは
5.0pt低下した。改善前のdedupケースは必要なrisk facetを欠いたまま同一digest 5件を全てrelevantとして
数えるためprecisionだけは100%になり、全施策ONでは正解回収とrelation展開で取得文書数が増える。
したがってgate・recallの改善をprecision単独で否定はできないが、複合anchor+intentで10文書を選ぶなど
過剰取得は残る。次の改善候補は、graph展開後のquery-aware rerankと低信号候補の打ち切りである。
