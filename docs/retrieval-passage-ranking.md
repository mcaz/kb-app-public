# Passage rankingの効果測定

- 実施日: 2026-08-23
- 対象: `passage-ranking-cost` challenge（3 surface）
- 比較対象: Typed relation重み付き伝播適用後

## 変更

推定4,000 tokenを超える長文ノートだけを対象に、Markdown見出しsectionへ分け、巨大sectionまたは
見出しのない本文は2,400 byte以下へ分割する。queryの異なる語を何種類含むかでpassageを順位付けし、
完全query一致を最優先、同点は本文順に固定する。重複passageを除き、最大3 passage・推定3,600 tokenを
frontmatterとともに返す。短いノート、候補順、最大10本文、全体10,000 token予算は変更しない。

## 実測

| 指標 | 変更前 | 変更後 | 差分 |
| --- | ---: | ---: | ---: |
| 対象3 surface selected recall | 100.0% | 100.0% | ±0 |
| 対象3 surface selected precision | 100.0% | 25.0% | -75.0pt |
| 対象3 surface avg tokens | 26,128 | 3,265 | -22,863 (-87.5%) |
| 対象3 surface spill | 3 | 0 | -3 |
| 対象3 surface budget exhausted | 3 | 0 | -3 |
| challenge全体 selected recall | 83.3% | 83.3% | ±0 |
| challenge全体 selected precision | 32.2% | 19.7% | -12.5pt |
| challenge全体 avg tokens | 5,733 | 1,922 | -3,811 (-66.5%) |
| control selected recall | 100.0% | 100.0% | ±0 |
| control selected precision | 72.4% | 72.4% | ±0 |

`notes/atlas-recovery-handbook`のrequired本文は全surfaceで保持し、spillと予算超過を解消した。
文書precisionの低下は、変更前には巨大な先頭文書が予算を使い切って後続を全て抑止していたのに対し、
変更後は空いた予算へ後続3文書も入ったためである。required recallを落とさずコンテキスト量を削減できたが、
回答精度への影響は実発話評価で別途確認する必要がある。
