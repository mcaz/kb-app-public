# kb-app(仮名)

そのままでも使えるナレッジベース。AI を繋ぐと、会話で育つ外部記憶になる。

- 要件定義: [docs/requirements.md](docs/requirements.md)
- UI たたき(5画面): [docs/ui-draft.html](docs/ui-draft.html)
- OKF 適合設計: [docs/okf-conformance.md](docs/okf-conformance.md)
- 設計判断(ADR): [docs/adr/](docs/adr/) — 0001: コアは Rust、UI は TypeScript
- コーディング規約: [docs/coding-guidelines.md](docs/coding-guidelines.md) — 機械が守る分と、残りの書き方
- PoC 実測: [docs/poc-report.md](docs/poc-report.md) — ADR-0001 判定 3/3 PASS
- AI 間の開発引き継ぎ: [docs/development-context.md](docs/development-context.md) — Context Pack v1
- 状態: **private backup のアプリ内 GitHub 認証まで実装(2026-08-16)** — OAuth device flow、OS キーチェーン保存、private+push gate、複数端末の検査付き復元が接続済み。残: 配布用 OAuth App の登録と実 GitHub 受入、Stop 安全網の再設計、お手入れ FR-C7

## GitHub OAuth の build 設定

GitHub の OAuth App で Device Flow を有効にし、公開情報である Client ID だけを build 環境へ渡す。
client secret は device flow では使わず、アプリや設定ファイルへ置かない。

```sh
KB_GITHUB_CLIENT_ID=Ov23li... npm --prefix app run tauri build
```

アプリは private repository の作成・検査・Git/Git LFS 同期に OAuth の `repo` scope を使う。
token は OS キーチェーンへ保存し、Git 子 process には実行中だけ認証 header として渡す。
