# ADR-0016: reversible adapterのidempotency・reconcile・compensationを共通gateへ固定する

## 状態

採用（2026-08-21）

## 背景

ADR-0014はservice非依存のrequest／decision／capability／receiptを定義し、
ADR-0015は署名済み単回capabilityの消費とpending receipt確保をSQLite transactionへ
統合した。ただし、実在adapterに近いreversible writeを接続したときの外部idempotency、
実行開始後crash、状態不明、補償期限は未検証だった。

pendingを「未実行」とみなして再送すると二重作用になる。反対に、外部成功が確認できても
receiptを一度だけ確定できなければ、再起動や並行reconcileで強制状態が分岐する。

## 決定

### 1. 外部idempotency keyをpermitからadapterへ渡す

`ExecutionPermit`はreceipt、workspace、request hash、idempotency key、完全修飾targetへ
固定する。fixture adapterは同じidempotency keyの再送で同じobjectを返し、新しいobjectを
作らない。低水準のcreate操作は公開せず、permitを受け取るtrait境界だけを実装する。

### 2. reconcileは3値にする

実行開始済みpendingはadapterへ照会し、`succeeded`、`failed`、`still_unknown`の
いずれかを返す。unknownは成功にも失敗にも変換せず、pendingのまま保持し、自動再実行しない。
実行開始時刻が無いpendingだけは、外部呼出し前crashとしてadapterを呼ばずfailedへ確定する。

成功結果はexternal reference、external target、補償期限を必須とし、request targetとの
不一致、空reference、完了前に切れた補償期限を拒否する。failed結果が成功object情報を
主張することも拒否する。

### 3. reconcile確定は既存receiptへ一度だけ行う

外部照会はtransactionの外で行い、確定updateは`status='pending'`を条件にatomicに行う。
並行reconcileで成功できるupdateは1件だけで、後続結果による上書きを認めない。

### 4. compensationもreceipt固定permitを使う

reversible成功receiptだけが補償対象である。gateはworkspace、request hash、external
reference、external target、補償期限を固定した`CompensationPermit`を生成する。
期限切れ、target不一致、reference不一致、二重補償はfail-closedにする。fixtureの補償操作も
同じexternal referenceに対して冪等である。

### 5. runtime schemaを7へ上げる

`action_receipts`へ`external_target`、`compensation_deadline`、`compensated_at`を
追加する。既存schema 3〜6からは表と列だけを非破壊で追加し、note本文、埋め込み、
distillation履歴、既存receiptを作り直さない。

## 受入

- 通常実行と再送でfixture objectは1件
- 外部成功後・receipt確定前crashを再起動後にsuccessへ収束
- unknownはpendingを維持し、objectを新規作成しない
- 未開始pendingはadapterを呼ばずfailed
- 並行reconcileのreceipt確定は1件
- 補償期限、target、reference、二重補償をfail-closed
- schema 6 receiptを保持したままschema 7へ移行

## 非目標

- GitHub、決済、契約等の実サービスadapter
- unknown pendingの自動再送
- multi-use／batch capability
- human approval UI
