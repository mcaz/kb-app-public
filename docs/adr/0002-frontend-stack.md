# ADR-0002: フロント = React + Tailwind、コンポーネントは Atomic Design のディレクトリ単位

- 状態: **採用**(2026-08-11 本人決定)
- 関連: [ADR-0001](0001-core-language.md)(UI は TypeScript・使い捨ての外殻)/
  [requirements.md](../requirements.md) 原則6

## 文脈

v0.2〜v0.4 の UI は素の TypeScript + DOM 直書きで実装した(`app/src/main.ts` 単一
1425行)。**規模が小さいうちに整理する**という判断で、実装前に現状を計測した:

- **全再描画モデルが限界**。`render()` がシェルごと DOM を作り直すため、その副作用を
  手作業で埋め合わせるコードが各所に生えている:
  - スクロール位置を `state.listScroll` / `noteScroll` / `dashScroll` へ退避し
    `requestAnimationFrame` で復元(3箇所)
  - ページャだけは全再描画を避けるための別経路(`fillRecent` / `fillTags` / `showItems`)
  - パネル幅を localStorage 経由で再適用
  - **実バグ**: タグを1つ選ぶと再描画で入力欄が作り直され、フォーカスが飛ぶ
    (連続してタグを打てない)
- `state` が4種の関心を1袋に混ぜている(一時UI状態 / localStorage 永続 /
  サーバキャッシュ / 選択)。localStorage のキーは6箇所に文字列直書き
- `ipc.ts` 294行のうち約140行がブラウザプレビュー用デモデータで、本番バンドルにも入る
- 型は Rust の構造体を手写し。`invoke` の引数名タイポはコンパイルで捕まらない
- ピボット(2026-08-10 の一本化)の残骸: CSS に `.inbox` / `.prop-card` /
  `.edit-split` / `.related-section` / `.lg-canvas`、`note_save` / `note_new` /
  `note_delete` / `note_make_mine` は UI から未使用
- モーダル・コンボボックス・ツールチップ・トーストが自前実装で壊れている
  (ESC 不発、フォーカストラップなし、ドロップダウンは `setTimeout(150)` で閉じる、
  `aria-*` ゼロ)
- リンタ・フォーマッタ・テストなし

## 決定

### 1. UI フレームワーク = React 19

**判断軸は「今後のアプリ化・マルチデバイス化・拡張性」+「本人が Svelte 未経験」**。

Tauri v2 のモバイルはどのフレームワークでも同じ WebView が動くので互角。差が出るのは
その先で、ネイティブが必要になったとき React Native / Expo に思考モデルを持ち出せる
(Svelte にこの経路は実質ない)。加えて **このアプリのコードは Claude が書く量が多く、
React は学習データが厚いため出力が安定する** — 未経験の言語では、混入した古い書き方を
レビューで弾けない。ここが決め手。

### 2. スタイル = Tailwind v4 + tailwind-variants、部品 = shadcn/ui

- **見た目は現行を維持する**。`docs/ui-draft.html` 由来のパレット(紙の地色・育つ緑・
  提案の琥珀・ダークモード)は製品の識別子であり、移行で変えない。今回の差分は
  「中身の入れ替え」に限定し、見た目の改善は別件に切る(差分の原因を切り分けられる)
- **shadcn/ui はライブラリではなくソースをリポジトリに所有する方式**。依存が肥大せず、
  気に入らなければ直接書き換えられる。テーマは CSS 変数なので現行パレットをそのまま流し込む
- **バリアントは `tailwind-variants`(`tv`)に統一**。shadcn が同梱する `cva` は
  移入時に `tv` へ置き換える。`slots` があるため多パーツのコンポーネント
  (一覧項目・カード等)を1箇所で定義できる
- **MUI / Ant Design / Mantine は棄却**。見た目を上書きされて製品の識別子が消えるうえ重い
- **Base UI + CSS Modules(見た目ゼロの部品 + 素の CSS)も候補だったが棄却**。
  a11y は同等に解けるが、全部品のスタイルを自前で当てることになり、原則6
  「外殻は使い捨て」に対して投資が重い

### 3. 状態 = TanStack Query(サーバ由来)+ Zustand(画面の都合)

役割分担を **「サーバ由来 = Query / 画面の都合 = Zustand」** で割り切る。いま `state` に
4種が混ざっているのが読みづらさの根であり、`state.tagOv` / `graphCache` の手書き
キャッシュと、各所に散った `await refreshHome(); render()` はすべて Query 側へ寄せる。

### 4. 型は tauri-specta で Rust から生成

`lib/bindings.ts` は生成物。**手書き禁止**。コアの構造体を変えたら生成し直す。
デモデータは `lib/demo/` へ分離し、Tauri 外(ブラウザプレビュー)でのみ読み込む。

