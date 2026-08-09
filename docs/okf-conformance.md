# OKF 適合設計 v0(spec 精読の結果)

2026-08-09 起草。[requirements.md](requirements.md) FR-C1「OKF 互換基本方針」の実行 —
技術設計の最初のタスクと位置づけた spec 精読と適合設計。
spec 原本: https://github.com/GoogleCloudPlatform/knowledge-catalog/tree/main/okf (SPEC.md)

## 最重要の発見 — OKF は v0.2 に改版済み

要件定義時の前提(v0.1・二次記事ベース)から spec が進んでいた。読んだのは
**v0.2**(spec リポ最新コミット 2026-08-07 時点)。v0.1 からの差分がまさに
kb-app の中核関心に重なる:

> v0.2 は provenance(出所)・trust(信頼)・lifecycle(状態)・attestation(検算)を
> frontmatter の第一級フィールドにした。「エージェントが書き続けるコーパス」を
> 明示の前提にしている。

**帰結: kb-app が「app 固有拡張」として設計する予定だったメタの大半が、標準語彙に
なった。** 拡張フィールドはほぼ不要になり、「ロックインしない」の外部証明は
要件定義時の想定より強く成立する。

| kb-app の要件 | 要件定義時の想定 | v0.2 での表現 |
|---|---|---|
| draft → 確定のライフサイクル | app 拡張 `status: draft` | **標準** `status: draft \| stable \| deprecated`(§5.4) |
| 受信箱の「承諾」 | app 独自の状態遷移 | `status: stable` 化+**標準** `verified: {by: human:<id>, at}` 追記(§5.2)。trust tier「human-reviewed」に一致(§5.3) |
| 会話由来の出所記録 | `source: session:...`(自 KB 規約) | **標準** `sources[].resource`(追跡不能な scope descriptor も許容、§5.1) |
| 書き手の区別(原則9) | app 拡張 | **標準** `generated.by` の actor 規約(§7): `human:<id>` / `<クライアント>/<モデル>`。ただし後述の通り「生まれ」は別途1キー必要 |
| お手入れの鮮度検出 | app 独自 | **標準** `stale_after: YYYY-MM-DD`(§5.5) |
| 操作記録(バックアップ・確定) | app 独自ログ | **予約ファイル** `log.md`(§9)に人間可読で併記できる |

## vault リサーチノートの「未確認3点」への回答

1. **拡張フィールドの流儀**: 名前空間規約は無い。producer は任意キーを追加してよく、
   consumer は未知キーを**保持し、拒否してはならない**(§4.1 Extensions)。
   → 拡張は自由だが、将来の OKF 標準キーと衝突しない命名を選ぶ責任は producer 側
2. **リンク記法**: `[[id]]` は仕様に**無い**。標準 markdown リンクのみ。
   バンドル相対の絶対形 `/notes/foo.md` が推奨形(§6.1)。リンク切れは不正では
   ない(未執筆の知識を表しうる。consumer は許容必須)
3. **resource の意味論**: 概念が記述する実体資産の正準 URI(§4.1)。抽象概念には
   不要。出所側は `sources[].resource`(URL / バンドル相対パス / scope descriptor)

## kb-app ノートの frontmatter 詳細設計(案)

概念 ID = パス(`.md` 抜き)。frontmatter に `id` フィールドは置かない(パス=ID を
そのまま採用。FR-C1 と整合)。全キーはアプリが生成・検証し、ユーザーには見せない(原則7)。

| キー | 由来 | 書くのは誰 | 内容 |
|---|---|---|---|
| `type` | OKF 必須 | app | 当面全ノート `Note` 固定(語彙拡張は未決へ) |
| `title` | OKF 推奨 | ユーザー / AI | 表示名 |
| `description` | OKF 推奨 | AI(提案) | 一文要約。index 生成・検索スニペットに使用 |
| `tags` | OKF 推奨 | AI 提案 → 承諾 | 分類方針(requirements FR-C1)の中分類 |
| `status` | OKF §5.4 | ライフサイクル API のみ | propose = `draft` / 承諾 = `stable` / 退役 = `deprecated` |
| `generated` | OKF §5.2 | app(保存時に自動) | `{by, at}`。actor 規約: ユーザー編集 = `human:<ローカルID>`、AI 起票・編纂 = `<クライアント>/<モデル>`(例 `claude-desktop/claude-fable-5`) |
| `verified` | OKF §5.2 | app(承諾時に追記) | 受信箱・お手入れの承諾履歴。人間の承諾 = `human:` actor |
| `sources` | OKF §5.1 | AI(propose 時) | 会話由来なら `resource: "conversation:<クライアント>/<日付>"`(scope descriptor)。外部 URL 由来ならその URL |
| `stale_after` | OKF §5.5 | お手入れ(任意) | 期限のある知識にだけ付く |
| `origin`(仮) | **app 拡張(唯一)** | app(作成時のみ) | `human` = メモ(聖域)/ `agent` = 育つノート。**越境の明示操作でのみ変更** |

