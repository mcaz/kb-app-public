# Claude Code 統合(司書運用)

Claude Code で kb-app を「育つ外部記憶」として使うための配線一式。
2026-08-10 に旧 KB の司書運用から移植。

先に kb-app の設定 Modal で「完全保護」を設定する。system-level managed settings が
Vault と kb-app の端末設定を Claude の Read / Edit / sandboxed Bash から常時隠す。
このポリシーが未導入・古い・競合状態なら、MCP と下記 hook はどちらも fail-closed になる。

## 構成

| 部品 | 役割 | 置き場 |
|---|---|---|
| MCP サーバー | read / write / maintenance の用途別3面 | `claude mcp add --scope user kb-app-read -- <kb バイナリ> mcp --surface read --vault <名前> --client claude-code/claude`（同様に`kb-app-write`=`--surface write`、`kb-app-maintenance`=`--surface maintenance`）。host 側の`search`は配信profile `session_auto`（省略時既定 — 現行の候補展開予算そのもの。予算を絞る`session_explicit`は明示選択のみ）で動き、`--retrieval-profile`で process 単位に固定できる。initialize の`kbApp.retrieval_profile`で確認する（[retrieval-profiles.md](../../docs/retrieval-profiles.md)） |
| 前出しフック | 発話ごとに同じkb-app実行ファイルをMCP serverとして子起動し（`--retrieval-profile session-auto`を明示）、`initialize → search(any, include_documents)`を実行。上位5 seedからDBリンクを最大2ホップ展開し、最大50候補から予算内・最大10本文を同じDB snapshotで返す。OFFならsearchの権威ある`kb_disabled`終端結果を検知して無音終了 | 完全保護のmanaged settingsがUserPromptSubmitへ登録 |
| kb-researcher | 検索専用サブエージェント(複数クエリ・全文読み・要点だけ返す) | `~/.claude/agents/kb-researcher.md` |
| 規律 | まず引く・終わりに propose 提案・確定は本人指示の二経路 | `~/.claude/CLAUDE.md`(server instructions と同型) |

## 管理フック

設定Modalの「完全保護を設定」が、Claude Codeのsystem-level managed settingsとCodexの
managed requirementsへ同じUserPromptSubmitフックを登録する。フックはCLIの`kb search`や
Vaultファイルを使わず、kb-app MCPの公開面だけを使う。検索失敗は主作業を止めないが、
「該当なし」へ変換せず劣化コンテキストとしてクライアントへ返す。

旧`~/.claude/settings.json`に残るPythonフックは、完全保護の更新時に他のフックを保ったまま
削除する。移行前に起動されたセッション向けに、[kb-hook-preprompt.py](kb-hook-preprompt.py)は
同じMCP自動retrievalモードへ転送する互換ラッパー、[kb-hook-stop.py](kb-hook-stop.py)は
no-opとして残す。

## 未移植・既知の制約

- 途中チェックポイント・サブエージェント注入は未移植(必要性を観察してから)
- ルーチン群(weekly-review 等)は移植しない — お手入れ(FR-C7)としてゼロベース実装済み
