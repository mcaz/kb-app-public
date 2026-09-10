# Rule Delivery Matrix evaluation

CodexとClaude Codeへ同じ20ケースを渡し、Ruleの配送方式だけを変えて比較するための隔離評価です。productionのKB、端末設定、AI guard、remote syncには触れません。

## 比較する4方式

| mode | 配送内容 |
|---|---|
| `semantic_only` | Ruleを事前配送しないbaseline |
| `rules_top_k` | event以外のRuleを関連度順に常に3件配送 |
| `always_topic` | `always`全部と、発話に一致した`topic` / `config`を配送 |
| `always_topic_event` | `always_topic`に加え、対象MCP toolの結果へ`event` Ruleを注入 |

固定suiteは `schemas/examples/rule-delivery-eval.example.json`、入力とtraceのschemaはそれぞれ `schemas/rule-delivery-eval.schema.json` と `schemas/rule-delivery-trace.schema.json` です。ケースIDはA1–A5、T1–T5、E1–E5、C1–C5の20件で固定されています。

## 2026-08-19 実測と実装への反映

macOSで各modeを20ケース×1回実行した結果は次の通りでした。

| client | semantic only | rules top-k | always + topic | always + topic + event |
|---|---:|---:|---:|---:|
| Claude Code | 5/20 | 8/20 | 10/20 | 18/20 |
| Codex CLI | 7/20 | 8/20 | 8/20 | 12/20 |
| 合計 | 12/40 | 16/40 | 18/40 | **30/40** |

`always_topic_event`をRule配送の採用方向とする。ただし30/40なので、event instructionを
モデルへ読ませるだけでは保証としない。実測から次をproductionへ戻した。

- model familyではなく`ClientSurface`を分離し、coding agentと通常チャットの能力を混同しない
- current-noteを持たないsurfaceでは`get.note`をschema上必須にする
- `allow_new_tags`をAI用MCPから除外し、通常tagsへの未知語の直接overrideをできなくする
- note link・degradation・終端状態を`conversation_events` v1で返し、対応hostが決定論的に描画する

以後の広い反復は止め、上記機構を通すfocused regressionと通常チャット接続PoCへ移る。

### production反映後のfocused regression

失敗が機構不足へ集中していたT1／T3／E3をClaude Code／Codex CLIで各1回再実測した。
T3とE3は両clientで通過した。T1の初回は、productionで未知タグを直接指定するoverrideをMCPから除いた後も
fixtureの`config.tag-vocabulary`が「必要なら新語を追加」と配送していたため両clientで失敗した。
Ruleを現行契約へ合わせ、タグ運用の検索・全文取得、既存`review`の使用、`allow_new_tags`を
渡さないことを明示してT1だけ再実測した結果、両clientで検索・全文取得・`review`による起票に成功した。

最終的なfocused regressionは、T1／T3／E3 × 2 clientの**6/6成功**。CodexはT1の最終文へ
リンクを再掲しなかったが、MCPは`required=true`の`note_link` eventを返していた。ADR-0007の
保証境界に合わせ、採点器はモデルの再掲ではなく必須conversation eventの配送をhard checkする。
これはevent配送の合格であり、未対応hostでの固定描画まで実証したものではない。

2026-09-08、fixtureのタグ案内を`tag_vocabulary`と[正式手順](tag-vocabulary.md)へ
合わせた。同日の本人指定で語彙の追加・統合・削除もAI判断とし、個別の本人確認を必須にしない。
既存語を優先し、意味の重複する新語を増やさず、本人の明示的な訂正に従う条件で評価する。
評価suiteは任意の`tag_vocabulary_source`にfixture内の`expected_id`を宣言できる。
公式fixtureは`notes/タグ運用`を明示し、作成時にそのノートのUIDへ固定する。未知のIDは
Vault作成前に拒否し、省略時は題名や候補数から自動選択しない。MCPの評価allowlistには
`tag_vocabulary`、write面の`set_tag_vocabulary_source`に加え、第3段階の
`plan_tag_vocabulary_change` / `apply_tag_vocabulary_change` /
`list_tag_vocabulary_changes` / `get_tag_vocabulary_change`、第4段階の
`plan_tag_vocabulary_rollback` / `rollback_tag_vocabulary_change` / `get_tag_vocabulary_stats`を含める。
KB利用禁止caseでは追加した7ツールも違反として採点する。変更/復元plan・履歴・集計は読取り専用、
applyとrollbackは一括書込みである。
正本指定と一括適用を個々の通常ノートupdateとして数えず、applyの実行ID・件数・保存結果は
必須`tag_vocabulary_changed` eventで届ける。復元は`tag_vocabulary_rolled_back`で元実行ID・復元ID・
件数・保存結果を届ける。T1/T2は指定済みfixtureでの既存語利用を評価する。
未指定・切替・競合・欠損、一括変更・復元・履歴・集計・surface拒否の境界はコア/MCPの回帰テストで検証する。
上記の実モデル成績は当時の配送文に対する結果で、AI自律方針・正本指定・一括変更を加えた
現行手順の再測定値ではない。今回の実装追従を、実モデル評価の再実行として扱わない。

