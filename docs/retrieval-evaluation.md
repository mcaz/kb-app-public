# Retrieval効果測定

リンク連鎖retrievalの品質とコストを、旧固定3本文方式と同じqueryで比較するための
ローカル評価器。実装は`crates/kb-core/src/retrieval_eval.rs`、CLIは`kb eval retrieval`。

## 比較する戦略

- `top3`: 検索上位3件だけを本文として選ぶ評価専用baseline。製品設定には公開しない
- `linked_v1`: 上位5件をseedに、出リンク最大2ホップとseedへの被リンクを辿り、最大50候補、
  推定10,000 token、最大10本文で選ぶ本番方式

検索は1ケースにつき1回だけ実行し、その順位を両戦略へ渡す。評価中は同じSQLite read
transactionを使うため、途中の更新で比較対象がずれない。

## Golden Query

入力は[JSON Schema](../schemas/retrieval-eval.schema.json)に従う。例は
[retrieval-eval.example.json](../schemas/examples/retrieval-eval.example.json)。

- `required`: 回答根拠として取得される必要があるノート。precisionでも適合扱い
- `relevant`: 追加で取得されてもよいノート。`required`との重複は禁止
- `excluded`: 選ばれると誤りになるノート
- `queries`: `codex`、`claude_code`、`chatgpt`それぞれの固定prompt。3 surfaceを必ず同じ
  snapshotで評価する
- `body_requirements`: 選択本文に必要な意味を決定的な語句契約で検証する。`all_terms`はすべて、
  `any_terms`は1つ以上が選択本文集合に含まれる必要がある
- `stability_runs`: 同一read snapshotでの反復回数（1〜10）。候補ID、選択ID、本文要件、
  degradationのいずれかが揺れるとgateを失敗させる

note IDの重複、適合と除外の競合、DBに存在しないID、deprecatedノートは評価開始前に拒否する。
実KB由来のqueryとnote IDをsource repositoryへcommitしない。リポジトリ内のexampleは形式説明用の
synthetic dataであり、そのまま実行するfixtureではない。

## 実行

```bash
kb --vault <name> eval retrieval --cases /private/path/golden.json
kb --vault <name> eval retrieval --cases /private/path/golden.json --format json
kb --vault <name> eval retrieval --cases /private/path/golden.json \
  --format markdown --output /private/path/report.md
kb --vault <name> eval retrieval --cases /private/path/golden.json --gate
```

既定出力はMarkdown。`--format json`は候補ID、選択ID、hop、劣化、上限到達を含む完全な
機械可読レポートを返す。両形式とも、共有した検索条件と各戦略のseed／hop／候補／本文／token
上限も記録するため、既定値が後で変わっても過去結果を解釈できる。どちらもノート本文は含めない。
本文要件の結果には要件ID、合否、根拠になった選択note IDだけを残す。`--gate`を付けると、
`linked_v1`でrequired未選択、excluded選択、本文要件不足、本文欠落、search degradation、または
反復不安定が1件でもあれば、レポート出力後に非0で終了する。

## 指標

- candidate recall: `required`のうち候補集合へ入った割合
- selected recall: `required`のうち本文選択された割合
- selected precision: 選択本文のうち`required`または`relevant`だった割合
- excluded violations: `excluded`が本文選択された件数
- selected depth: 0／1／2ホップと被リンクの採用数
- cost: 本文数、推定token、12,000 token spill、候補・本文・予算上限、p50／p95時間
- gate: surfaceごとの本文意味要件、missing documents、degradation、反復安定性

集計はcaseごとのmacro average。JSONとMarkdownの両方に`linked_v1 - top3`の差を残す。

## Google型改善の合成benchmark

実KB由来のqueryをcommitせず、field別ranking、query intent、anchor text、typed relation
ranking、passage ranking、重複排除・多様化の改善前後を固定条件で測るため、
[実行可能な合成suite](../schemas/examples/retrieval-google-benchmark.example.json)を用意した。
形式は[benchmark schema](../schemas/retrieval-benchmark.schema.json)に従う。

```bash
kb eval retrieval-benchmark \
  --suite schemas/examples/retrieval-google-benchmark.example.json
kb eval retrieval-benchmark \
  --suite schemas/examples/retrieval-google-benchmark.example.json \
  --format json --output /private/path/baseline.json
```