### なぜ `origin` だけは拡張が要るか(原則9 の機構化)

`generated.by` は「**最後の**意味ある変更を書いた者」であり「生まれ」ではない。
育つノートをユーザーが手直しすると `generated.by` は `human:` になり、書き手由来の
種類判定が反転してしまう。git の初回コミット著者から導出する案は棄却 —
正本はファイルであり、git を持たない配布形(tarball、§3)や再構築(原則1)で
消える情報を種類判定の根拠にできない。よって「生まれ」は frontmatter に1キーだけ
持つ。命名は requirements 未決「ノート2種類の呼び分け」と同時に決める。

### 適合宣言と予約ファイル

- バンドル(= vault)ルートの `index.md` に `okf_version: "0.2"` を宣言(§12)。
  `index.md` は人間規約ではなく**アプリが自動生成**する(progressive disclosure、§8)。
  自 KB の「INDEX 規約を人間が維持する」統治史をアプリ側に持ち込まない
- `log.md` を承諾・バックアップ等の操作記録の人間可読面として自動追記(§9)。
  機械的な監査痕跡は git 履歴が正で、log.md はその可読ビュー
- `index.md` / `log.md` は概念ノート名として使用禁止(§3.1)— lint で検査

### リンク(つながり)の正準形

保存形は標準 markdown リンク・バンドル相対 `/notes/foo.md`(§6.1 推奨形)。
`[[wikilink]]` は採用しない(ゼロベースなので移行問題なし。既存 KB の将来移行では
`[[id]] → markdown リンク` の機械変換が互換レイヤの仕事になる)。
UI はリンクをタイトルで表示し、パスは見せない(原則7)。リンク切れはエラーに
しない(§6.1)— お手入れ(FR-C7)が「未執筆の知識」として提案に変える。

### conformance 検査(lint の中核)

§11 の適合条件をそのまま検査器にする:

1. 予約名以外の全 `.md` が parse 可能な frontmatter を持つ
2. `type` 非空
3. 予約ファイルが §8 / §9 の構造に従う

加えて consumer 側の寛容規則(未知 type・未知キー・リンク切れで拒否しない)を
アプリ自身が守る。**検査はアプリの自己修復(外部 git 操作後の再索引)と同じ経路で
走らせ、違反は「壊れたら見える」(原則4)に従い画面に出す。**

## 版追随戦略

- **書き側は v0.2 に固定**(`generated` / `sources` を最初から使う。v0.1 の
  `timestamp` / `# Citations` では書かない)
- **読み側は v0.1 フォールバックを実装**(`generated` 不在時に `timestamp`、
  `sources` 不在時に `# Citations` を読む — §13.1 が consumer に推奨する形)。
  外部バンドル取込み(将来のチーム共有・OKF エコシステム相互運用)への備え
- v0.1→v0.2 で minor と言いつつ breaking 2点が実際に起きた。「ハードロックせず
  追随余地を残す」方針は正しかった — 適合ロジックはコア内の1モジュールに隔離し、
  版差分をそこに閉じ込める

## スコープ外と判断したもの

- **Attested Computation**(§10): データカタログ文脈の機能。個人 KB の v1 には
  不要。プラグイン(FR-A8)の将来ネタとして記録のみ
- **per-claim attribution**(footnote 記法、§5.1): propose の生成品質に依存する
  細部。v1 は `sources` のノート単位付与まで。本文 footnote は将来の propose 強化で

## 新たな未決(requirements 未決へ追記)

- `origin`(仮)の命名 — 「ノート2種類の呼び分け」の命名と同時に決める
- ローカルユーザーの actor ID(`human:<何>`)— アカウント概念を持たないアプリで
  何を ID にするか(マシン名? 設定時の表示名? 匿名 `human:owner`?)
- `type` 語彙の拡張タイミング(全部 `Note` で始めて、いつ分けるか)
