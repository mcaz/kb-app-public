# kb-app(仮名)

そのままでも使えるナレッジベース。AI を繋ぐと、会話で育つ外部記憶になる。

- 要件定義: [docs/requirements.md](docs/requirements.md)
- UI たたき(5画面): [docs/ui-draft.html](docs/ui-draft.html)
- OKF 適合設計: [docs/okf-conformance.md](docs/okf-conformance.md)
- 設計判断(ADR): [docs/adr/](docs/adr/) — 0001: コアは Rust、UI は TypeScript
- コーディング規約: [docs/coding-guidelines.md](docs/coding-guidelines.md) — 機械が守る分と、残りの書き方
- PoC 実測: [docs/poc-report.md](docs/poc-report.md) — ADR-0001 判定 3/3 PASS
- Rule Delivery評価: [docs/rule-delivery-evaluation.md](docs/rule-delivery-evaluation.md) — Codex / Claude Code共通の隔離20ケース
- AI 間の開発引き継ぎ: [docs/development-context.md](docs/development-context.md) — Context Pack v1
- 状態: **private backup実受入、authority付き正本判定、snapshot固定のread-only蒸留plannerまで実装(2026-08-20)** — `plan_distillation` / `kb distill plan`はDBを変更せず、入力hash・snapshot digest・決定的plan ID付きで候補を列挙する。semantic executorとatomic supersedeは次段

## 継続蒸留plan

準備済みSQLite DBを、pull・sync・migrationなしのread-only snapshotとして点検する。

```sh
kb --vault <name> distill plan
kb --vault <name> distill plan --format markdown
```

同じsnapshotからは同じJSONが出る。これは承認キューや実行指示ではなく、後続のsemantic executorが
入力版を再照合するための監査記録である。出力契約は
[schemas/distillation-plan.schema.json](schemas/distillation-plan.schema.json)、設計判断は
[ADR-0010](docs/adr/0010-read-only-distillation-planner.md)を参照。

## GitHub OAuth の build 設定

GitHub OAuth App `kb-app` (`mcaz` 所有) は Device Flow と期限付き token を有効化済み。
公開情報である Client ID だけを build 環境へ渡す。client secret は device flow では使わず、
アプリや設定ファイルへ置かない。

```sh
KB_GITHUB_CLIENT_ID=Ov23li1xyWYmAMscYlj8 npm --prefix app run tauri build
```

ローカルの macOS アプリは、次の1コマンドでビルドから安全な差し替えまで更新できる。

```sh
npm --prefix app run update:app
```

更新処理は起動中の GUI だけを終了し、AI client が子起動している MCP server は強制終了しない。
配置に失敗した場合は旧版へ戻す。更新後、接続中の AI client で MCP を再接続すると新しい版が
使われる。Git LFS sidecar と Lindera 辞書はローカルに再利用し、毎回の再取得を避ける。
従来の `install:app` は互換 alias として残す。

このコマンドは上記 Client ID を既定値として使う。fork や別の OAuth App を使う場合だけ
`KB_GITHUB_CLIENT_ID` で上書きする。

アプリは private repository の作成・検査・Git/Git LFS 同期に OAuth の `repo` scope を使う。
token は OS キーチェーンへ保存し、Git 子 process には実行中だけ認証 header として渡す。

2026-08-16 の実受入では、非機密 fixture 専用の private repository をアプリから作成し、
private + push 権限の再検査、初回 push、別ディレクトリへの clone / Storage Contract 検査 / 復元を
完走した。remote URL と `.git/config` に token が残らないことも確認済み。
