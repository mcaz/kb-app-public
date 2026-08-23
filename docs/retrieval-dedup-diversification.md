# 重複排除・多様化の効果測定

- 実施日: 2026-08-23
- 対象: `dedup-diversification` challenge（3 surface）
- 比較対象: Passage ranking適用後

## 変更

FTSの広めの候補集合をfield score・query intent・authorityで先に順位付けし、その順序を保ったまま
類似候補をcluster化する。cluster条件は次のいずれかとした。

- scope・authority role・authority statusが全て同じ
- authority role・statusが同じで、Unicode英数字へ正規化した本文が同一
- authority role・statusが同じで、本文先頭2,048正規化文字の文字trigram Jaccardが85%以上

各clusterは最上位1件だけを残し、重複候補でlimitを埋め戻さない。main・rescue・semanticの融合後にも
同じ判定を適用する。本文が同じでもroleまたはstatusが異なるcanonical・record・proposalは異なるfacetとして保持する。

## 実測

| 指標 | 変更前 | 変更後 | 差分 |
| --- | ---: | ---: | ---: |
| 対象3 surface selected recall | 0.0% | 100.0% | +100.0pt |
| 対象3 surface selected precision | 0.0% | 50.0% | +50.0pt |
| 対象3 surface avg tokens | 1,365 | 531 | -834 (-61.1%) |
| challenge全体 selected recall | 83.3% | 100.0% | +16.7pt |
| challenge全体 selected precision | 19.7% | 37.3% | +17.5pt |
| challenge全体 avg documents | 5.83 | 4.50 | -1.33 |
| challenge全体 avg tokens | 1,922 | 1,558 | -364 (-18.9%) |
| control selected recall | 100.0% | 100.0% | ±0 |
| control selected precision | 72.4% | 73.3% | +0.9pt |
| control avg tokens | 857 | 772 | -85 (-10.0%) |

5件の同一`Mercury Program Digest`は最上位1件へまとまり、6位だった
`notes/mercury-program-risk-facet`が全surfaceで選択された。control・challengeの全caseがPASSし、
このsynthetic suiteに残るexpected failureは0件になった。
