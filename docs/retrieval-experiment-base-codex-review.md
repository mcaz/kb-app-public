# Retrieval 実験共通 base — Codex 独立監査

- 監査日: 2026-08-28
- 対象: `96b9215be73134b4439e3bf9c221a3e7e89be920`
- 比較起点: `91b9bf28a782676678aaff3f049349e2a69fd187`
- 結論: **PASS（実装修正なし）**

## 1. 結論

`96b9215` は、配信 profile／rerank の実装を先取りせず、両案を同じ fixture・runner・report 軸で
比較する共通 base として凍結できる。schema の旧版入力、既存 Google／holdout の検索構造、
`session_auto`／`evaluation` の中立性、新 fixture 10 family の期待値、集計値、CLI、fixture digest に
blocking な不整合は見つからなかった。この監査では source・schema・fixture・baseline report を変更せず、
本書だけを追加した。

監査対象の `docs/retrieval-profile-context-experiment.md` と
`docs/codex-led-retrieval-discussion.md` は base branch には含まれないため、設計 branch の固定 commit
`5a5f176` から読んだ。base 文書が併記する最終契約は `1234807` の
`docs/claude-led-retrieval-discussion.md` §8 も補助照合した。

kb-app MCP はこのクライアントで `[kb_disabled]` を返した。指示どおり、KB の保存先や旧 vault を
別経路で探索していない。

## 2. 凍結契約との一致

| 観点 | 判定 | 監査結果 |
| --- | --- | --- |
| base の境界 | PASS | 本番 retrieval のロジック変更はなく、`retrieval.rs` は report に必要な既存3定数を `pub` にしただけ。新 profile は全て `RetrievalOptions::default()` へ解決される |
| benchmark 1.0.0 / 1.1.0 | PASS | 1.0.0 は profile／rerank field を拒否し、CLI flag は注意を出して無視する。1.1.0 は CLI > suite > default の順で matrix を解決する |
| Golden Query 2.0.0 / 2.1.0 | PASS | 2.0.0 は `family`、`gate_mode`、`queries.routine` を拒否する。2.1.0 は3 fieldを後方互換で追加する |
| strategy 軸 | PASS | `top3`／`linked_v1` を先頭に固定し、`profile:<name>:rerank_<off|on>` を追加する。profile の重複指定は決定的に除かれる |
| G0 | PASS | 旧2 suite の candidate列、selected ID列、gate、本文要件、劣化、安定性は起点と一致した |
| G1 の土台 | PASS | explicit の selected gate、routine の candidates gate、profile別予算を差し込む接続点が分離されている |
| G2 の土台 | PASS | rerank label、候補順に対する required rank、本文選択費用、family別集計を同じ report に残せる |
| G3 の土台 | PASS | profile複数指定と rerank on／off の別 runで、凍結契約の4 run matrixを表現できる |

`session_explicit` の AND 検索、`routine_auto` の出力形、query-aware rerank、card／派生索引は意図どおり
feature branch の責務である。base の strategy label、configuration、候補・本文・費用の共通 field は、
それらを同じ fixture で比較するために十分であり、base 自体に feature 挙動を入れる必要はない。

## 3. 後方互換と挙動中立性

起点 `91b9bf2` を一時 directoryへ展開して旧 `kb` を別 buildし、現在の凍結 report と独立比較した。
計時値は比較から除き、契約で固定した次の構造を case × surface × strategy ごとに照合した。

- `candidate_ids`
- selected ID 列
- `gate_passed`
- `body_requirements`
- `required_in_selected`
- `excluded_in_selected`
- `search_degraded`
- `stable`

| suite | surface | 起点との差 |
| --- | ---: | ---: |
| Google | 33 | 0 |
| realistic holdout | 36 | 0 |

旧 fixture 本体の SHA-256 も起点と現在で一致した。1.0.0 suite に
`--profiles evaluation --rerank on` を付けた実行では注意文が stderr に出て、report は
`profiles: []`／`rerank: null` の従来 strategy 構造を維持した。

新 fixture 69 surface では、`session_auto` と `evaluation` の候補列・本文列・gate・本文要件・
required／excluded・rank・token統計が全件一致した。`session_auto --rerank on` も base では
`linked_v1` と意味構造が全件一致し、rerank flag が実装前に挙動差を作っていない。

## 4. 新 fixture 10 family

fixture は97 note、22 case、69 surfaceで、全 surfaceが `stable: true`、検索劣化なしだった。
各 family は異なる固有語を使い、共通 pool は仮説語を持たない。known PASS／FAIL は実装案の都合で
期待値を動かさず、維持または反転を判定できる形になっている。

