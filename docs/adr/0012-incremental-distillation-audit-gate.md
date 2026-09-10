# ADR-0012: 継続蒸留を増分worksetと全体受入gateで再開可能にする

- Status: Accepted
- Date: 2026-08-20

## 文脈

[ADR-0010](0010-read-only-distillation-planner.md)と
[ADR-0011](0011-snapshot-bound-semantic-executor.md)により、同一snapshotの候補列挙と既存ノートの
atomic semantic waveは機構化された。しかし、毎回129件の全ノートを読み直さなければ安全性を説明できない
状態では継続運用のコストが高く、途中中断からの再開位置もAIの会話記憶に依存する。一方、前回からの差分だけを
見て「健全」と判定すると、未処理候補、Storage Contract違反、Markdown outbox、未バックアップcommitを
見落とす。

増分checkpointを実行権限や承認状態にするとstale plan拒否を迂回し、廃止済みのdraft queueも復活する。
また、read-only監査のたびにremote pullやnetwork照会を行うと、同じローカル状態の再実行が外部状態に左右される。

## 決定

### 1. 前回checkpointとの差分をstable identityで作る

`audit_distillation`と`kb distill audit`は、現在の`mechanical-v1` planから
`kb-app.distillation-checkpoint/v1`を返す。次回はそのcheckpointを任意のbaselineとして受け、次を辞書順で
分類する。

- `added` / `changed` / `removed`
- 不変`note_uid`が同じでpathだけが変わった`moved`
- 内容とpathが変わらない`unchanged`

identityはauthority付きノートでは`note_uid`、legacyではnote IDをfallbackにする。checkpoint全体は
`checkpoint_id`で自己digest化する。schema、planner profile、件数、hash、自己digest、note ID、stable
identityの重複を検証し、改変・曖昧なbaselineは監査前に拒否する。

### 2. worksetは差分と現在候補の依存グラフ閉包にする

AIが全文確認する`workset`のseedは、追加・変更・移動された現存ノートと、現在planで`keep`以外または
`risk != none`の全ノートとする。seedから`depends_on` edgeを両方向に閉包し、canonicalと根拠recordの
片側だけを読んで判断しない。削除済みnoteは`removed`へ残すが、現在本文がないためworksetには入れない。

baseline無しの初回は全ノートをworksetにする。これにより初期完全監査と、その後の増分再開を同じ出力契約で
扱える。

### 3. 受入判定は増分ではなく現在の全体状態へかける

`gate`はworksetの小ささと独立に、現在の全planと派生状態について次をすべて検査する。

- actionable entryが0
- unresolved entryが0
- `none`以外のriskが0
- pending Markdown exportが0
- Storage Contractが検証できる
- remote未設定のlocal-only、またはlocal Git上の未backup commitが0

1件でも失敗すれば`ATTENTION`とし、codeと違反件数を機械可読に返す。remote検査は設定有無とlocal Gitの
aheadだけを読み、pull、fetch、GitHub API等のnetwork I/Oは行わない。remoteのprivate性・到達性・実際の
最新性を能動確認する検査はこのgateの保証外とする。

2026-09-08、Issue #88: StorageReportの`legacy_files`は引き続き物理総数。
`legacy_inventory`で未昇格Artifact数と、現役・保持・未分類の物理path数を分ける。
新昇格の構造化証拠と現在の参照・実体が一致したものだけを保持へ数える。証拠の壊れたJSON、
保持を記録した実体の欠損・改変はStorage Contract失敗とし、旧散文しかない実体は未分類に残す。
詳細は[Artifact監査](../artifact-audit.md)。

cadence runがbaselineを受け入れる際は7番目の`artifact_metadata_stable`検査を追加する。
監査前後の台帳識別値が一致しなければ失敗させ、変更後のgateを含めてaudit IDを再計算する。
standalone auditは従来の6検査を返し、それだけでcadenceの受入baselineを進めない。

### 4. auditとcheckpointはread-onlyで実行権限を持たない

監査は準備済みDBをread-onlyで開き、remote pull、schema migration、索引同期、care、outbox更新を行わない。
出力全体から時刻を除いた決定的`audit_id`を作る。同じ現在状態とbaselineなら同じJSONとIDになる。

checkpointは再開用の比較材料であり、`apply_distillation`へ渡す実行tokenではない。executorは従来どおり
現在の全planをwrite transaction内で再計算し、snapshotと全input hashを照合する。gate PASSも自動writeの
権限にはしない。

## 強制点

- `kb-core::distillation_audit`: checkpoint検証、stable identity差分、依存閉包、全体gate、決定的audit ID
- MCP: `audit_distillation`をread-only / idempotent / closed-worldで公開し、remote syncを抑止
- CLI: `kb distill audit --baseline <audit-or-checkpoint.json> --format json|markdown`
- Schema: `schemas/distillation-audit.schema.json`
- 自動テスト: 初回full、同一checkpointの空workset、path move、依存閉包、gate失敗、DB無書き込み、MCP境界

## 帰結

- 継続蒸留は会話が中断してもcheckpointから再開できる
- 平常時にAIが読む集合を差分へ縮めつつ、全体の未処理・派生状態driftを見逃さない
- schedulingとcheckpointの永続保管は[ADR-0013](0013-persistent-distillation-cadence.md)の
  端末ローカルcadenceへ分離し、audit自体の決定性を維持する
- 検索Golden Queryの継続評価、active remote health、semantic判断、atomic supersede・moveは別の強制点として残る
