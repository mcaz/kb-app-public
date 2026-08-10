# kb-app(仮名)

そのままでも使えるナレッジベース。AI を繋ぐと、会話で育つ外部記憶になる。

- 要件定義: [docs/requirements.md](docs/requirements.md)
- UI たたき(5画面): [docs/ui-draft.html](docs/ui-draft.html)
- OKF 適合設計: [docs/okf-conformance.md](docs/okf-conformance.md)
- 設計判断(ADR): [docs/adr/](docs/adr/) — 0001: コアは Rust、UI は TypeScript
- PoC 実測: [docs/poc-report.md](docs/poc-report.md) — ADR-0001 判定 3/3 PASS
- 状態: **kb-app へ一本化(2026-08-10 本人決定)** — 旧 KB から50本移植済み・Desktop は kb-app のみ・ソースは github.com/mcaz/kb-app(private)。残: 段1 かしこい検索・GitHub OAuth・Claude Code 側の旧サーバー引退
