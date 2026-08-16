# コーディング規約

- 制定: 2026-08-12
- 関連: [contract.md](contract.md)(契約の正本)/ [ADR-0001](adr/0001-core-language.md) /
  [ADR-0002](adr/0002-frontend-stack.md)(層・ディレクトリ・状態・エラーの決定)

> **この文書は強制の階段の最下段(常駐文章)にいる。**
> 確定すべき挙動は機構で必然にする、というのがこのアプリの設計原則
> ([contract.md](contract.md) 設計原則 / KB「AI 協働アプリの規律は文章でなく機構で確定させる」)。
> したがってこの規約の役目は2つだけ:
>
> 1. **機械が既に守っている規律を列挙して、二度と文章で繰り返さないこと**(§1)
> 2. **まだ機械化できていない残りを書き、機械へ上げる道筋を持つこと**(§2〜§8、§9)
>
> 規約が増えたと感じたら、条文を足す前に §9 を見る。文章のまま太らせない。

## 0. 何をどこに書くか

書く場所を間違えると、正本が二重化して静かにズレる。振り分けは固定する。

| 種類 | 正本 | 例 |
| --- | --- | --- |
| 契約(壊すと UI が成立しない不変) | `docs/contract.md` + `kb-core` の検証コード | タグ1〜4個、OKF はアプリが管理 |
| 設計判断(なぜこの構成か) | `docs/adr/` | コアは Rust、フロントは React |
| 開発運用(AI エージェント向けの手順) | `AGENTS.md` | 検証コマンド、`cargo clean` の禁忌 |
| コードの書き方 | **この文書** | コメントの流儀、エラーの置き場 |
| 運用の合意・調べた知見 | KB(kb-app MCP) | タグ運用、技術選定の軸 |

**契約と運用を混同しない。** 契約の変更は文書とコアの検証を同時に変える。
運用は会話で合意して KB に記録する。この規約はその中間 — コードの一貫性のためだけにある。

## 1. 機械が守るもの(ここでは繰り返さない)

以下は**文章で守る対象ではない**。破れば手元とCIで落ちる。規約として再掲しない。

| 規律 | 強制点 |
| --- | --- |
| 整形(TS/CSS/MD、Tailwind クラス順) | Prettier(`printWidth: 100`・二重引用符・末尾カンマ)→ `npm run format:check` |
| 整形(Rust) | `rustfmt` 既定(edition 2024)→ `cargo fmt --all -- --check` |
| 層の境界(副作用は organisms 以上・下から上を見ない) | `eslint.config.js` の `no-restricted-imports` |
| Tauri commandの入口は `src/lib/api` だけ | `no-restricted-imports` で `invoke` とnamespace importを禁止 |
| コンポーネントの公開面は `index.ts` だけ | 同上(`DEEP_IMPORT`) |
| 型の厳しさ(`strict` / `noUncheckedIndexedAccess` / 未使用の禁止) | `tsconfig.json` → `npm run typecheck` |
| 投げっぱなしの Promise 禁止、型 import はインライン | typescript-eslint(型情報あり) |
| a11y の基本 | eslint-plugin-jsx-a11y |
| Rust の警告ゼロ | `cargo clippy --workspace --all-targets -- -D warnings` |
| `app/src/lib/bindings.ts` は生成物(手書き禁止) | CI の `git diff --exit-code` |
| 対訳の欠落・余剰・空文字 | `src/i18n/locales/locales.test.ts` |
| JSX の日本語文字列は locale に置く | `no-restricted-syntax` で JSX 内だけを検査(コメント・正規表現は対象外) |
| TSX に生の hex を書かない | `no-restricted-syntax`。canvas の `.ts` fallback は対象外 |
| 契約1〜4(タグ・OKF・所有・劣化表示) | `kb-core` の検証コード |
| 契約文書と強制実装・テストの同一コミット更新 | 契約SHA-256テスト + `scripts/check-contract-cochange.py`(CI) |
| coreの診断文をGUIへ直送しない | `CoreError` → `AppError`。`From<anyhow::Error>` を持たず未分類はコンパイル失敗 |
| Rust の unsafe・仮実装・理由なし allow 禁止 | workspace lint(`unsafe_code` / `dbg_macro` / `todo` / `unimplemented` / `allow_attributes_without_reason`) |

