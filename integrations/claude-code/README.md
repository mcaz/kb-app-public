# Claude Code 統合(司書運用)

Claude Code で kb-app を「育つ外部記憶」として使うための配線一式。
2026-08-10 に旧 KB の司書運用から移植。

## 構成

| 部品 | 役割 | 置き場 |
|---|---|---|
| MCP サーバー | search / get / recent / propose(confirm は非公開) | `claude mcp add --scope user kb-app -- <kb バイナリ> mcp --vault <名前> --client claude-code/claude` |
| 前出しフック | 発話のたびに `kb search --any`(OR・bm25)で関連ノートを文脈注入。fail-open・通知/スラッシュコマンドはスキップ | [kb-hook-preprompt.py](kb-hook-preprompt.py) を settings.json の UserPromptSubmit へ |
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

フックは `KB_BIN` 環境変数(未設定なら PATH → 開発ビルドの順)で kb バイナリを探す。

## 旧司書運用からの差分(未移植・既知の制約)

- **Stop 安全網・途中チェックポイント・サブエージェント注入の各フックは未移植**。
  旧実装はベクトル検索+セッション状態共有に依存しており、段1(埋め込み内蔵)実装後に
  再設計する。当面は CLAUDE.md の規律(終了前検索・kb-researcher 委譲)が代替
- 前出しの順位品質は段0(FTS+bm25)なり。段1 で旧システム同等(ベクトル)に上がる
- ルーチン群(weekly-review / maintenance / scout / follow-up)は未移植 —
  FR-C7 お手入れの設計と合わせて別途(旧システム側のルーチンは停止対象)