### 5. コンポーネントは Atomic Design。境界は「副作用を持てるか」で切る

層の議論で消耗しないよう、判定を機械的にする:

| 層 | 許されること | 禁止 |
|---|---|---|
| atoms | DOM を1つ包むだけ | ドメイン型(Note/Tag/Favorite)を知ること |
| molecules | atom の組み合わせ。ドメイン型を props で受ける | データ取得・変更・グローバル状態 |
| organisms | `useQuery` / `useMutation` / zustand に触れる | — |
| templates | 配置のみ。`children` を受ける | ドメイン型を知ること |
| pages | 画面の組み立て、ビュー状態の入口 | — |

**副作用を持てるのは organisms 以上だけ**。これが現状の病巣(副作用が全レイヤに散在)への
直接の処方箋であり、この規約こそが Atomic Design を導入する理由。

### 6. 1コンポーネント = 1ディレクトリ。私物ファイルは同居させる

ファイル直置きは構造が汚れるため、層の下にコンポーネント名のディレクトリを作る:

```
components/<layer>/<ComponentName>/
  index.ts              ← 公開面。外部からはこれ以外を import しない
  <ComponentName>.tsx   ← 本体
  variants.ts           ← tailwind-variants の tv() 定義
  <private>.ts(x)       ← そのコンポーネント専用のフック・小部品・型
  <name>.module.css     ← Tailwind で表現できない部分だけ(あれば)
```

そのコンポーネントだけが使う下位部品は、同じディレクトリの下に入れ子で置く。
**外から見える面は `index.ts` だけ**(ディレクトリ内への直接 import は禁止)。

### 7. 多言語(日本語・英語)を前提にする

- **react-i18next**。文言は `src/i18n/locales/{ja,en}/<画面>.json` に画面ごとの
  namespace で置く
- **日本語が正本**(NFR-5: 日本語第一級)。`fallbackLng: "ja"` — 英語に未訳のキーが
  出たら空表示にせず日本語で見せる
- **キーは型で守る**。`i18next.d.ts` の module augmentation で ja のリソースから
  キーを型付けし、存在しないキーは `npm run typecheck` で落ちる。
  対訳の欠落・余剰・空文字は `locales.test.ts` が落とす
- 日付は `Intl.DateTimeFormat`、タイトル整列は現在ロケールの `localeCompare`
- 言語は OS/ブラウザのロケールから検出し、`prefs` ストアに永続する

**エラー文言**: Tauri 層は `AppError` で種類を型にしたので、画面は `code` で
訳し分けられる(決定10)。ただし kb-core は anyhow のままなので、**Tauri 層が
文脈を知らないエラーは `unexpected` に落ち、コアの日本語文言をそのまま運ぶ**。
英語環境で日本語が混じる余地はここだけ残っている(残課題)。

### 8. テーマ(ライト / ダーク / システム)は data 属性で切り替える

配色は `<html data-theme="light|dark">` だけを見る。**「システムに従う」の解決は
JS 側に一本化**し(`lib/theme.ts`)、CSS には `prefers-color-scheme` の分岐を置かない
— 同じ配色を2箇所に書いてズレる事故を作らないため。Tailwind の `dark:` も
`@custom-variant` で同じ属性に向ける。

- 設定は `prefs.theme`(`system` / `light` / `dark`)。既定は `system`
- 起動時は描画前に `applyTheme()` で属性を貼る(ライトで一瞬出てから切り替わるのを防ぐ)
- OS 側の切り替えは `useSyncExternalStore` で購読(effect 内 setState による再描画の
  連鎖を作らない)
- **つながりグラフは canvas で、生成時に CSS 変数を読んで色を決める**ため、
  テーマが変わったら作り直す(`useEffect` の依存に解決後のテーマを入れてある)

### 9. アイコンは lucide-react

`atoms/Icon` に包み、太さ(`strokeWidth=1.75`)とサイズ(sm/md/lg)の既定をそこへ閉じ込める。
lucide 既定の 2 はこのパレット(紙の地色・細い文字)には太い。

絵文字をやめた理由は **OS によって字形が変わること**(Windows 対応を入れたため)。
副産物として、絵文字では区別が付かなかった2つの意味が分かれた:
**「繋ぐ」= `Plug`(外部アプリとの接続)/「つながり」= `Link`(ノート間リンク)**
— どちらも 🔗 だった。

文言に混ざっていた絵文字は locale JSON から剥がし、アイコンはコンポーネント側に置く
(翻訳者が記号の面倒を見ない形にする)。

### 10. Tauri 層は commands / state / error に分ける

`src-tauri/src/lib.rs` も単一570行で、フロントと同じ症状だった。標準的な構成に寄せる:

