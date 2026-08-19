# ADR-0011: semantic executor v1をsnapshot再照合付きatomic waveにする

- Status: Accepted
- Date: 2026-08-20

## 文脈

[ADR-0010](0010-read-only-distillation-planner.md)で、全DB documentのinput hash、snapshot digest、
決定的plan IDを持つread-only plannerを導入した。planを得た後に通常の単一note `update`を順番に
呼ぶだけでは、途中失敗で一部だけ更新され、plan作成後の変更を見落とし、同じwaveを二重実行できる。
canonicalと根拠recordを同時に更新する蒸留では、この境界は成立しない。

一方、既存ノートの作成・削除・merge・supersede・split・path移動は別々の不変条件を持つ。特に削除は
[ADR-0008](0008-two-phase-mcp-removal.md)の対象固定tokenを、supersedeはactive canonical一意性と後継edgeを
同じtransactionで遷移させる専用操作を必要とする。v1 executorへ汎用raw writeとして混ぜると、既存の
安全境界を迂回する。

## 決定

### 1. 実行直前にplan全体を再計算する

`apply_distillation`は次をrequestに必須とする。

- execution request schema
- plan schemaとplanner profile
- plan ID
- snapshot digestとnote count
- 各対象のnote ID、input hash、plan operation、変更理由、構造化target

executorは同じSQLite write transaction内で`mechanical-v1` planを再計算し、schema、profile、plan ID、
snapshot digest、note count、各input hash、各operationをすべて照合する。1項目でも違えば書き込み前に
wave全体を拒否する。plan確認と最初のwriteの間に別transactionが入った場合もSQLite snapshotの昇格失敗で
commitできず、古いreadをwriteへ持ち上げない。

### 2. v1の能力を既存ノート3操作へ閉じる

v1が受け付けるのはauthority envelopeを持つ`origin: agent`の既存ノートだけとする。

- `normalize`: 空でないdescriptionの追加だけ。title、body、tags、relationsは変更不可
- `revise`: active canonicalのtitle、body、description、tags、relations
- `extract`: recordのdescriptionとrelations。原証拠であるtitle、body、tagsは変更不可

`note_uid`、authority、status、origin、created、sources、未知frontmatterは常に保持する。既存タグ語彙、
typed relation、canonical一意性は通常writeと同じcore validationを通す。create、delete、merge、supersede、
split、archive、legacy authority backfill、path移動は入力schemaへ能力を置かない。

### 3. 1 requestを1 transactionと1 execution identityにする

対象をnote ID順へ正規化し、request JSONのSHA-256を`execution_id`にする。同じrequestは
`distillation_runs.execution_id`の一意制約で二重実行を拒否する。全noteと派生索引を更新し、Markdown
durable outbox、実行前後document、前後snapshot、client、時刻を同じtransactionへ積む。途中の1件が
失敗した場合はnote、索引、outbox、execution recordをすべてrollbackする。

DB commit後のMarkdown出力失敗は確定済みDBを巻き戻さず、`markdown_export` degradationとpending outboxを
返す。通常の再接続でoutboxを回復する。

### 4. rollbackも後続変更を許さないatomic waveにする

`rollback_distillation`は保存済みexecution IDを受ける。次のすべてを満たす場合だけ、全対象を実行前
documentへ同じtransactionで戻す。

- statusが`applied`で、まだrollbackされていない
- pending outboxがない
- 現在の全DB snapshotがexecution直後のsnapshotと一致する
- 対象documentが保存済みafter documentと一致する
- 復元後のsnapshotとplan IDが実行前と一致する

したがって、対象外noteを含む後続変更が1件でもあれば自動rollbackは停止する。二重rollbackも拒否する。

### 5. 監査の正本と回復範囲

各Markdown exportの`log.md` entryとGit commit messageへplan／execution／rollback IDを残す。Git履歴が
内容回復の正本で、fresh cloneでも実行前後のdocumentを確認・復元できる。SQLiteの
`distillation_runs`は、現端末での二重実行拒否と即時atomic rollbackに使うruntime記録であり、DB binaryを
Git同期する理由にはしない。別端末・fresh cloneでの自動rollback manifestは、atomic supersede等と合わせる
後続段で設計する。

planもexecutionも人間承認queueではない。AIは方針内の対象を全文確認してrequestを組み、MCPから自律実行する。

## 強制点

- `kb-core::distillation_executor`: request正規化、snapshot再照合、operation制限、execution ID、atomic apply／rollback
- `kb-core::note_store::queue_put`: 既存write validationとdurable outboxを外側transactionへ合流
- SQLite schema v5: `distillation_runs`
- MCP: `apply_distillation` / `rollback_distillation`をdestructive・non-idempotent・closed-worldとして公開
- CLI: `kb distill apply --input` / `kb distill rollback --execution-id`
- Schema: `schemas/distillation-execution*.schema.json`
- 自動テスト: stale plan、二重実行、record不変、途中失敗の全rollback、正常rollback、二重rollback、MCP境界

## 帰結

- canonical reviseとrecord lineage整理を一部成功なしで同時実行できる
- planner以後に1件でも変わったwaveは自動停止する
- 同じrequestの再送で重複変更を作らない
- deleteやsupersedeの既存・後続安全境界を汎用executorが迂回しない
- semantic merge、atomic supersede、新規extract／split、legacy backfill、別端末自動rollbackは次段に残る
