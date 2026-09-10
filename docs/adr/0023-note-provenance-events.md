# ADR-0023: ノートの来歴を、本文と分離した追記専用イベント台帳に持つ

- 日付: 2026-09-10
- 状態: 採用(Phase 1 / Phase 2)
- 関連: [契約20](../contract.md) / [ADR-0004](0004-storage-contract.md)(正本の定義)/
  [ADR-0007](0007-client-surface-and-conversation-events.md)(client surface)/
  [ADR-0009](0009-canonical-authority.md)(note_uid)

## 背景

ノートは複数のAIが順番に育てる。しかし OKF frontmatter の `generated` は
「最後の書き手」1件しか持てず、次の書き手が上書きすると前の書き手が消える。
その結果、いま画面に見えているのは常に直近の1人で、

- ある段落を誰が書いたのか
- ある記述がいつ・なぜ入ったのか
- 同じ主張を別のモデルが繰り返しているのか、1回しか出ていないのか

を後から引けない。蒸留と検索がノート本文を主な入力にしている以上、
この情報を本文へ書き戻すのは選べない — 履歴が本文に混ざると、検索の当たりも
蒸留の判断材料も履歴文で汚れる。

## 決定

ノートの書込1回を、ノートと**分離した追記専用イベント**として正本に残す。

1. コアの書込経路(`note_store::put` / `queue_put` / `delete`)だけがイベントを発行し、
   ノート行・export outbox と**同じ transaction** で確定させる。呼び出し面が
   省略できる引数にはしない(省略できる記録は、いずれ必ず欠ける)。
2. イベントは本文を複製しない。持つのは書き手・operation・改版種別・変更のあった見出し・
   frontmatter の差分・本文の unified diff(上限 8KiB、超過は省略と明示)・前後の
   document hash。復元に必要な全文は、ノート正本と Git 履歴の側にある。
3. 正本は vault 内の `.kb-events/YYYY-MM.jsonl` へ1行追記し、Markdown 出力と
   同じ commit に載せる。`.gitattributes` に `merge=union` を張り、端末間で並行に
   追記されても片方を捨てない。既存行の改変・削除はコアの操作として持たない —
   訂正は新しいイベントを足して表す。
4. DB の `note_events` は durable table(schema v9)。派生索引ではないので再構築対象に
   せず、失われたら `.kb-events` から復元する。
5. 書き手は「製品名・モデル」と**それを何から得たか**(handshake / アプリ確定 /
   自己申告 / 設定値 / 不明)を一緒に持つ。同じ文字列でも確からしさは同じではない。
   Phase 1 は `--client` の設定値と、アプリ主導書込のアプリ側指定までを扱う。

## 却下した案

**frontmatter に `revisions:` を積む。**
ノート1ファイルで完結し、Obsidian からも読める点は優れていた。しかし本文と同じ
ファイルが単調に太り、蒸留・検索・埋め込みの入力に履歴が混ざる。
`merge=union` も frontmatter には効かず、端末間の並行追記が YAML 破損になる。

**DB 専用表だけに持つ。**
書きやすく速い。だが `.kb/index.db` は Git に入れず、契約6で明示的に**派生**と
決めている置き場所で、fresh clone で消える。「消えても再構築できる」条件を
満たさない情報を派生側に置けない。

**Git の commit trailer だけに持つ。**
既に commit を作っているので追加コストがほぼ無い。しかし1 commit = 1 ノート操作の
粒度しか持てず、見出し単位の帰属が入らない。さらに Git 履歴は再 clone・rebase・
将来の backend 差し替えで形が変わる前提(ADR-0004)で、Storage Contract の
snapshot からも読めない。

**vault の外(アプリのデータ領域)へ sidecar として置く。**
vault を汚さない。だが端末間で同期されず、バックアップにも入らない。
「どのAIが書いたか」は共有したい情報なので、端末ローカルは要件を満たさない。

## 技術的にはこちらが優れていた

