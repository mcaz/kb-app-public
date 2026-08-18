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

note IDの重複、適合と除外の競合、DBに存在しないID、deprecatedノートは評価開始前に拒否する。
実KB由来のqueryとnote IDをsource repositoryへcommitしない。リポジトリ内のexampleは形式説明用の
synthetic dataであり、そのまま実行するfixtureではない。

## 実行

```bash
kb --vault <name> eval retrieval --cases /private/path/golden.json
kb --vault <name> eval retrieval --cases /private/path/golden.json --format json
kb --vault <name> eval retrieval --cases /private/path/golden.json \
  --format markdown --output /private/path/report.md
```

既定出力はMarkdown。`--format json`は候補ID、選択ID、hop、劣化、上限到達を含む完全な
機械可読レポートを返す。両形式とも、共有した検索条件と各戦略のseed／hop／候補／本文／token
上限も記録するため、既定値が後で変わっても過去結果を解釈できる。どちらもノート本文は含めない。

## 指標

- candidate recall: `required`のうち候補集合へ入った割合
- selected recall: `required`のうち本文選択された割合
- selected precision: 選択本文のうち`required`または`relevant`だった割合
- excluded violations: `excluded`が本文選択された件数
- selected depth: 0／1／2ホップと被リンクの採用数
- cost: 本文数、推定token、12,000 token spill、候補・本文・予算上限、p50／p95時間

集計はcaseごとのmacro average。JSONとMarkdownの両方に`linked_v1 - top3`の差を残す。

## 範囲外

この評価器は発話本文を自動記録せず、外部analyticsへ送信しない。回答自体の正確さ、追加の
MCP `get`／`search`、手戻り、利用者評価を比べるend-to-end A/Bは後続段階とする。
