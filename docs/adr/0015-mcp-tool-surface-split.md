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

- `kb-app-read`: search / get / recent、[ADR-0019](0019-proposal-workflow.md)の提案レビュー専用get_proposal、語彙・正本指定状態・候補を読むtag_vocabulary、語彙変更履歴のlist_tag_vocabulary_changes / get_tag_vocabulary_change、保存済み実行のget_tag_vocabulary_stats（2026-09-08追加）、読み取り専用の来歴閲覧history（2026-09-10追加、[ADR-0023](0023-note-provenance-events.md) Phase 2)
- `kb-app-write`: propose / update / attach / prepare_remove / commit_remove、語彙の正本を明示指定するset_tag_vocabulary_source、語彙と利用ノートを一括確定するapply_tag_vocabulary_change、履歴から復元するrollback_tag_vocabulary_change（2026-09-08追加）
- `kb-app-maintenance`: 語彙変更を計画するplan_tag_vocabulary_change、復元を計画するplan_tag_vocabulary_rollback、Markdown競合、蒸留、initiative closure、旧Artifact移行、来歴backfillのplan_provenance_backfill / apply_provenance_backfill（2026-09-10追加）

各processは`tools/list`を絞るだけでなく、一覧外toolの`tools/call`をVault操作前に拒否する。
自動retrievalの子processは`read`へ固定する。従来の全ツール面はCLIと評価fixtureの後方互換として残すが、
通常の接続設定には使わない。

語彙変更/復元のplanはmaintenance面でも読取り専用で、履歴のlist/getと運用集計も準備済みDBを読む。
これら5ツールはDB初期化・修復・同期・書出しを行わない。従来のtag_vocabularyなど、通常のDB準備・
同期を伴うreadツールとは区別する。applyとrollbackは計画の再照合と一括保存を行い、本人承認を要求しない。
専用GUI・別端末への復元履歴移送は[ADR-0022](0022-atomic-tag-vocabulary.md)の後続範囲とする。

## 帰結

read面を参照用の7 toolへ絞り、write / maintenanceは必要時だけ発見させられる。
ただし実際にどのserverを遅延ロードするかはhostの責務であり、kb-appだけでは保証しない。
