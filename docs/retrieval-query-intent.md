# Query intentランキング効果測定

- 実施日: 2026-08-23
- 対象: `query-intent-historical` challenge（3 surface）
- 比較対象: Field別ランキング適用後

## 変更

queryの明示語を次のauthority属性へ対応付け、完全タイトル一致の次に再順位付けする。

| intent | 主な明示語 | 加点するauthority |
| --- | --- | --- |
| current | 現行・現在・最新・current・latest | `active` status、`canonical` role |
| historical | 当時・過去・以前・履歴・historical・previous | `historical` status、次点で`superseded` |
| record | 記録・ログ・監査・日時・record・audit | `record` role、`records` namespace |
| rationale | 理由・根拠・経緯・なぜ・reason・why | `decisions` / `records` namespace |

明示語が無いqueryにはintent加点を行わず、従来のactive canonical優先を維持する。複数intentは
加算できるため、「当時の監査理由」のようなqueryはhistorical recordを選べる。SQLite schemaや
既存indexは変更しない。

## 実測

| 指標 | 変更前 | 変更後 | 差分 |
| --- | ---: | ---: | ---: |
| 対象3 surface selected recall | 0.0% | 100.0% | +100.0pt |
| 対象3 surface selected precision | 0.0% | 20.0% | +20.0pt |
| 対象3 surface avg tokens | 1,285 | 1,298 | +13 |
| challenge全体 selected recall | 33.3% | 50.0% | +16.7pt |
| challenge全体 selected precision | 21.7% | 25.0% | +3.3pt |
| challenge全体 avg tokens | 5,680 | 5,682 | +2 |
| control selected recall | 100.0% | 100.0% | ±0 |
| control selected precision | 72.4% | 72.4% | ±0 |

変更前はactive canonical 5件だけが選ばれ、historical recordの
`notes/orion-audit-record-2025`は候補外だった。変更後は同recordが全surfaceで1位になり、
required本文を取得した。対象tokenは正解本文が加わった分だけ13増えた。未着手のanchor text、
typed relation ranking、重複排除・多様化の9 surfaceはFAILのまま維持した。