| family | surface | gate | baseline | 監査所見 |
| --- | ---: | --- | --- | --- |
| isolated-fallback | 3 | selected | PASS | 孤立 note の global fallback を直接検出する |
| interference | 6 | selected | PASS | 東京／大阪の近似 template 2組を両方 required とし、cluster／rerank の取りこぼしを検出する |
| routine-template | 12 | candidates | PASS | 3 caseだけ routine surfaceを持つ。本文0件変種を許しつつ required の候補脱落は許さない |
| explicit-precision | 9 | selected | PASS | required 1 + relevant 2 + OR noise／低信号linkで、AND化と縮約の両方を測る |
| deep-signal | 9 | selected | FAIL | candidate recall 100%、required rank 24、selected recall 0%。depth-2 rerank の反転対象として明瞭 |
| inbound-alias | 6 | selected | FAIL | candidate recall 100%、required rank 14。非seed被リンクanchorを使う案の反転対象になる |
| long-heading | 6 | selected | FAIL | candidate recall 100%、required rank 14。見出し評価と passage 3,600 token上限を別々に確認できる |
| current-vs-history | 6 | selected + excluded | FAIL | required は選べる一方、historical recordが計30件選択される。excluded 0への反転を直接測れる |
| rationale-bundle | 6 | selected | PASS | decision／supports record／derived_from procedure の3件同時回収を維持gateにできる |
| multi-scope-compare | 6 | selected | PASS | 異なる2 scopeと近似templateの2 caseがあり、群 contextを不要にできるかの維持gateになる |

`required`／`relevant`／`excluded` の競合、未知 ID、deprecated、body requirement の空条件は
評価前に拒否される。candidate gateでも excluded、missing document、検索劣化、反復不安定は
strategy gateを落とすため、routineだけを不当に緩める形にはなっていない。

## 5. 指標・集計・CLI

- `required_rank` は候補列で最初の required が出る1始まりの位置で、候補に無い場合は `null`。
  summaryは存在する値の中央値と欠落surface数を併記するため、欠落を良い順位に見せない。
- `tokens_per_required` は選択本文の推定token合計を、選択された required 件数で割る。requiredを
  1件も選ばない surfaceは `null` とし、summaryは平均に使ったsurface数を別 fieldで持つ。
- family集計は fixture の初出順を保ち、各 strategyについて full summaryと同じ式を使う。
- routine surfaceは明示された caseにだけ追加され、通常3 surfaceの順序を変えない。
- `--profiles` は comma 区切りの kebab-case、suite／report は snake_caseで、変換と未知値拒否を
  CLI unit testが固定する。`--rerank` は on／offを1 runに1値持ち、off／on比較は同じfixtureの2 runで行う。
- profileごとの gate は `StrategySummary.gate_passed`／`gate_failed_cases` に残る。report全体の
  `gate` は従来どおり `linked_v1` なので、feature判定では必ず strategy／family summaryを使う。

## 6. digest・report決定性・artifact

fixture digestは入力JSONの生byte列に対するSHA-256で、CLIが読んだ同じbyte列から計算される。

| fixture | SHA-256 |
| --- | --- |
| Google | `6dd8b38422b0101b0b66bc3052beec1d9355a38a948125e22a1dfb3fb89925c3` |
| realistic holdout | `2228c1afc16d1392193a811c2436350ae5e69e76df328448057136219012a3aa` |
| profile／context | `b3f2743be28d191cbcd9380428e74ca11e884f7cae7af407169189ce5091921c` |

新 fixtureの3 profile matrixを2回実行した。JSON byte列は計時 fieldのため一致しないが、
`search_elapsed_us`／`retrieval_elapsed_us`／`total_elapsed_us`／p50／p95を除いたJSONは一致し、
保存済み `profile-context-matrix-off.json` とも一致した。これは最終契約が byte一致ではなく
構造一致を採用した理由どおりで、candidate順、selected順、family順、gate、本文要件、指標は決定的である。

追加 artifact は合計約2.4 MiB、最大は3 profileの完全JSON約1.2 MiBだった。候補列まで含む
比較証跡として過大ではなく、実KB／実発話／ノート本文を含まず synthetic fixtureだけなので、
削除・LFS化が必要な巨大 artifactとは判断しなかった。

## 7. 既知の非blocking事項

- 新 fixtureは契約の「約75 note」より多い97 noteだが、depth-2 noise、異なるpool、historical 8件を
  同時に成立させる理由が base 文書に記録され、familyの独立性を損なっていない。
- `kb eval vault-shape` は最終契約案にあったが、base taskの対象外として明示的に見送られている。
  今回の synthetic fixture／runner／schema比較を妨げない。
- reportの経過時間は本質的に非決定的である。比較時は同一端末・同一buildを使い、意味構造の
  凍結testと分離する現在の扱いが妥当である。
- 10k release gateの再計測は依頼どおり行っていない。

## 8. 検証結果

| 検証 | 結果 |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --workspace --all-targets -- -D warnings` | git-lfs sidecar不在でTauri buildが停止（既知の環境条件） |
| `TAURI_CONFIG='{"bundle":{"externalBin":[]}}' cargo clippy --workspace --all-targets -- -D warnings` | PASS |
| `cargo test -p kb-core retrieval` | PASS（28件） |
| `cargo test -p kb-cli retrieval_benchmark` | PASS（2件） |
| `TAURI_CONFIG='{"bundle":{"externalBin":[]}}' cargo test --workspace` | PASS（kb-core 354件、app 19件、kb-cli 11件、補助binary 1件。意図された ignored 2件） |
| 旧 `91b9bf2` binaryとの Google／holdout 構造比較 | PASS（差0） |
| 新 fixture重要gate／3 profile matrix／rerank on | PASS（known PASS／FAIL、base同値を確認） |
| 新 fixture report 2回 + 保存済みbaselineの意味構造比較 | PASS |

`npm --prefix app run check` も試したが、このworktreeには `prettier`（`app/node_modules`）がなく
開始前に停止した。frontend変更はなく、今回指定された cargo／retrieval検証の判定には含めていない。
