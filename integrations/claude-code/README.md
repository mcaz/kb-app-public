# Claude Code 統合(司書運用)

Claude Code で kb-app を「育つ外部記憶」として使うための配線一式。
2026-08-10 に旧 KB の司書運用から移植。

先に kb-app の設定 Modal で「完全保護」を設定する。system-level managed settings が
Vault と kb-app の端末設定を Claude の Read / Edit / sandboxed Bash から常時隠す。
このポリシーが未導入・古い・競合状態なら、MCP と下記 hook はどちらも fail-closed になる。

## 構成

| 部品 | 役割 | 置き場 |
|---|---|---|
| MCP サーバー | search / get / recent / propose(confirm は非公開) | `claude mcp add --scope user kb-app -- <kb バイナリ> mcp --vault <名前> --client claude-code/claude` |
| 前出しフック | 発話ごとに同じkb-app実行ファイルをMCP serverとして子起動し、`initialize → search(any) → get`を実行。OFFならsearchの権威ある`kb_disabled`終端結果を検知して無音終了 | 完全保護のmanaged settingsがUserPromptSubmitへ登録 |
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