コマンドは44件のsynthetic noteを一時Vaultへ作り、終了時に破棄する。既定vault、実KB、
外部analyticsは使わない。suiteは次の2集合を同じ検索・評価実装へ渡す。

- `controls`: 現行で守る5ケース。完全タイトル、日本語2文字語、日本語部分語、1-hop link、
  active canonicalを3 surfaceで測り、gateはPASSでなければ回帰
- `challenges`: 改善対象6ケース。現行でFAILすること自体をgateにせず、改善前後のrecall、
  precision、token、上限到達、失敗surfaceを比較する

`core 0.0.1`での初期baselineは次のとおり。時間は端末差が大きいためbaseline契約に含めない。

| suite | surfaces | linked selected recall | linked precision | avg tokens | spill | budget exhausted | failed surfaces |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| controls | 15 | 100.0% | 73.7% | 806 | 0 | 0 | 0 |
| challenges | 18 | 16.7% | 18.3% | 5,684 | 3 | 3 | 15 |

challengeの15 failureは、field別ranking、historical intent、anchor text、typed relation ranking、
重複排除・多様化の各3 surface。passage rankingケースはrequired本文を取得できる一方、巨大本文を
丸ごと選ぶため3 surfaceともspill／budget exhaustedになる。各改善PRでは同じJSON reportを保存し、
このsemantic baselineとの差分を示す。controlの低下は回帰、challengeのrecall／precision改善と
token／spill低下は効果として扱う。

個別実験:

- [Field別ランキング](retrieval-field-ranking.md)
- [Query intentランキング](retrieval-query-intent.md)
- [アンカーテキスト索引](retrieval-anchor-text.md)
- [Typed relation重み付き伝播](retrieval-typed-relation-ranking.md)
- [Passage ranking](retrieval-passage-ranking.md)
- [重複排除・多様化](retrieval-dedup-diversification.md)
- [実運用風holdoutでの全施策比較](retrieval-realistic-holdout.md)
- [Retrieval profile／context index実験の共通base](retrieval-experiment-base.md)

## 配信profile × rerankの比較軸(benchmark 1.1.0 / Golden Query 2.1.0)

配信profile(`session_auto` / `session_explicit` / `routine_auto` / `evaluation`)とquery-aware rerankの
実験を同じfixtureで比べるため、benchmark suiteとGolden Queryの形式を後方互換で広げた。google / holdoutの
2 suiteは1.0.0のまま凍結し、`kb eval retrieval-benchmark`に`--profiles` / `--rerank`を付けても
stderrへ注意を出して無視する(従来出力)。

```bash
kb eval retrieval-benchmark \
  --suite schemas/examples/retrieval-profile-context.example.json \
  --profiles session-auto,session-explicit,routine-auto --rerank off \
  --format json --output /private/path/suite-off.json
```

- benchmark 1.1.0: `profiles`(既定`["session_auto"]`)と`rerank`(既定`off`)。CLI flagがsuiteの値より優先する。
  1.1.0 suiteのreportには`top3` / `linked_v1`に加えて`profile:<name>:rerank_<off|on>`のstrategyが並ぶ
- Golden Query 2.1.0: caseの`family`(family × strategyの集計軸)、`queries.routine`(定型routine promptを模した
  4番目のsurface、任意)、`gate_mode`(`selected`は required全件の本文選択、`candidates`は required全件の候補化で
  合格。`candidates`では本文要件を報告だけに留める)
- 追加指標: `required_rank`(候補順で最初のrequiredが出る位置、1始まり。予算に依存しない順位指標)と
  `tokens_per_required`(選択本文の推定token ÷ 選択されたrequired件数)。summaryではそれぞれ中央値と平均を出し、
  strategyごとの`gate_passed` / `gate_failed_cases`も持つ
- 実験baseでは全profileが現行の`RetrievalOptions::default()`へ解決し、`session_auto`は`linked_v1`と候補列・選択・
  gateが一致する(`crates/kb-core/src/retrieval_profile.rs`)。profileごとの予算・AND検索・rerankは各実験branchが
  実装し、[共通base](retrieval-experiment-base.md)の表との差分で判定する

## 範囲外

この評価器は発話本文を自動記録せず、外部analyticsへ送信しない。本文要件は意味を表す
ローカルな語句契約であり、LLMによる自由判定ではない。回答自体の正確さ、追加の
MCP `get`／`search`、手戻り、利用者評価を比べるend-to-end A/Bは後続段階とする。