手元の通し方は [AGENTS.md](../AGENTS.md) の Verification に一本化してある。

## 2. 言語

- **コメント・ドキュメント・コミットメッセージ・`docs/` は日本語。**
  識別子・ファイル名・タグ・git のブランチ名は英語。`AGENTS.md` だけは英語
  (AI クライアントが読む面のため)。
- **エラー文言は `code` / `kind` で訳し分ける。** `unexpected.message` もログ専用で、
  画面は locale の一般文言を表示する(§5)。

## 3. コメント

**「何をしているか」は書かない。コードが答える。書くのは「なぜそうなっているか」。**
このリポジトリのコメントは実質すべてこの形で、水準はここで揃える。

書く価値があるのは次の4種:

1. **選ばなかった選択肢と、その理由** —
   `app/src/styles/app.css` 冒頭(CSS に `prefers-color-scheme` を置かない理由)
2. **事故の記録**。日付と実際に起きたことを添える —
   `crates/kb-core/src/tags.rs` の `glossary`(2026-08-12 に偽タグ16語が混入した経緯)
3. **踏むと分からない罠** —
   `eslint.config.js` の「`no-restricted-imports` は後勝ち」注意書き
4. **定数の値の根拠** — `MAX_LEN` / `VOCAB_SHOWN` のように、値の隣に なぜその数か

長い背景は**リンクで逃がす**。ADR・契約・KB ノートを参照し、コメント本体は要点だけにする
(`crates/kb-core/src/tags.rs` のモジュールコメントが手本)。

## 4. 層をまたぐときの原則

lint が落とすのは import の向きだけで、**責務の置き場所は文章側に残っている**。

- **統治のロジックは `kb-core` にしか置かない。** Tauri の `commands/` と `kb-cli` と
  `mcp` は、コア API を呼ぶだけの薄い口。判断を口の側に書いたら、他の2つから消える。
- **書き込み経路はコアで合流させる。** ノートを変える経路は MCP と CLI の2つあるので、
  検証を経路ごとに書かない(`tags.rs` がこの形)。
- **サーバ由来の状態は `lib/queries`、画面の都合は `lib/stores`。**
  手書きのキャッシュを作らない(旧実装の `tagOv` / `graphCache` に戻る)。

## 5. Rust

- **エラーは層で使い分ける。** `kb-core` 内部では原因のchainに `anyhow` を使ってよいが、
  GUIへ渡す境界では必ず `CoreError` のkindへ分類する。Tauri 層はそれを `AppError` へ
  対応付け、診断detailをserializeしない。`AppError` は `From<anyhow::Error>` を持たないため、
  新しいコア呼び出しを未分類のまま追加するとコンパイルで止まる。画面が新しい案内を
  必要とするときは `CoreErrorKind` とlocaleを同じ変更で増やす。
- **テストは同じファイルの `#[cfg(test)] mod tests`。** 別ディレクトリに出さない。
  実データを触るテストは `tempfile::tempdir()` + `Vault::create` の `setup()` を各モジュールに置く。
- **事故を直したら再現テストを足し、そのテストに日付入りの doc コメントを書く。**
  実例: `tags.rs::glossary_reads_only_the_vocabulary_section`。
  「なぜこの奇妙なケースを検査するのか」が消えると、次の誰かがテストごと消す。
- **`pub` にするのは他の crate/層が実際に使うものだけ。** モジュール名は機能の名詞
  (`tags` / `search` / `vault`)、関数は動詞(`validate_shape` / `glossary`)。
- **feature は用途で切り、下流に持ち込まない。** `specta` は GUI 向けの型生成専用で
  CLI には入れない(`crates/kb-core/Cargo.toml` のコメント参照)。
- **長い処理は同期コマンド+イベント。** async コマンドは async ランタイムを止める
  (ADR-0002 決定10。`embed_enable` が実例)。

## 6. TypeScript / React

