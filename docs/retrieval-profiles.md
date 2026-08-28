# 配信profileの分離と効果測定

- 実施日: 2026-08-28
- 起点: `origin/main` `91b9bf2`（PR #100〜#106 merge後）
- 実験契約: [claude-led-retrieval-discussion.md](claude-led-retrieval-discussion.md) §8.4（初期値）/
  §8.8 G0・G1（gate）/ §9 `*/retrieval-profile`（scope）
- branch: `mcaz/claude-retrieval-profile`（Claude主体の案1単独実装。他主体のbranchは読んでいない）
- KB参照の劣化: kb-app MCP `search`は`no such table: fts_main`で失敗したままで、UserPromptSubmit hookの
  自動retrievalも同じ劣化を返した。本書はrepo正本（契約 / 実験契約 / kb-core）だけに基づく

## 統合coreでの既定の変更（2026-08-28 R4 I-2）

本branchはhost既定を`session_explicit`にして測ったが、統合core（`integration/retrieval-core`）では
**host既定を`session_auto`**（= 現行の候補展開予算そのもの）にする。rationale:

- 既定profileには「平均token改善」より「必要候補を落とさない」を優先する。`session_explicit`は
  token −44.8%（G1）を出す一方、holdout challengeでrequired candidate recall 85%
  （`notes/helios-migration-review-2024`が候補集合から落ちる2 surface）の既知回帰がある
- `session_explicit`は削除せず、明示選択（`--retrieval-profile session-explicit` / `kb search --profile`）
  でのみ有効。未知profileのfail-closed・tool schema非露出・hook子processの
  `--retrieval-profile session-auto`明示は本branchの実装のまま
- 既定変更を再判断する条件（別PR）: 実KB評価でrequired candidate recall低下0・excluded増加0・
  token改善が複数query familyで再現・surface別の最低値でもgate通過を同時に満たすこと

以下の本文は実験時点（host既定=`session_explicit`）の記録。routingの現状は上記が優先する。

## 変更

同じ`search` toolでも、管理hookが発話ごとに自動で引く経路と、host側でmodelが明示的に呼ぶ経路とでは
許容できる本文量と求める精度が違う。呼び出し用途ごとの予算と出力形を**配信profile**
（`crates/kb-core/src/retrieval_profile.rs`）に集約し、process起動時に固定する。queryの内容は見ない
（queryの意味で順位を変える`QueryIntent`とは別の軸）。

| profile | any | limit | seed | depth | cand | docs | token | incoming | passage（trigger / 件 / token） | 出力 | 用途 |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | --- | --- |
| `session_auto` | true | 5 | 5 | 2 | 50 | 10 | 10,000 | true | 4,000 / 3 / 3,600 | 本文 | 管理hookの子process（契約8の数値そのもの） |
| `session_explicit` | 引数（既定false） | 8 | 5 | 1 | 20 | 5 | 6,000 | true | 4,000 / 2 / 2,400 | 本文 | host側MCP `search`の実験変種（統合coreでは明示選択のみ） |
| `routine_auto` | true | 5 | 3 | 1 | 20 | 3（変種A: 0） | 3,000 | true | 2,000 / 1 / 1,200 | card-lite + 本文≤3 | benchmark上の変種のみ |
| `evaluation` | = `session_auto`（同じ計画を返す。一致は`retrieval_profile.rs`のtestで固定） | | | | | | | | | | `kb eval retrieval` / benchmarkの`linked_v1` |

- `SearchPolicy`（any / limit / field ranking候補倍率 8 / rescue / semantic / diversify）、
  `PassagePolicy`（passage rankingの4定数）、`OutputShape`（`body` / `card_lite`）を`RetrievalPlan`にまとめた。
  `search` / `search_mode`は`search_with(conn, query, &SearchPolicy)`のwrapper、`RetrievalOptions`は
  `PassagePolicy`を持つ。どのprofileも検索経路（rescue / semantic / diversify）と倍率は現行のまま
