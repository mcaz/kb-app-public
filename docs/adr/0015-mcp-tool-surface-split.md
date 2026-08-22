# ADR-0015: MCPツールを用途別surfaceへ分離する

- 日付: 2026-08-22
- 状態: 採用
- 関連: [contract.md](../contract.md) 契約8 / [requirements.md](../requirements.md) FR-C5

## 背景

単一MCP serverへ全ツールを公開すると、hostのtool searchや遅延ロードによって`search` / `get` /
`recent`まで初期文脈から外れることがある。その場合、モデルは接続済みMCPを「使えない」と誤認しやすい。
一方、MCP server自身はhostの`defer_loading`方針を強制できない。

## 決定

通常接続を次の3 serverへ分ける。

- `kb-app-read`: search / get / recent
- `kb-app-write`: propose / update / attach / prepare_remove / commit_remove
- `kb-app-maintenance`: Markdown競合、蒸留、initiative closure、旧Artifact移行

各processは`tools/list`を絞るだけでなく、一覧外toolの`tools/call`をVault操作前に拒否する。
自動retrievalの子processは`read`へ固定する。従来の全ツール面はCLIと評価fixtureの後方互換として残すが、
通常の接続設定には使わない。

## 帰結

read面は3 toolだけなのでhostが初期ロードしやすく、write / maintenanceは必要時だけ発見させられる。
ただし実際にどのserverを遅延ロードするかはhostの責務であり、kb-appだけでは保証しない。
