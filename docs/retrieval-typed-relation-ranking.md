# Typed relation重み付き伝播の効果測定

- 実施日: 2026-08-23
- 対象: `typed-relation-ranking` challenge（3 surface）
- 比較対象: アンカーテキスト索引適用後

## 変更

検索上位seedから有向グラフを最大2 hop展開するとき、edgeの意味を3 tierへ分けて候補順へ反映する。

1. `derived_from`・`supports`・`updates`・`contradicts`・`supersedes`
2. 通常Markdown link
3. `mentions`

明示的な根拠・系譜・変更・矛盾・後継を、単なる言及より強い推薦信号として扱うseed-biasedな
重み付き伝播である。同じtierではノートID順に固定し、同一宛先へ複数edgeがある場合は最も強い
tierを採用する。候補上限・本文上限・token予算は変更しない。

## 実測

| 指標 | 変更前 | 変更後 | 差分 |
| --- | ---: | ---: | ---: |
| 対象3 surface selected recall | 0.0% | 100.0% | +100.0pt |
| 対象3 surface selected precision | 10.0% | 20.0% | +10.0pt |
| 対象3 surface avg tokens | 2,637 | 2,671 | +34 |
| challenge全体 selected recall | 66.7% | 83.3% | +16.7pt |
| challenge全体 selected precision | 30.6% | 32.2% | +1.7pt |
| challenge全体 avg tokens | 5,727 | 5,733 | +6 |
| control selected recall | 100.0% | 100.0% | ±0 |
| control selected precision | 72.4% | 72.4% | ±0 |

変更前も`notes/zeta-telemetry-record`は候補集合に入ったが、9件の`mentions`が先に本文上限10件を
埋めていた。変更後は`supports`の同ノートが最初の展開候補となり、全surfaceでrequired本文を取得した。
未着手の重複排除・多様化3 surfaceだけをFAILとして維持した。