- `session_explicit`の`include_incoming`はtrue（§6-1: `supports` / `derived_from`は「根拠 → 正本」の
  被リンク方向が自然で、切ると根拠recordを候補からも落とす。depth 1に絞るだけにする）
- `RetrievalCandidate`にauthority（namespace / role / status / scope）を足した（card-liteの素材。envelopeの
  無いlegacy noteでは出力しない）。`OutputShape::CardLite`はこのroundでは応答の組み立てを変えず、
  routine変種のラベルと表示にだけ使う（routine routing・card注入は§8.12の範囲外）
- routing: 起動引数`--retrieval-profile`（`kb mcp` / `kb-app --mcp`）> host既定（実験時点は
  `session_explicit`、**統合coreでは`session_auto`** — 冒頭の節）。未知値はfail-closed
  （`kb-app --mcp`はexit 2、`kb mcp`はエラー終了）。管理hookの子processは
  `child_mcp_args`で`--retrieval-profile session-auto`を明示する（`ai_guard.rs`のhook登録引数は不変）。
  環境変数・markerによる`routine_auto`の自動選択は実装しない（§6-3）
- 表示: initializeの`capabilities.experimental.kbApp.retrieval_profile`と、`search`応答の
  `structuredContent.retrieval_profile`。tool入力schemaは不変。`kb search --profile`も同じ値を受ける
- 契約変更なし（契約8はhook経路だけを縛る。§6-4）。`docs/contract.md`・`contract_guard.rs`は触っていない
- host経路の実挙動変更は1点だけ: read / all面の`search(include_documents)`が`RetrievalOptions::default()`
  （= `session_auto`）から`session_explicit`の予算へ移る。hook経路はbyte同一（下記）。
  **統合coreではこの1点を採らず**、host既定=`session_auto`により候補展開は現行予算のまま
  （`search`の引数省略時既定はprofileに従いlimit 5 / OR結合になる。tool引数`limit` / `any`での
  上書きは従来どおり）

## 現行挙動の保証（G0 / G1: `session_auto` = `evaluation`）

`session_auto`（= `evaluation`）で既存2 suiteを`kb eval retrieval-benchmark --format json`で測り、
起点`91b9bf2`の同じcommandのreport（同一端末・debug build）と構造一致で比較した。比較はcase × surface × 戦略
ごとの`candidate_ids` / `selected`（id・source・depth・token）/ `gate_passed` / `body_requirements` /
`search_degraded` / `stable`と、`linked_v1`の集計・gate・戦略設定（μs系だけ除外）。

| suite | linked_v1 surface | 構造差分 | control gate | challenge gate | control selected recall / precision | challenge selected recall / precision | avg docs / tokens（control / challenge） |
| --- | ---: | ---: | --- | --- | --- | --- | --- |
| google 44 note | 33（15 + 18） | 0 | PASS | PASS | 100.0% / 73.3% | 100.0% / 37.3% | 3.00 / 772・4.50 / 1,558 |
| holdout 55 note | 36（15 + 21） | 0 | PASS | PASS | 100.0% / 55.7% | 100.0% / 29.8% | 3.40 / 1,059・6.00 / 1,792 |

すべて起点と同値（excluded violation 0・spill 0）。制御30 surfaceの選択集合は不変（G0）。
`session_auto` = `evaluation`の計画一致、`session_auto`の計画が`RetrievalOptions::default()`と
passage rankingの4定数に等しいこと、hook経路（`session_auto`）のMCP応答がprofile分離前の経路
（`search_mode(any, 5)` + `RetrievalOptions::default()`）とhits・候補・本文で一致することはtestで固定した
（`retrieval_profile.rs` / `mcp.rs::hook_profile_search_matches_the_pre_profile_default_path`）。

## `session_explicit` / `routine_auto` のpreview（既存2 suite）