同日の証拠形式の修正も、合成traceとparser・採点器の回帰テストで確認したものです。
外部AIでの再実測は未確認です。旧runnerは初期`delivered_rule_ids`を配送計画から転記して
いたため、上記の旧成績を「initializeを実観測した証拠」へ読み替えません。

`get_tag_vocabulary_stats`の適用/復元件数は実KBの保存済み事実を確かめる手段であり、このfixtureの
意味判断品質や失敗率の採点とは分離する。計画・拒否・実行時間・意味品質は履歴集計では未計測である。

## ビルド

```sh
cargo build -p kb-cli --bins
```

Lindera辞書を既にキャッシュしている環境では、必要に応じて専用の `LINDERA_BUILD_DICTIONARY_CACHE_DIR` を指定します。

## 配送計画とfixtureの確認

配送計画はAIを起動せずに確認できます。

```sh
target/debug/kb eval rule-delivery-plan \
  --suite schemas/examples/rule-delivery-eval.example.json \
  --mode always-topic-event \
  --format json
```

使い捨てVaultだけを生成する場合:

```sh
target/debug/kb eval rule-delivery-fixture \
  --suite schemas/examples/rule-delivery-eval.example.json \
  --output /private/tmp/kb-rule-eval-fixture
```

出力先が空でなければfixture生成は拒否されます。

## Claude Code / Codexで実行

まず小さいmatrixをdry-runします。

```sh
python3 scripts/rule_delivery_eval.py \
  --client claude \
  --client codex \
  --modes always_topic_event \
  --cases A1,E4,C5 \
  --dry-run
```

実行例:

```sh
python3 scripts/rule_delivery_eval.py \
  --client claude \
  --client codex \
  --runs 2 \
  --output /private/tmp/rule-delivery-traces.json
```

指定を省略すると2 client × 4 mode × 20 case × 1 run = 160 runです。各runは別のfixture VaultとMCP processを使います。runnerは途中でもtrace JSONを更新し、client stdout/stderr、MCP request/response JSONL、実行commandを`<output名>-raw/`へ残します。既存outputや空でないraw directoryは上書きしません。

runnerはmacOS / Windowsで同じsuiteと採点器を使います。Windowsでは`target/debug/kb.exe`と`kb-rule-eval-mcp.exe`を自動選択し、traceの`os`へ実行OSを記録します。PowerShellでも、リポジトリrootから次のように実行できます。

```powershell
cargo build -p kb-cli --bins
python scripts/rule_delivery_eval.py --client claude --client codex --cases A1,E4,C5 --dry-run
```

Claude CodeとCodex CLI本体の導入・ログインはOSごとに事前確認します。client、model、OSはtraceの別fieldなので、macOS結果とWindows結果を混ぜずに比較できます。

traceの任意の`evidence`には、実行開始時のsuiteファイルのバイト列に対する`suite_sha256`、
CLI版と取得元を持つ`cli_version`、実MCP traceから読んだ`initialize_response`を記録します。
runnerは開始時のsuiteを一時ファイルへ固定し、plan・fixture・各MCP起動・hash計算に
同じファイルを使います。元のsuiteを実行途中で編集しても、そのmatrixの入力は変わりません。
同じバイト列をrawディレクトリの`suite.snapshot.json`にも保存します。
再採点にはこの保存済みsuiteを指定できます。
CLI版の取得元はClaudeの`system/init.claude_code_version`、またはMCP初期化要求の
`clientInfo.version`という自己申告値です。取得できない版は`null`のままにし、端末設定から
推測しません。旧互換の`model_version`は新runnerでは`null`で、CLI版をモデル版と呼びません。
suite digestは記録したファイルの識別用で、採点時に別ファイルへ自動照合したという証拠ではありません。

`initialize_response`は実応答の`instructions`と`rule_identity`を保持します。初期Rule IDは
実応答の`[Delivered Rules]`本文が準備済みの規則本文と完全一致した場合だけ導出します。
未観測・異なる本文・再接続時に異なる応答があった場合は計画IDを転記しません。
`source=mcp_initialize_server_response`はサーバーで生成された応答のtrace観測です。
traceはstdout書込み前に保存されるので、stdout出力完了、host受信、モデルの遵守までは確認しません。
識別情報のない旧サーバーの実応答は本文観測と版未確認を分け、`rule_identity=null`を残します。
評価用に差し替えた`instructions`のhashを記録し、productionの案内本文のhashを流用しません。

