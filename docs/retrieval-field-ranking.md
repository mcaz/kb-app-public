# Field別ランキング効果測定

- 実施日: 2026-08-23
- 対象: `fielded-title-ranking` challenge（3 surface）
- 比較基盤: `retrieval-google-benchmark.example.json` schema 1.0.0

## 変更

既存FTSのBM25上位だけで確定せず、最終件数の8倍を候補として取得し、query termが現れる
fieldごとに固定重みで再順位付けする。

| field | 1 termあたりの重み |
| --- | ---: |
| title | 64 |
| description | 16 |
| tag | 12 |
| namespace / scope | 8 |
| body | 1 |

完全タイトル一致は最優先とし、それ以外では既存のactive canonical優先を維持する。同じfield内の
反復回数は加点しないため、本文に同じ語を大量反復したノートがtitle一致を押し出さない。
SQLite schemaは変えず、既存indexの再構築を発生させない。

個別効果が交絡しないよう、historical intentと重複排除のfixtureはtitle fieldの一致語数を
正解・誤答間で揃えた。変更前の検索器では両ケースとも引き続きFAILし、初期baselineのrecall／
precisionは変わらないことを確認した。

## 実測

| 指標 | 変更前 | 変更後 | 差分 |
| --- | ---: | ---: | ---: |
| 対象3 surface selected recall | 0.0% | 100.0% | +100.0pt |
| 対象3 surface selected precision | 0.0% | 20.0% | +20.0pt |
| 対象3 surface avg tokens | 1,390 | 1,364 | -26 |
| challenge全体 selected recall | 16.7% | 33.3% | +16.6pt |
| challenge全体 selected precision | 18.3% | 21.7% | +3.4pt |
| control failed surfaces | 0 | 0 | ±0 |
| control selected precision | 73.7% | 72.4% | -1.3pt |

変更前は5件の本文反復canonicalだけが選ばれ、完全タイトル一致の
`notes/nebula-launch-checklist`は候補にも入らなかった。変更後は同ノートが全surfaceで1位になり、
required本文を取得した。control gateとrecallは維持したが、control precisionは1.3pt低下したため、
後続の重複排除・多様化でも再測定する。時間値は端末差が大きいため効果判定から除く。