正式なmatrix（`--profiles` / `rerank`、新fixture）は共通base待ちなので、既存2 suiteの各surfaceを
profileの`SearchPolicy` + `RetrievalOptions`でそのまま引いた**preview**を載せる（`retrieval_benchmark.rs`の
`profile_preview_on_existing_suites`、`--ignored`。precisionはreportと同じmacro定義、card tokensは
`retrieval_candidates`のJSONを2 byte/tokenで見積もった暫定値）。gate判定ではない。

| suite / group | profile | surface | required selected | required candidates | excluded | precision | avg docs | avg tokens | 対`session_auto` tokens | card tokens |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| google control | `session_auto` | 15 | 15/15 | 15/15 | 0 | 73.3% | 3.00 | 772 | — | 418 |
| google control | `session_explicit` | 15 | 15/15 | 15/15 | 0 | 100.0% | 1.40 | 359 | 46.5% | 196 |
| google control | `routine_auto` B（本文≤3） | 15 | 12/15 | 15/15 | 0 | 73.3% | 1.80 | 466 | 60.4% | 316 |
| google control | `routine_auto` A（本文0） | 15 | 0/15 | 15/15 | 0 | — | 0.00 | 0 | 0% | 347 |
| google challenge | `session_auto` | 18 | 18/18 | 18/18 | 0 | 37.3% | 4.50 | 1,558 | — | 695 |
| google challenge | `session_explicit` | 18 | 18/18 | 18/18 | 0 | 59.4% | 2.50 | 1,058 | 67.9% | 494 |
| google challenge | `routine_auto` B | 18 | 18/18 | 18/18 | 0 | 47.2% | 2.50 | 898 | 57.6% | 654 |
| google challenge | `routine_auto` A | 18 | 0/18 | 18/18 | 0 | — | 0.00 | 0 | 0% | 698 |
| holdout control | `session_auto` | 15 | 18/18 | 18/18 | 0 | 55.7% | 3.40 | 1,059 | — | 463 |
| holdout control | `session_explicit` | 15 | 18/18 | 18/18 | 0 | 90.0% | 1.80 | 556 | 52.5% | 243 |
| holdout control | `routine_auto` B | 15 | 18/18 | 18/18 | 0 | 60.0% | 2.40 | 732 | 69.1% | 358 |
| holdout control | `routine_auto` A | 15 | 0/18 | 18/18 | 0 | — | 0.00 | 0 | 0% | 400 |
| holdout challenge | `session_auto` | 21 | 21/21 | 21/21 | 0 | 29.8% | 6.00 | 1,792 | — | 931 |
| holdout challenge | `session_explicit` | 21 | **19/21** | **19/21** | 0 | 55.7% | 2.52 | 751 | 41.9% | 481 |
| holdout challenge | `routine_auto` B | 21 | 18/21 | 21/21 | 0 | 42.9% | 3.00 | 934 | 52.1% | 900 |
| holdout challenge | `routine_auto` A | 21 | 0/21 | 21/21 | 0 | — | 0.00 | 0 | 0% | 953 |

読み取り（gateの事前登録値は§8.8。ここは既存suiteでの傾向）:

- **`session_explicit`**: control 30 surfaceでrequired selected recall 100%を保ち、tokensは−47.5%〜−53.5%、
  precisionは+34pt / +26pt。challengeでもgoogle 18 surfaceは100%を保ち−32%。holdout challengeだけ
  `holdout-historical-intent`のclaude_code / chatgpt surfaceで`notes/helios-migration-review-2024`が
  **候補集合から**落ちる（selected recall 90.5%）。切り分け（同testの診断変種）:
  - `depth 2 / cand 50`へ戻しても同じ2 surfaceが候補外のまま → 展開予算の縮小が原因ではない
  - `any=true`（OR）にすると2 surfaceは候補に戻るが、代わりに`holdout-relation-evidence` 3 surfaceと
    google `control-linked-one-hop` 3 surfaceで候補内のrequiredがdocs 5 / 6,000 tokenに押し出される
  - つまり原因はAND検索でこの2つの自然文queryがhistorical recordをseedにできないこと。profile分離前の
    host経路（AND・depth 2・cand 50）も同じ2 surfaceを取れないので、host経路の退行ではない。`any`は
    tool引数のままにしてあり、hostのmodelが文まるごと投げるときは`any: true`を指定する現行の使い方で回収できる