**本文 diff の生成は `similar` crate の方が優れている。** word 単位・行単位の
アルゴリズム選択、unified 形式の細かな制御、Rust らしい API のいずれでも
libgit2 の `Patch` を上回る。それでも既存依存の `git2::Patch::from_buffers` を
選んだのは、この作業環境がネットワーク遮断で新規 crate を取得できないことと、
差分1機能のために依存を1つ増やすとレビュー面が広がるため(coding-guidelines §7)。
`git2` は Markdown 出力の commit で既に使っており、追加の学習面も増えない。
差分の見た目に不満が出たら、この判断だけを差し替えられるよう生成は
`provenance::body_diff` の1関数に閉じてある。

**shard は日別の方が競合が少ない。** 月別 `YYYY-MM.jsonl` は同じ月に触った
全端末が同じファイルを追記するので、`merge=union` の出番が増える。それでも
月別にしたのは、ノート数に対してイベント数が多く、日別だと年間365ファイルが
vault 直下に増えて Obsidian からの見え方が悪くなるため。union merge がある以上、
衝突は「両方残る」で正しく解決する。

## この段で持ち込まないもの

- 来歴を読む Tauri command と画面。Phase 1・2 はコア API と MCP までで、
  `bindings.ts` を増やさない。
- 来歴を根拠にした自動判断(重複検出・信頼度の重み付け)。まず記録を貯める。

## Phase 2(2026-09-10 採用)

Phase 1 で貯めた記録を、書く側と読む側の両方から使えるようにした段。

1. **名乗りの取り込み**: `initialize` の `clientInfo` を process 内で保持し、以後の書込の
   書き手を handshake basis へ上げる。名乗りは来歴の basis だけを上げ、能力判定・ON/OFF・
   観測台帳は設定値(`--client`)のままにする — 自己申告で機能や許可が変わると、
   名乗りがそのまま権限になる。
2. **改版の意味の申告**: `propose` / `update` へ任意の `actor.model` / `revision` /
   `origin_claim` を足す。どれも任意のまま(必須にすると経路が迂回される)。
3. **読む面**: `get` はリンク行の直後に1行、read 面の `history` は台帳と差分を DB だけから返す。
   本文へは書き戻さない。
4. **自動 retrieval**: 各 document に1行の来歴を添え、hook の出力予算に含める。

### 2026-09-10 追記: モデルの動作設定(`actor.mode`)

`codex-cli/gpt-5-codex` のようにモデル名だけを記録すると、どのモデルの**どの設定**
(推論強度・思考モード・ペルソナ名)で書いたかが分からない(本人指摘)。書き手に
`mode` を足し、表示名と frontmatter の `generated.by` を
`<製品名>/<モデル> <動作設定>`(例 `codex-cli/gpt-6-codex Astra medium`)の形にする。

- 動作設定はモデルに付く情報として扱う。`actor.model` と一緒に自己申告し、モデル無しの
  `mode` だけは書く前に拒否する。モデルを申告し直したら設定値の動作設定は引き継がない。
- `--client <製品名>/<モデル>/<動作設定>` の3番目の segment を設定値(`config`)として読む。
- basis はモデルと共有し、`mode` 用の basis は持たない。書き手の同一性キー(`client/model`)
  と `note_events` の列は変えず、`mode` はイベントの payload と JSONL の行に載る
  (無いときは鍵ごと出さないので、既存 shard の digest は変わらない)。
- `generated.by` の先頭 segment は `ClientSurface::from_hint` が読むため、handshake で
  名乗った名前ではなく接続設定の製品名を置く。
5. **検索**: 派生索引 `fts_events`(summary / reason / 見出し / 種別)を、
   historical / rationale intent の query でだけ弱い加点として合流させる。
   intent が立たない query の順位は変えない。
6. **移行**: `plan_provenance_backfill` / `apply_provenance_backfill` が、Git 履歴の
   `propose {id} (via {client})` から作成相当のイベントを、まだ1件も持たないノートにだけ
   生成する。月別 shard を書き換えず、`backfill-YYYY-MM-DD.jsonl` へ追記して同じ commit に載せる。
