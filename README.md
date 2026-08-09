# kb-app(仮名)

そのままでも使えるナレッジベース。AI を繋ぐと、会話で育つ外部記憶になる。

- 要件定義: [docs/requirements.md](docs/requirements.md)
- UI たたき(5画面): [docs/ui-draft.html](docs/ui-draft.html)
- OKF 適合設計: [docs/okf-conformance.md](docs/okf-conformance.md)
- 設計判断(ADR): [docs/adr/](docs/adr/) — 0001: コアは Rust、UI は TypeScript
- PoC 実測: [docs/poc-report.md](docs/poc-report.md) — ADR-0001 判定 3/3 PASS
- 状態: v0.1 コア最小を実装(ストア・レジストリ・検索・ライフサイクル・MCP。受け入れ=Claude Desktop 接続検証待ち)(2026-08-09)