- **1コンポーネント = 1ディレクトリ**、公開面は `index.ts` だけ(lint が強制)。
  そのコンポーネントだけが使う下位部品は同じディレクトリに入れ子で置く。
- **バリアントは `tailwind-variants` の `tv()` を `variants.ts` に置く**(ADR-0002 決定6)。
  `cva` は移入時に置き換える。命名は `<コンポーネント名>Variants`。
  本体ファイルには JSX と振る舞いだけを残し、見た目の指定はそこへ寄せる
  — 配色やサイズを追うときに開くファイルが1つに定まる。
  例外は `atoms/ui` の shadcn 由来部品で、上流の形(同ファイルで `buttonVariants` を
  公開)のまま置く(ADR-0002 決定2「できるだけ素のまま取り込む」)。
- **ダークは `dark:`(= `data-theme`)のみ。** CSS に `prefers-color-scheme` を書かない。
- **ノートの段(`NotesLayout`)は幅を内容から決める。** 列が横に並び、はみ出したら
  横スクロールする形なので、**内側に横長の要素を足すと列そのものが広がって窓の外へ出る**。
  本文と同じ 46em の段に収め、`min-w-0` を鎖で切らさないこと(`FilePanel` が実例)。
  2026-08-13 に操作ボタン付きの行を足して 642px → 857px に広がった。画面写真では
  「ボタンが切れている」までしか分からないので、**足す前後の幅を数値で測って確かめる**。
- **テストは「壊れても画面に出ない純ロジック」に絞る。** 現状のテストは
  `hits` / `graph/subgraph` / `locales` の3つで、コンポーネントのレンダリングテストは無い。
  この方針を踏襲する — 見た目の回帰は目で見て、計算とデータ変換は自動で守る。
- **フックは `use` 接頭辞で `src/hooks/`**(画面横断)か、コンポーネントのディレクトリ内
  (そのコンポーネント専用)。query key は `lib/queries/keys.ts` に集約し、文字列を散らさない。

## 7. 依存を足すとき

新しいライブラリ・技術の判断軸は性能や記述量ではなく、
**「LLM が間違えたときに、レビューで気づけるか」**
(KB「AI が書く比率が高いコードでは『LLM が安定して出せるか』が技術選定の軸に入る」)。

- 本人がその技術を知らず、かつ LLM の学習データが薄い/最近構文が変わった場合、
  混入した古い書き方を弾けない。この2つが揃うときだけ不利に働く。
- この軸は選択を主流へ寄せる方向にしか働かないので、**打ち消したいときは「学ぶ」を選ぶ**。
- **採否を ADR に残すときは「技術的にはこちらが優れていた」を必ず書く。**
  書かないと、後から見て単なる保守的選択に見える。
- shadcn/ui 由来のソースは**できるだけ素のまま**取り込む(`atoms/ui`。lint も対象外にしてある)。
  手を入れるほど上流の更新を取り込めなくなる。

## 8. 変更を出す前に

- 着手前に `git status` を見て、無関係な作業を巻き込まない([AGENTS.md](../AGENTS.md))。
- コアを変えたら `cargo test` で `bindings.ts` を生成し直す(忘れると CI が落ちる)。
- **コミットメッセージは Conventional Commits の接頭辞 + 日本語の要約。**
  実績のある接頭辞は `feat:` / `fix:` / `docs:` / `refactor:` / `style:` / `build:` / `perf:` /
  `chore:`。スコープを付けてよい(`fix(ci):`)。**方針の撤回や機能の削除は `feat!:`**
  (例: 下書き状態の廃止、関連づけを AI の領分へ)。
- **要約には「何をしたか」だけでなく「なぜ」と「実測」を書く。** このリポジトリのログは
  そういう形をしていて、ADR に上げるほどではない判断の唯一の記録になっている。
  本人決定に基づく変更は `(2026-08-11 本人決定)` のように日付と出所を添える。

## 9. まだ機械化できていないもの(次の一手)

**この節がこの文書の生存戦略。** 現在、§2〜§8 に機械へ上げられる既知の残件はない。
新しく見つけたら、上げ方と一緒にここへ置き、機械化した時点で対応する条文を本文から消して
§1 の表へ移す。
