# コントリビューションガイド

kb-app への関心をありがとうございます。このドキュメントは、変更を提案するときの
前提・手順・受け入れ条件をまとめたものです。

## はじめに読むもの

- [AGENTS.md](AGENTS.md) — 製品境界と不変条件。ここに書かれた invariants に反する
  変更は、実装として正しくても受け入れられません。
- [docs/contract.md](docs/contract.md) — 製品契約の正本。
- [docs/coding-guidelines.md](docs/coding-guidelines.md) — コーディング規約。

特に重要な不変条件を再掲します。

- ノートは AI 所有です。下書き状態・承認キュー・確定ボタンを追加しないでください。
  一時的な状態はタグで表現します。
- `docs/contract.md` は契約の正本です。契約の変更は、同じコミットで文書と
  `kb-core` の実装・テストの両方を更新する必要があります(CI が機械的に検査します)。
- `app/src/lib/bindings.ts` は tauri-specta の生成物です。Rust 側を変更して再生成
  してください。手で編集しないでください。

## 開発環境

- Rust stable(rustfmt, clippy)
- Node.js 22

## 変更前に通すもの

CI と同じ検査をローカルで実行できます。

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
npm --prefix app run check
```

契約に触れる変更では、co-change gate も確認してください。

```sh
python3 scripts/check-contract-cochange.py <base-sha> <head-sha>
```

## Pull Request

1. 目的を1つに絞ってください。無関係な整形やリファクタリングを混ぜないでください。
2. 振る舞いを変える変更にはテストを付けてください。
3. コミットメッセージは日本語で、`fix(ui): ...` のような接頭辞を付けます。既存の
   履歴に倣ってください。
4. 契約・ADR に関わる判断を含む場合は、`docs/adr/` に判断と理由を残してください。

## 依存関係を追加・更新したとき

配布物に含まれる依存には、ライセンス表記の同梱義務があります(例: 同梱している
IPADIC 辞書)。依存を変更したら通知ファイルを再生成してください。

```sh
cargo fetch && npm --prefix app ci && python3 scripts/gen-third-party-notices.py
```

コピーレフト(GPL / AGPL / SSPL)や非商用ライセンスの依存は追加しないでください。
本プロジェクトは MIT で配布しており、これらとは両立しません。

## コントリビューターライセンス契約(CLA)

初回の Pull Request の際に、[CLA.md](CLA.md) への同意をお願いしています。これは
あなたの著作権を取り上げるものではなく、プロジェクトがあなたの貢献を配布・
再ライセンスできるようにするためのものです。詳細と背景は CLA.md に記載しています。

## 脆弱性の報告

公開 Issue ではなく [SECURITY.md](SECURITY.md) の手順に従って非公開で報告して
ください。