- **`routine_auto`**: required candidate recall 100%（4 group全部。routineのgate種別）。変種B（本文≤3）の
  注入tokenは`session_auto`比52〜69%で≤50%に届かない。変種A（本文0・card-liteのみ）は本文0で、card-liteの
  暫定見積もりは347〜953 token（≤1,500、`session_auto`比38〜53%）。≤50%の判定はroutine-template family
  （300字級の定型prompt）と、candidate一覧をcard-liteとして整形したときの実tokenで行う必要がある
- excluded violationはどのprofileでも0

## 検証

- `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`: 通過
  （app crateは`TAURI_CONFIG='{"bundle":{"externalBin":[]}}'`でgit-lfs sidecarの解決を外して検査。
  sandboxからgithub.comへ到達できず`scripts/prepare-git-lfs.mjs`が動かないため。repoの設定は変えていない）
- `cargo test`: kb-core 351 passed（`github::tests::private_gate_*` 4件はsandboxが`TcpListener::bind`を
  拒否するため`--skip`。本branchは`github.rs`を触っていない）、kb-cli 11 + 1、kb-app 19（hook子processの
  引数固定を含む）
- 追加したtest: `retrieval_profile.rs` 6件（`session_auto` = `evaluation`・表の数値・parse・candidate倍率）、
  `retrieval.rs` 2件（`PassagePolicy`の狭い予算がrequired passageを落とさない・候補のauthority）、
  `mcp.rs` 3件（host既定=`session_explicit`で1ホップ止まり・hook経路の構造一致・initialize表示）、
  `kb-cli` 1件（`--retrieval-profile` / `--profile`）、`hook_mode.rs`の引数固定を更新
- 既存suite: 上の表のとおり両suiteのcontrol / challenge gateがPASSで起点と構造一致
- 10k性能gate（release、1回、`--test-threads=1`）: PASS。index_rebuild 23,519 ms（予算30,000）、
  keyword_search_x100 17 ms、semantic_search_x10 233 ms、linked_context_x20 3 ms、note_list_x5 435 ms。
  同じ端末で並行してdebug buildが走っていたので起点の15,503 msとは比較しない（静かな環境での3回中央値は残件）

## 残件（共通base待ち）

- 共通base（`experiment/base`）: `retrieval-benchmark.schema.json` 1.1.0の`profiles` / `rerank`、
  `retrieval-eval.schema.json` 2.1.0の`queries.routine` / `gate_mode`、`EvaluationStrategy::Profile`、
  `kb eval retrieval-benchmark --profiles … --rerank off`、新fixture
  `retrieval-profile-context.example.json`（explicit-precision / routine-template / interference /
  isolated-fallback ほか）での計測。G1のexplicit-precision −30%、routine-templateの≤50%かつ≤1,500 tokenは
  このfixtureと正式runnerでしか判定できない。本branchの`RetrievalProfile::plan()`をrunnerから呼べば
  `--profiles`はそのまま乗る（変種Aは`document_limit: 0`）
- 10k gate 3回の中央値（§8.10）と1 note `upsert`計測。本branchは索引を触らないので構造上は不変
- `vault-shape`（DB復旧後）とR1-3（routine由来発話の比率）。DBは`fts_main`欠落のまま
- 稼働中MCP binary（`target/release/kb`）の更新は本worktreeからは行っていない（merge後に
  `cargo build --release -p kb-cli`）
