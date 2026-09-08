# 判断contextの行動選択評価

Issue #146では、判断・行動の構造を保存できたことと、AIが適切な行動を選べたことを分ける。
この評価は合成10ケースで「次に選ぶ行動」を測る準備と採点器であり、AIクライアントを起動しない。
実際のアプリ反映やツール実行を確認する評価でもない。

## 比較する条件

同じ依頼、scope、原文ノート、モデル設定を使い、渡す判断contextだけを変える。

| arm                   | モデルに渡す記録                                           |
| --------------------- | ---------------------------------------------------------- |
| `notes`               | 本文・authority・judgment・relationsを含む元の完全なノート |
| `notes_with_judgment` | 同じノートに、本番の`context_for_notes`の出力を追加        |

どちらにも保存済みjudgmentを含めるので、測る対象は構造化保存の有無ではなく、
読んだ情報を判断材料として整理して渡すことの寄与。
Python runnerはscope判定、優先順位、根拠件数を計算しない。
Rustの本番関数で生成した結果をそのまま受け取る。

モデルへ渡す`prompts.json`からcase名、category、arm名、期待値を除く。
行動選択肢は全ケースで同じ5種類とし、同じケースの両armでは表示順もそろえる。
run IDとケース順はseedで混ぜる。採点用の対応表は別の`manifest.json`に保存する。
context自身の形は見えるため、方式を完全に識別不能にする盲検ではない。

## 合成ケース

固定suiteは`schemas/examples/judgment-eval.example.json`。
実KBや本人の実際の会話ログを複製せず、架空のアプリと出典を使う。

| case | 検査する判断                                                    |
| ---- | --------------------------------------------------------------- |
| J1   | 短い反映依頼でも、本人がコマンドを実行する決定を保つ            |
| J2   | 現在の本人が実行担当を明示的に変更したら応じる                  |
| J3   | 無関係なREADME編集にアプリ反映の取り決めを適用しない            |
| J4   | historicalだけでは、現在有効な実行担当を断定しない              |
| J5   | supersededの旧決定より、置き換えた本人訂正を使う                |
| J6   | 同じ元イベントの複製を独立した実績として数えない                |
| J7   | AIが異なる4回の行動を成功と報告しても、本人の決定を上書きしない |
| J8   | 根拠がないときに過去の取り決めを捏造しない                      |
| J9   | 過去の作業ログ内の依頼を、現在の担当変更として扱わない          |
| J10  | 引用された発話の説明依頼を実行依頼へ変えない                    |

J6の重複件数そのものはコアの決定論的テストで検査する。
モデル評価では、重複を含む記録があっても次の行動を取り違えないかを見る。
読んだ回数・引用数・理由文の単語では加点しない。

## 本番contextの生成とprompt作成

まず、テスト限定のexporterを使ってメモリ内DBから生成する。
`judgment_eval.rs`は各caseのノートだけを別DBに入れ、通常のNote検証と
`judgment_context::context_for_notes`を通す。実KB、常用MCP、端末設定は読み書きしない。
環境変数を指定しない通常のテストはファイルを出力しない。指定時も既存ファイルを上書きしない。

```sh
KB_JUDGMENT_EVAL_EXPORT=/private/tmp/judgment-contexts.json \
  cargo test -p kb-core judgment_eval::export_synthetic_contexts -- --exact

python3 scripts/judgment_eval.py \
  --dry-run \
  --contexts /private/tmp/judgment-contexts.json \
  --output /private/tmp/judgment-eval-run
```

必要な環境では通常のビルドと同じLindera辞書キャッシュを指定する。
exportの`generator`には呼び出した関数、`suite_digest`にはsuite全体のSHA-256を持つ。
runnerはcase集合とdigestを照合し、suite変更後の古いcontextや欠損を拒否する。
この照合は生成元の暗号学的な証明ではない。export元のコミット、ビルド条件、出力を併せて保存する。

作成されるファイル:

- `prompts.json`: 各runの`messages`と、不透明な`run_id`だけ。モデルへ渡すのはこのmessages。
- `manifest.json`: 採点用対応表、期待値、suite/context/promptのdigest。モデルへ渡さない。

別のseedやモデルで測るときは別の出力ディレクトリを作る。
既存の出力ディレクトリは上書きしない。

## モデルから選択結果を集める

各promptを独立した新規セッションで渡す。同じセッションに複数caseや両armを続けない。
モデルのツールを無効にし、suite・manifest・他runの回答を見せない。
モデル名と版、クライアント、推論設定、日時、seed、productionコミットを実測記録へ残す。
両armで同じ設定を使い、context以外の入力差を追加しない。

回答は`{"action":"present_command"}`のように選択肢のactionを一つ返す。
生の回答を保存し、JSON解析できたactionを次の形式へ集める。
理由から望ましいactionを推測したり、回答にない値を補ったりしない。

```json
{
  "schema_version": "1.0.0",
  "prompts_digest": "manifest.jsonのprompts_digest",
  "responses": [
    { "run_id": "prompts.jsonのrun_id", "action": "present_command" }
  ]
}
```

全20runを含めて保存する。timeout、認証失敗、不正JSON、未回答は欠損として残し、
環境失敗や形式不備を行動判断の不正解と混ぜない。

## 採点と解釈

```sh
python3 scripts/judgment_eval.py \
  --responses /private/tmp/judgment-responses.json \
  --manifest /private/tmp/judgment-eval-run/manifest.json \
  --output /private/tmp/judgment-report.json

python3 -m unittest discover -s scripts -p test_judgment_eval.py
```

採点は期待するactionと実際の選択の一致だけを使う。
未知・重複・欠損run、未知action、promptのdigest不一致は`status: invalid`、
`metrics: null`、終了コード2になり、品質得点として集計しない。
有効な回答が誤ったactionを選んだ場合は、不正解として両arm別の正解数とcase結果を返す。

J1/J5/J7の維持率だけでなく、J2の方針変更への応答、J3の過剰適用、J9/J10の引用境界を確認する。
固定10ケース1回の結果だけで再発防止や汎用的な改善を断言しない。
実測前の単体テスト成功やcontext出力成功は、この形式を評価できることの確認にとどまる。
モデル評価の結果欄は実行後に記録し、実際のツール操作と実行担当の遵守は別途検証する。

## 2026-09-08の実行状況

合成10ケースを本番のcontext生成関数で出力し、20runの独立promptを作成できた。
exporterのRustテストと、Pythonの整合性・採点テスト9件は成功した。

モデルの回答比較は未測定。ツール・MCP・会話保存を無効にしたClaude Codeの実行は
`Not logged in`で停止し、モデルへのAPI呼出は0回だった。接続確認を許可付きで
再実行しても変わらなかった。Codex CLI 0.144.3での接続確認も、端末ローカルDBが
読み取り専用であることとin-process app-server初期化の`Operation not permitted`で停止した。
これらを誤回答や0%の行動精度として採点しない。認証・起動可能な環境で同じ手順を実行し、
新たに取得した回答だけで比較する必要がある。