```
src-tauri/src/
  main.rs      入口だけ
  lib.rs       Builder の組み立てだけ
  error.rs     画面へ返すエラー(種類を型にする)
  state.rs     vault と索引接続の共有
  mcp_mode.rs  同じ実行ファイルを MCP サーバーとして動かす経路
  commands/    invoke で呼ばれる関数。機能ごとに分割
```

- **接続を共有する**。以前は `Vault::open` 16箇所・`open_db` 11箇所で、**ノートを
  1本開くだけでも vault と DB を開き直し、索引の全体 sync まで走っていた**。
  `AppState` に集約し、生成は遅延(未オンボーディングでも起動できるように)
- **索引 sync は用途で分ける**。一覧・検索・ホームは `Sync::Force`(鮮度が要る)、
  それ以外は3秒のスロットリング。取りこぼしても TanStack Query の再取得で追いつく
- **エラーは型にする**(`AppError`)。specta が TS へ判別可能な union を出すので、
  画面は `code` で訳し分けられる。**ただしコアは anyhow のままなので、分類できるのは
  Tauri 層が文脈を知っている場合だけ**。それ以外は `unexpected` に落ち、コアの
  日本語文言を運ぶ(残課題は「ほぼ塞がった」であって「塞がった」ではない)
- **長い処理は同期コマンド + イベント**。Tauri は同期コマンドを別スレッドで動かすが、
  async コマンドは async ランタイム上で動く。`embed_enable` は数分ブロックするため
  async にするとランタイムを止める → 同期に直し、進捗を `EmbedProgress` で流す

### 11. 規律は lint で強制する

層の規約(5)とディレクトリ規約(6)は `eslint.config.js` の `no-restricted-imports`
で機械的に落とす。**文章だけの規約は必ず破られる**ため
(KB「AI 協働アプリの規律は文章でなく機構で確定させる」と同じ判断)。

- molecules から `@tanstack/react-query` / `zustand` / `@/lib/queries` を import → エラー
- 下の層から上の層を import → エラー
- コンポーネントの内部ファイルを直接 import → エラー
  (shadcn が平置きで吐く `atoms/ui` だけ対象外)

ほかに typescript-eslint(型情報あり)・react-hooks・jsx-a11y、整形は Prettier
(+ Tailwind クラス並べ替え)。`npm run check` で 整形 → lint → 型 → テスト を通す。

### 12. CI は GitHub Actions の2ジョブ

- **frontend**: `format:check` → `lint` → `typecheck` → `test` → `vite build`
- **rust**: `cargo fmt --check` → `clippy -D warnings` → `cargo test` →
  **`git diff --exit-code app/src/lib/bindings.ts`**

最後の1つが要点で、`cargo test` が bindings を生成し直すため、
**コアを変えて TS 型の再生成を忘れると CI が落ちる**。

## 影響

- `app/src/main.ts`(1425行)・`ipc.ts`・`style.css` は廃止し、上記構成へ移す
- グラフ描画(d3-force + canvas)は React 非依存のまま `lib/graph/` へ移す
- ピボット残骸(死んだ CSS・未使用 API・未使用 Tauri コマンド)は移行と同時に落とす

## 残課題(この決定から派生したもの)

GitHub Issues は使っていないため、ここに置く。

- **バンドルが単一チャンク 634KB(gzip 198KB)**。ローカルアプリなので実害は無いが、
  画面ごとの `lazy()` 分割か `build.rolldownOptions.output.codeSplitting` で分けられる。
  重いのは d3-force・marked・Radix 一式。**着手の合図は「起動が遅い」と感じたとき**で、
  それまでは分割しない(計測せずに分けても複雑になるだけ)
- **kb-core を型付きエラーにする**。Tauri 層は `AppError` で分類できるようになったが、
  コアが anyhow のままなので、文脈を持たないエラーは `unexpected` に落ちて
  日本語文言をそのまま運ぶ。英語環境で残る最後の穴はここ
- **設定画面の中身**。いまは「見た目(テーマ・言語)」だけの最小形。
  vault の切り替えなど、置く候補が出たときに広げる

## 検討した代替案

| 案 | 判定 |
|---|---|
| Svelte 5 + スコープ付き CSS | **棄却**。記述量が最少で CSS の腐敗も構造的に止まるが、本人未経験で LLM 出力(runes と旧4系の混在)をレビューで弾けない。ネイティブへの逃げ道も無い |
| Preact + signals | **棄却**。最小構成だが、CSS のスコープ化は別途必要で、拡張性の在庫も React に劣る |
| 素の TS のまま、モジュール分割だけ | **棄却**。依存は増えないが、全再描画の手当てとフォーカス飛びを自前で解き続けることになる |
