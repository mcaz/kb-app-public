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
- `allow_new_tags`をAI用MCPから除外し、未合意タグ追加を能力の不在にする
- note link・degradation・終端状態を`conversation_events` v1で返し、対応hostが決定論的に描画する

以後の広い反復は止め、上記機構を通すfocused regressionと通常チャット接続PoCへ移る。

### production反映後のfocused regression

失敗が機構不足へ集中していたT1／T3／E3をClaude Code／Codex CLIで各1回再実測した。
T3とE3は両clientで通過した。T1の初回は、productionで新規タグ追加能力をMCPから除いた後も
fixtureの`config.tag-vocabulary`が「必要なら新語を追加」と配送していたため両clientで失敗した。
Ruleを現行契約へ合わせ、タグ運用の検索・全文取得、既存`review`の使用、`allow_new_tags`を
渡さないことを明示してT1だけ再実測した結果、両clientで検索・全文取得・`review`による起票に成功した。

最終的なfocused regressionは、T1／T3／E3 × 2 clientの**6/6成功**。CodexはT1の最終文へ
リンクを再掲しなかったが、MCPは`required=true`の`note_link` eventを返していた。ADR-0007の
保証境界に合わせ、採点器はモデルの再掲ではなく必須conversation eventの配送をhard checkする。
これはevent配送の合格であり、未対応hostでの固定描画まで実証したものではない。

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

Claude Code側は`--setting-sources ""`でuser / project / local設定を外し、`--strict-mcp-config`で評価用MCPだけを読み、利用可能toolもその7 toolへ限定します。Codex側は`--ignore-user-config`、`--ignore-rules`、`required=true`、tool allowlistを使います。Codexの設定根拠は公式の[MCP設定](https://learn.chatgpt.com/docs/extend/mcp?surface=cli)と[non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode)です。

clientの認証切れ、状態DB初期化失敗、timeoutなどで有効な推論へ到達しなかった場合、runnerは最初の`Client error:`をtraceへ保存した直後に停止します。環境失敗を残りのケースへ反復して、品質上の0点として集計しません。

`get`はMCPのtext contentに加え、structuredContentにも`body`、`description`、`tags`、`status`、`origin`を含めます。structuredContentを優先するclientでも「全文取得」がメタデータだけに縮退しないことを評価前提とします。各caseのpromptは単発sessionだけで操作内容が確定する自己完結文にし、既に望ましい状態のfixtureへ変更を要求するno-op caseを作りません。

タグfixtureの「タグ運用」ノートはproduction parserと同じ`## 語彙`表形式で作り、サーバー側の語彙検証も有効にします。T1／T2は最終回答の語だけでなく、成功した`propose`／`update`の`tags`引数に`review`があり、未合意の`assistant-eval`が無いことを機械判定します。

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

あわせて、無関係Rule数、推定Rule token、client input token、latencyをmode別に集計します。

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
