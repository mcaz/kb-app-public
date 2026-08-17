# Claude Code 統合(司書運用)

Claude Code で kb-app を「育つ外部記憶」として使うための配線一式。
2026-08-10 に旧 KB の司書運用から移植。

## 構成

| 部品 | 役割 | 置き場 |
|---|---|---|
| MCP サーバー | search / get / recent / propose(confirm は非公開) | `claude mcp add --scope user kb-app -- <kb バイナリ> mcp --vault <名前> --client claude-code/claude` |
| 前出しフック | 発話のたびに `kb search --any`(OR・bm25)で関連ノートを文脈注入。実行前に `kb settings ai-enabled --client claude-code/claude` で全体・Claude別設定を確認し、OFF・判定不能なら注入しない | [kb-hook-preprompt.py](kb-hook-preprompt.py) を settings.json の UserPromptSubmit へ |
| kb-researcher | 検索専用サブエージェント(複数クエリ・全文読み・要点だけ返す) | `~/.claude/agents/kb-researcher.md` |
| 規律 | まず引く・終わりに propose 提案・確定は本人指示の二経路 | `~/.claude/CLAUDE.md`(server instructions と同型) |

## settings.json 断片

```json
"hooks": {
  "UserPromptSubmit": [{
    "hooks": [{
      "type": "command",
      "command": "python3 <このリポ>/integrations/claude-code/kb-hook-preprompt.py",
      "timeout": 15,
      "statusMessage": "kb-app を検索中…"
    }]
  }]
}
```

フックは `KB_BIN` 環境変数(未設定なら PATH → Application Support の導入済みCLI →
開発ビルドの順)で kb バイナリを探す。
前出しと Stop 安全網はどちらも同じAI利用設定に従う。

## Stop 安全網(2026-08-10 追加)

[kb-hook-stop.py](kb-hook-stop.py) を settings.json の Stop へ。前出しフックが
「関連ノートあり」を `~/.claude/state/kb-app/<session>.relevant` に記録し、
セッションが kb-app を一度も引かずに終わろうとしたときだけ差し戻す
(stop_hook_active では必ず通す=無限ループ防止・fail-open)。
文章の「終了前に検索して」を機構強制に置き換えたもの(docs/contract.md 設計原則)。

## 未移植・既知の制約

- 途中チェックポイント・サブエージェント注入は未移植(必要性を観察してから)
- ルーチン群(weekly-review 等)は移植しない — お手入れ(FR-C7)としてゼロベース実装済み
