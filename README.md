# kb-app(仮名)

そのままでも使えるナレッジベース。AI を繋ぐと、会話で育つ外部記憶になる。

- 要件定義: [docs/requirements.md](docs/requirements.md)
- UI たたき(5画面): [docs/ui-draft.html](docs/ui-draft.html)
- OKF 適合設計: [docs/okf-conformance.md](docs/okf-conformance.md)
- 設計判断(ADR): [docs/adr/](docs/adr/) — 0001: コアは Rust、UI は TypeScript
- コーディング規約: [docs/coding-guidelines.md](docs/coding-guidelines.md) — 機械が守る分と、残りの書き方
- PoC 実測: [docs/poc-report.md](docs/poc-report.md) — ADR-0001 判定 3/3 PASS
- AI 間の開発引き継ぎ: [docs/development-context.md](docs/development-context.md) — Context Pack v1
- 状態: **段1 まで完成(2026-08-10)** — 一本化完了(Desktop/Code とも kb-app のみ・旧サーバー引退・ollama 停止)+かしこい検索が有効(53/53 埋め込み済み)。残: GitHub OAuth・Stop 安全網の再設計・お手入れ FR-C7
