# アンカーテキスト索引の効果測定

- 実施日: 2026-08-23
- 対象: `anchor-text-alias` challenge（3 surface）
- 比較対象: Query intent適用後

## 変更

標準Markdown linkの`[anchor text](target.md)`からanchor textとリンク先を取り出し、リンク先に
紐づく専用FTS5索引へ格納する。リンク先のtitleや本文にquery語が無くても、別名や通称から検索できる。

アンカーは本文検索より弱い補助信号なので、OR検索で1語だけ一致したリンク先は昇格せず、全query
termが一致した場合だけ採用する。完全タイトル一致と明示的query intentはアンカーより優先する。
DB schema v8への移行時は、SQLiteの既存本文から索引を一度だけ再構築し、Markdownの直接走査や
埋め込み再計算は行わない。更新・削除時もsource単位で同期する。

## 実測

| 指標 | 変更前 | 変更後 | 差分 |
| --- | ---: | ---: | ---: |
| 対象3 surface selected recall | 0.0% | 100.0% | +100.0pt |
| 対象3 surface selected precision | 0.0% | 33.3% | +33.3pt |
| 対象3 surface avg tokens | 1,300 | 1,571 | +271 |
| challenge全体 selected recall | 50.0% | 66.7% | +16.7pt |
| challenge全体 selected precision | 25.0% | 30.6% | +5.6pt |
| challenge全体 avg tokens | 5,682 | 5,727 | +45 |
| control selected recall | 100.0% | 100.0% | ±0 |
| control selected precision | 72.4% | 72.4% | ±0 |

変更前は本文反復decoy 5件だけが選ばれた。変更後は`blue comet policy`アンカーのリンク先
`notes/kepler-retention-procedure`が全surfaceで1位になり、リンク元`notes/storage-map`とともに
required本文を取得した。対象token増加は正解本文とリンク元本文を取得した分。未着手のtyped relation
rankingと重複排除・多様化の6 surfaceはFAILのまま維持した。
