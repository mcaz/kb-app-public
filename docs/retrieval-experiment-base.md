# Retrieval profile／context index 実験の共通base(B0)

- 作成: 2026-08-28
- 起点: `origin/main` `91b9bf28a782676678aaff3f049349e2a69fd187`(PR #100〜#106 merge 後)
- 実験契約: [claude-led-retrieval-discussion.md](claude-led-retrieval-discussion.md) §8〜§10、
  [codex-led-retrieval-discussion.md](codex-led-retrieval-discussion.md)
  (どちらも `mcaz/*-retrieval-experiment-design` branch 上の文書。本書は両者が合意した §8.1 の base を実装したもの)
- 性質: **挙動不変の計測基盤**。配信 profile・rerank・派生索引の実装は各 experiment branch が行う。
  本書の数値は base 凍結時の baseline であり、以後の branch はこの表との差分で判定する

## 0. 前提と劣化(要報告)

- kb-app MCP `search` は本タスクでも `fts_main` 欠落(`no such table: fts_main`)で失敗し、
  UserPromptSubmit hook の自動 retrieval も劣化を返した。AGENTS.md が求める KB での過去決定の確認は
  実施できていない。本書と実装は repo 正本(contract / requirements / ADR / docs/retrieval-*.md /
  kb-core)だけに基づく。KB 側に矛盾する運用決定があれば、各 branch の実装前に再確認が要る。
- 合成 benchmark は一時 Vault を作るので実 vault の劣化の影響を受けない。

## 1. base が足したもの(挙動不変)

| 場所 | 変更 |
| --- | --- |
| `schemas/retrieval-benchmark.schema.json` | 1.1.0。`schema_version: "1.0.0" \| "1.1.0"`、`profiles: [session_auto \| session_explicit \| routine_auto \| evaluation]`(既定 `["session_auto"]`)、`rerank: "off" \| "on"`(既定 off)。1.0.0 では両 field を禁止し、controls / challenges を 2.0.0 に固定 |
| `schemas/retrieval-eval.schema.json` | 2.1.0。case の `family`、`queries.routine`(任意の 4 番目 surface)、`gate_mode: "selected" \| "candidates"`(既定 selected)。2.0.0 では 3 つとも禁止 |
| `crates/kb-core/src/retrieval_profile.rs`(新規) | `RetrievalProfile`(4 種)と `RerankMode` の名前だけ。全 profile が `RetrievalOptions::default()` へ解決し、`session_auto` = `evaluation` を test で固定。差を付けるのは profile branch |
| `crates/kb-core/src/retrieval_eval.rs` | `EvaluationStrategy::Profile { profile, rerank }`、`EvaluationPlan`(`top3` / `linked_v1` + profile 列)、`EvaluationSurface::Routine`、`GateMode`、report の `families`(family × strategy 集計)、`required_rank`(候補順で最初の required が出る位置、1 始まり)、`tokens_per_required`(選択本文 token ÷ 選択された required 件数)、strategy ごとの `gate_passed` / `gate_failed_cases`、`StrategyConfiguration` に profile / rerank / passage 値。JSON の `strategy` は従来どおり文字列 label(`profile:<name>:rerank_<off\|on>`) |
| `crates/kb-core/src/retrieval_benchmark.rs` | 1.0.0 / 1.1.0 受入、`evaluate_with(suite, BenchmarkRunOptions { profiles, rerank, fixture_digest })`。1.0.0 suite は指定を無視して従来出力。report に `suite_schema_version` / `profiles` / `rerank` / `fixture_digest` |
| `crates/kb-core/src/retrieval.rs` | `PASSAGE_RANKING_TRIGGER_TOKENS` / `PASSAGE_DOCUMENT_LIMIT` / `PASSAGE_DOCUMENT_TOKEN_BUDGET` を `pub` にしただけ(report へ写すため) |
| `kb eval retrieval-benchmark` | `--profiles a,b,c`(comma 区切り、kebab-case)と `--rerank off\|on`。1.0.0 suite に付けると stderr に注意を出して無視する。report に suite file の SHA-256 を `fixture_digest` として写す |
| `schemas/examples/retrieval-profile-context.example.json` | 新 fixture(§3)。97 note、22 case × 3 surface + routine 3 surface = 69 surface |
| `docs/retrieval-experiment-base/*.json` | base 凍結時の JSON report(§4)。`frozen_baseline_reports_match_the_current_control_suites` が google / holdout の構造一致を検出する |

base で**作らなかった**もの: profile ごとの予算・AND 検索・出力形(profile branch)、card / 二段 rerank / 低信号後回し
(context branch)、v9 索引(context branch、G2 通過後)、hook routing、session hint、群 context(§8.12)。
`kb eval vault-shape`(§8.1 項目 7)も本 base には含めていない。coordinator の task 指示に無く、実 vault の DB 復旧が
前提の read-only 統計であるため、復旧後に別 task として切り出す。

## 2. 構造一致(§6-2 / §8.1)

91b9bf2 の release binary(`target/release/kb`、変更前に build)と base の binary で google / holdout の JSON を取り、
case × surface × strategy ごとに `candidate_ids` / selected ID 列 / `gate_passed` / `body_requirements` /
`required_in_selected` / `excluded_in_selected` / `search_degraded` / `stable` を突き合わせた。

| suite | surface | 構造差分 |
| --- | ---: | ---: |
| google(1.0.0) | 33 | 0 |
| holdout(1.0.0) | 36 | 0 |

同じ一致を repo 内でも 2 つの test に固定した。

- `retrieval_benchmark::tests::frozen_baseline_reports_match_the_current_control_suites`:
  `docs/retrieval-experiment-base/{google-benchmark,realistic-holdout}.json` と現在の実行結果を上の field で比較
- `retrieval_benchmark::tests::session_auto_profile_matches_linked_v1_structure_on_control_suites`:
  google / holdout を 1.1.0 として `--profiles session_auto --rerank off` 相当で走らせ、同じ report 内の `linked_v1` と
  `profile:session_auto:rerank_off` が一致することを確認

R2-2(rerank が control 30 surface の selected 集合を 1 つでも変える)と G0 の検出器はこの 2 test で、branch 側は
`docs/retrieval-experiment-base/*.json` を更新しない限り緑にならない。

## 3. 新 fixture `retrieval-profile-context.example.json`

- 97 note(実験契約の「約 75」より多い。理由: 低信号 link / depth-2 の noise を 15 件の共有 pool `notes/common-*` に
  まとめた上で、signal note を seed 枠(上位 5)から外すために query の一般語だけを持つ decoy(deep 2 / alias 3 /
  long 3)と、historical record を seed から押し出す active な同名 companion(current-vs-history 各 1)が要ったため)
- `family` は case ごとに宣言し、report の `families` に family × strategy の集計が出る
- `stability_runs: 3`。全 69 surface で `stable: true`、`search_degraded` は空
- 語彙は family ごとに固有名詞を分け(lumen / quartz / helios / ember / tundra / meridian / kestrel / nimbus /
  zephyr / 経費・棚卸)、pool と decoy は他 family の query 語を含まないようにした。同じ 8 link を複数 seed が共有すると
  本文 trigram が 85% 以上似て契約 16 の cluster に潰され seed が消えるため、seed ごとに pool の別の 8 件を link している

| family | case | 形状 | baseline(`linked_v1` = `session_auto`) | 反転させる状態 | gate |
| --- | ---: | --- | --- | --- | --- |
| isolated-fallback | 1 | link・typed relation・scope 仲間の無い孤立 note。seed 1 件だけで候補 1 | **PASS**(全状態で必須) | — | selected |
| interference | 2 | holdout と同型の東京 / 大阪 template 近似(経費締め・棚卸)。record / active、scope 別 | **PASS**(全状態で必須)。相互の query 語(地域・記録)で他方の pair も seed に入る | — | selected |
| routine-template | 3 | 300 字級の定型 prompt(routine surface)+ 内容語 2〜3。boilerplate 語に当たる noise 2 件(定例確認テンプレート / 週次報告の書き方) | **PASS**。routine surface では 1 文字助詞まで title 一致に数える field score のため boilerplate note が seed 1 位、required は 2 位(seed 3 でも候補に残る)。routine surface の注入は 1,612〜1,632 token / 5 本文 | profile(`routine_auto`: ≤50% かつ ≤1,500 token)、combined(card) | candidates |
| explicit-precision | 3 | AND query(3 語全てを持つのは required だけ)。required は `derived_from` / `supports` の relevant 2 件と pool 8 件へ link。2 語だけ持つ OR decoy 1 件 | **PASS**。OR 検索で decoy と同 project の他 note が seed に入り、10 本文 / 3,509 token / precision 30% | profile(`session_explicit`: AND + docs 5 / depth 1 / token 6,000 で −30% 以上) | selected |
| deep-signal | 3 | seed → hub(depth 1)→ pool 15 + signal 1(depth 2)。signal は description と scope にだけ query 語(score 49)、seed 枠は完全タイトル + decoy 2 + 他 case の seed 2 が埋める | **FAIL**(9/9)。candidate recall 100%、required rank **24**、document_limit で落ちる | context(spike の群内 rerank) | selected + required rank |
| inbound-alias | 2 | seed が pool 8 + target へ link。target の唯一の証拠は非 seed note からの被リンク anchor(`ember rollback`、query 3 語のうち 2 語なので anchor 検索は昇格しない) | **FAIL**(6/6)。required rank **14**、document_limit で落ちる | context(alias を card に載せる) | selected |
| long-heading | 2 | 約 22k token の Operations Compendium。見出し `## Tundra restore checksum verification` だけが query に一致し、title / description は一致しない。seed から pool 8 件と並んで link | **FAIL**(6/6)。required rank **14**、document_limit で落ちる。選ばれた場合は契約 15 の passage 縮約で ≤3,600 token になる | context(見出しを card に載せる) | selected + token ≤3,600 |
| current-vs-history | 2 | 同語の active canonical 1 + scope の異なる historical record 8(canonical の History 節から link)+ active な runbook 1。query に `current` / `現行` marker | **FAIL**(6/6)。intent 一致で active note が seed 5 枠を埋め(他 family の `*Policy` を含む)、record は depth 1 で 5 件選ばれ **excluded 5 / surface** | context(H2-d: intent 連動の後回し) | excluded 0 + selected |
| rationale-bundle | 2 | 「なぜ」query で decision canonical + `supports` record + `derived_from` procedure | **PASS**。3 件とも本文一致で seed に入る(precision 51%) | (通過。維持を確認) | selected |
| multi-scope-compare | 2 | 2 scope の canonical を比較する query。nimbus は本文が異なり、cobalt は末尾 1 文以外同一の template | **PASS**。どちらも契約 16 の cluster に潰されず 2 seed | (通過。維持を確認。落ちれば §4-H → 群 context) | selected |

known FAIL は `retrieval_benchmark::tests::profile_context_fixture_records_known_family_results` に family 単位で固定した。
family を反転させた branch は、この test の期待値と §4 の baseline JSON を同じ commit で更新し、
required / relevant / excluded / body_requirements は変えない(§4-D)。

## 4. baseline(base 凍結時、`core 0.0.1`)

計測 command は §5。時間は端末差が大きいので比較表には載せず、JSON にだけ残す。

### 4.1 control 2 suite(1.0.0、従来出力)

| suite | surfaces | gate | selected recall | precision | avg docs | avg tokens | spill | budget exhausted | failed |
| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| google controls | 15 | PASS | 100.0% | 73.3% | 3.00 | 772 | 0 | 0 | 0 |
| google challenges | 18 | PASS | 100.0% | 37.3% | 4.50 | 1,558 | 0 | 0 | 0 |
| holdout controls | 15 | PASS | 100.0% | 55.7% | 3.40 | 1,059 | 0 | 0 | 0 |
| holdout challenges | 21 | PASS | 100.0% | 29.8% | 6.00 | 1,792 | 0 | 0 | 0 |

91b9bf2 と同値([retrieval-realistic-holdout.md](retrieval-realistic-holdout.md) の全施策 ON 列と一致)。

### 4.2 新 suite(`--profiles session_auto --rerank off`)

| suite | surfaces | gate | candidate recall | selected recall | precision | excluded | avg docs | avg tokens | tokens / required | required rank p50 | failed |
| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| controls | 9 | PASS | 100.0% | 100.0% | 63.3% | 0 | 3.33 | 1,234 | 664 | 1 | 0 |
| challenges | 60 | FAIL | 100.0% | 65.0% | 34.1% | 30 | 8.05 | 2,888 | 1,996 | 1 | 27 |

family 別(`linked_v1`。`session_auto` / `session_explicit` / `routine_auto` は base では全て同値):

| family | surfaces | gate | failed | candidate recall | selected recall | precision | excluded | avg docs | avg tokens | tokens / required | required rank p50 |
| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| isolated-fallback | 3 | PASS | 0 | 100.0% | 100.0% | 100.0% | 0 | 1.00 | 284 | 284 | 1 |
| interference | 6 | PASS | 0 | 100.0% | 100.0% | 45.0% | 0 | 4.50 | 1,710 | 855 | 1 |
| routine-template | 12 | PASS | 0 | 100.0% | 100.0% | 50.8% | 0 | 4.42 | 1,440 | 1,440 | 1 |
| explicit-precision | 9 | PASS | 0 | 100.0% | 100.0% | 30.0% | 0 | 10.00 | 3,509 | 3,509 | 1 |
| deep-signal | 9 | FAIL | 9 | 100.0% | 0.0% | 20.0% | 0 | 10.00 | 4,283 | — | 24 |
| inbound-alias | 6 | FAIL | 6 | 100.0% | 0.0% | 10.0% | 0 | 10.00 | 3,488 | — | 14 |
| long-heading | 6 | FAIL | 6 | 100.0% | 0.0% | 10.0% | 0 | 10.00 | 3,524 | — | 14 |
| current-vs-history | 6 | FAIL | 6 | 100.0% | 100.0% | 20.0% | 30 | 10.00 | 3,160 | 3,160 | 1 |
| rationale-bundle | 6 | PASS | 0 | 100.0% | 100.0% | 51.0% | 0 | 7.00 | 2,378 | 793 | 1 |
| multi-scope-compare | 6 | PASS | 0 | 100.0% | 100.0% | 73.3% | 0 | 4.67 | 1,758 | 879 | 1 |

routine surface だけの注入(G1 の `routine_auto` 判定に使う値): routine-template-01 1,612 token / 5 本文、
02 1,632 / 5、03 1,626 / 5。required rank はいずれも 2。`session_auto` 比 ≤50% の目標は約 810 token 以下。

`--profiles session_auto,session_explicit,routine_auto --rerank off` の 3 profile matrix
(`docs/retrieval-experiment-base/profile-context-matrix-off.json`)は、全 69 surface × 3 profile で
`linked_v1` と候補列・選択・gate が一致した(差分 0)。fixture digest(SHA-256)は
`b3f2743be28d191cbcd9380428e74ca11e884f7cae7af407169189ce5091921c`。

### 4.3 10k 性能 gate(release、3 回)

`cargo test --release -p kb-core search::tests::ten_thousand_note_performance_gate -- --ignored --exact --nocapture --test-threads=1`。
base は索引・検索経路(`index.rs` / `search.rs` / `retrieval.rs` の本体)を変えていないので、正典の baseline は
coordinator が **clean な `origin/main` 91b9bf2 を同一端末の無負荷時に 3 回**測った値とする。

| 計測 | DB 初回復元(ms) | 判定 |
| --- | ---: | --- |
| main 91b9bf2 run 1 | 15,693 | PASS |
| main 91b9bf2 run 2 | 15,566 | PASS |
| main 91b9bf2 run 3 | 15,756 | PASS |
| **中央値(正典 baseline)** | **15,693** | 30,000 ms 予算内 |

本 branch(worktree)で取った 3 回は、同じ端末で他 session の並列 build が走り load average が 27〜30 の状態
(`uptime`: 29.82 29.46 27.47、16 users)だったため **並列負荷による汚染**として記録だけ残し、baseline には使わない。

| 計測(本 branch、負荷あり) | DB 初回復元(ms) | 判定 |
| --- | ---: | --- |
| run 1 | 20,641 | PASS(他は home 20 回 329 / カテゴリ 20 回 113 / 一覧 5 回 432 / 全文検索 100 回 13 / KNN 10 回 229 / 詳細 5 回 120 / 連鎖 20 回 2 ms) |
| run 2 | 59,450 | FAIL(予算 30,000 ms 超) |
| run 3 | 34,004 | FAIL(予算 30,000 ms 超) |

各 experiment branch の 10k 計測は、この正典 baseline と同じ条件(clean な端末、3 回、中央値)で取る。
負荷下の値は G2 の +10% 判定に使えない。

## 5. 計測手順(各 branch で同一)

```bash
cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
cargo build --release -p kb-cli
target/release/kb eval retrieval-benchmark --suite schemas/examples/retrieval-google-benchmark.example.json \
  --format json --output /private/path/<branch>-google.json
target/release/kb eval retrieval-benchmark --suite schemas/examples/retrieval-realistic-holdout.example.json \
  --format json --output /private/path/<branch>-holdout.json
target/release/kb eval retrieval-benchmark --suite schemas/examples/retrieval-profile-context.example.json \
  --profiles session-auto --rerank off \
  --format json --output /private/path/<branch>-suite-session-auto.json
target/release/kb eval retrieval-benchmark --suite schemas/examples/retrieval-profile-context.example.json \
  --profiles session-auto,session-explicit,routine-auto --rerank off \
  --format json --output /private/path/<branch>-suite-off.json
# context / combined branch のみ --rerank on を追加で 1 回
cargo test --release -p kb-core search::tests::ten_thousand_note_performance_gate -- \
  --ignored --exact --nocapture --test-threads=1   # 3 回、中央値を表へ
```

- report の比較軸: `profiles` / `rerank` / `fixture_digest` / `core_version` と、各 `families[].summaries[]`。
  coordinator は branch ごとの JSON を「状態 × family × 指標」の 1 表へ集計する。core commit は report に埋め込まず、
  表の行に添える(build 時の環境変数に頼ると stale になる)
- `gate_mode: candidates` の family(routine-template)は required が候補集合に入れば PASS で、本文要件は報告のみ
  (本文 0 件の routine 変種 A を測るため)

## 6. 既知の制約・逸脱

- note 数 97(契約の「約 75」より多い。理由は §3)
- 3 surface の query は多くの family で同文。family の反転条件を surface 間で揺らさないためで、
  holdout のように言い回しを変えた family は routine / current-vs-history / rationale / multi-scope だけ
- current-vs-history で historical record を seed から押し出しているのは、`current` marker の intent 一致が
  active note を優先する現行仕様(`search.rs` `rank_hits`)であり、他 family の `*Policy` canonical も seed に入る。
  family の判定(excluded 0)には影響しないが、precision の絶対値は fixture 依存
- routine surface で required が 2 位になるのは、field score が 1 文字の助詞(「の」等)まで title 一致に数えるため。
  現行仕様の記録であり、base では直さない(profile branch が `routine_auto` を測るときの前提)
- 本 worktree では Tauri crate の `build.rs` が要求する git-lfs sidecar を network 制限で取得できず
  (`scripts/prepare-git-lfs.mjs` が github.com へ届かない)、`cargo clippy --workspace` /
  `cargo test --workspace` は `TAURI_CONFIG='{"bundle":{"externalBin":[]}}'` を付けて実行した
  (repo のファイルは触らない)。kb-core / kb-cli の検査には影響しない
- `github::tests::private_gate_*` 4 件は sandbox が `TcpListener::bind` を拒否して落ちる(環境起因、CI で判定)。
  本 base のコード変更とは無関係