Claude Code側は`--setting-sources ""`でuser / project / local設定を外し、`--strict-mcp-config`で評価用MCPだけを読み、利用可能toolもrunnerのallowlistへ限定します。Codex側は`--ignore-user-config`、`--ignore-rules`、`required=true`、tool allowlistを使います。Codexの設定根拠は公式の[MCP設定](https://learn.chatgpt.com/docs/extend/mcp?surface=cli)と[non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode)です。

clientの認証切れ、状態DB初期化失敗、timeoutなどで有効な推論へ到達しなかった場合、runnerは最初の`Client error:`をtraceへ保存した直後に停止します。環境失敗を残りのケースへ反復して、品質上の0点として集計しません。

`get`はMCPのtext contentに加え、structuredContentにも`body`、`description`、`tags`、`status`、`origin`を含めます。structuredContentを優先するclientでも「全文取得」がメタデータだけに縮退しないことを評価前提とします。各caseのpromptは単発sessionだけで操作内容が確定する自己完結文にし、既に望ましい状態のfixtureへ変更を要求するno-op caseを作りません。

タグfixtureの「タグ運用」ノートはproduction parserと同じ`## 語彙`表形式で作り、サーバー側の語彙検証も有効にします。T1／T2はAIが既存の`review`で表せると判断するケースです。成功した`propose`／`update`の`tags`引数に`review`があり、意味の重複する`assistant-eval`が無いことを機械判定します。新語一般の禁止や本人確認の有無を評価するケースではありません。

E4はclient UIの画像upload能力ではなく、固定の1 px PNG Base64を発話へ含めて`attach`のMCP契約、`artifact_id`、`ref_name`、`availability`を比較します。通常チャットUIでの実ファイルuploadとクリック可能なartifact deep linkは別PoCです。

## 採点

```sh
target/debug/kb eval rule-delivery-score \
  --suite schemas/examples/rule-delivery-eval.example.json \
  --traces /private/tmp/rule-delivery-traces.json \
  --format markdown \
  --output /private/tmp/rule-delivery-report.md
```

機械判定は次をhard failureとして扱います。

- 必須tool順序の欠落、禁止tool、tool error
- Codex builtin shell / file change / web searchによるMCP迂回
- 必須note IDや回答factの欠落
- 必須`note_link` conversation event、添付identity、操作前notice、degraded表示の欠落
- 配送Rule IDまたはevent Rule IDの不一致
- 観測したinitialize規則本文の不一致、または`rule_identity`と実本文のhash不一致

あわせて、無関係Rule数、推定Rule token、client input token、latencyをmode別に集計します。
JSON reportは各traceの`evidence`と`delivery_evidence`（`unverified` /
`server_response_observed` / `mismatch`）を残し、Markdown reportも規則本文のサーバー応答を
観測した件数を成績と分けて表示します。schema 1.0の既存fixture・traceは引き続き読み込めます。
`evidence`のない旧traceの採点は維持し、配送証拠は`unverified`とします。`passed`だけから
host受信や版一致を推測しません。event Rule IDもサーバーtrace由来であり、host受信確認ではありません。

## テスト

```sh
cargo test -p kb-core rule_delivery_eval::tests
cargo test -p kb-core mcp::tests
cargo test -p kb-cli --bins
python3 -m unittest scripts/test_rule_delivery_eval.py
```

## 評価境界

- 実KBは使わず、suiteから生成した合成ノートだけを使う。
- coding agentのmanaged `UserPromptSubmit` hookが実KBを前出ししないよう、runnerは既存hookが通知として除外する固定`task-notification`識別子を発話末尾へ付ける。元のケース本文は変更しない。
- 評価MCPの検索は埋め込みqueueを進めず、fixture生成直後の`embedding_index_pending`を実験上のdegradedへ混ぜない。
- ノート本文に含めた命令注入文字列はデータとして扱う。
- `conversation_link`はv0ではfixture内ノートの絶対path。通常チャット向け製品deep linkは別途設計する。
- `eval_injected_degradations`はfixture fault injectionであり、production劣化ではない。
- clientの認証切れや起動失敗もtraceへ残るが、品質測定値としては採用せず環境失敗として再実行する。
- fixtureに存在せず、tool結果にも現れない`/notes/...md`参照を回答した場合は、実KB混入としてhard failureにする。
