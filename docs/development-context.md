# 開発 Context Pack v1

Claude と Codex が会話履歴なしでも、同じ根拠から現在の作業を再開するための P0 契約。
会話ログを共有するのではなく、情報ごとに正本を分けて起動時に JSON へ合成する。

| 情報 | 正本 |
|---|---|
| branch、HEAD、tracked / untracked の内容 | Git |
| 検証コマンド、終了コード、範囲、対象 state stamp | `.kb-dev/checks.json` |
| task、next、discarded、open questions、TTL | `.kb-dev/handoff.json` |
| 共有へ昇格した計画と受入条件 | GitHub Issue / PR |
| 長期に残す理由、失敗、仕様解釈 | KB |

`.kb-dev/checks.json` と `.kb-dev/handoff.json` はローカル状態なので Git へ入れない。
schema、example、生成スクリプト、contract test は追跡する。

## 使い方

```bash
# 起動時の短い情報。通常はこれだけを読む
scripts/dev-state.sh

# 指定した contract / ADR / pitfall の本文まで読む
scripts/dev-state.sh --mode detail --id docs/contract.md

# 明示的に全件の本文を読む
scripts/dev-state.sh --mode full
```

出力は常に [context-pack.schema.json](../schemas/context-pack.schema.json) に従う。将来の
`project_context` MCP も同じ schema を返し、違いは `producer.kind` だけにする。

## checks の規則

[checks.schema.json](../schemas/checks.schema.json) の `recorded_for.state_stamp` は HEAD だけでなく、
index、全 tracked file の現在内容、全 untracked file の現在内容を含む。1 byte の編集でも新規
test file でも値が変わる。

stamp、branch、base commit のどれかが現在値と違えば、過去の結果が成功でも Context Pack は
`validity: stale`、`result: unknown`、`scope: unknown` を返す。`partial` を `full` へ昇格させない。

この P0 のスクリプトは読み取り専用である。検証 runner はコマンド完了後、schema に従う JSON を
一時ファイルへ作り、rename で `.kb-dev/checks.json` へ原子的に置く。writer の統合は次段で行う。

## handoff の規則

[handoff.schema.json](../schemas/handoff.schema.json) は短命な作業意図であり、Git の事実を上書きしない。
branch が違えば `orphaned_handoff`、base commit が違えば `stale_base`、TTL 超過なら `expired`。
いずれも task / next を出力せず、AI が誤って採用できない形にする。

push や Context Pack の取得だけで GitHub Issue を作らない。Issue 作成は明示的な promote、PR 作成、
または本人がデバイス間継続を要求した場合に限る。

## 検証

```bash
python3 tests/context-pack/test_contract.py
```

テストは依存パッケージなしで動き、JSON Schema の本リポジトリ使用部分、script / future MCP fixture、
tracked / untracked の stale 判定、partial、freshness、wrong-branch handoff を検査する。合意した
T1〜T14 の責任範囲は [acceptance-cases.json](../tests/context-pack/fixtures/acceptance-cases.json) に固定した。
T9 の note revision / expected_version は次段の core 実装であり、P0 では受入条件だけを保持する。
