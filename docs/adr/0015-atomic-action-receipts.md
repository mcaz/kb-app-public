# ADR-0015: 単回capabilityの消費とpending receipt確保をatomicにする

- 状態: 評価中
- 日付: 2026-08-21
- 関連: Issue #74、ADR-0014

## 背景

ADR-0014のpure evaluatorは、request・policy・capability・既知receiptから決定的な
decisionを返せる。しかし、外部adapterが「allow判定」「capability消費」「receipt保存」を
別々に行うと、同じ単回capabilityを2実行が同時にallowされる競合窓が残る。外部実行後、
receipt確定前にprocessが停止した場合も、単なる未記録とみなして再実行すると二重作用になる。

また、issuerを検証しないcapabilityは、対象固定でも任意のclientが自作できる。KBの散文を
issuerや消費状態の正本にしても実行経路上の強制にはならない。

## 決定

### 1. local capabilityをissuer署名へ固定する

`kb.action-signed-capability.v1`はworkspace、actor、client surface、issuer、発行時刻、nonce、
leaseをHMAC-SHA-256の対象にする。leaseはrequest hash、action、target、expiry、cost上限、
使用回数を含む。reserve時に署名scopeを現在のworkspaceとrequestへ再照合するため、別workspace、
別actor、別surfaceへcapabilityを流用できない。secretはcapabilityへserializeせず、runtimeが
明示的に受け取った32 byte以上のtrusted issuer keyだけで検証する。

v1は端末ローカルの対称鍵を想定する。verifyだけを行うremote adapterや複数issuerへ広げる前に、
public-key署名とKeychain等の鍵保管を別waveで決める。

### 2. exact-request capabilityは単回だけにする

ActionRequestのhashにはidempotency keyも含まれるため、同一hashを複数回許可するとreplay拒否と
矛盾する。署名capabilityの`remaining_uses`はv1で1に固定する。複数回またはbatch権限は、
request hash固定ではなく別のscope contractとして設計する。

### 3. reserveを外部副作用の唯一の前段にする

`action_governance_store::reserve`はSQLiteの`BEGIN IMMEDIATE`内で次を行う。

1. request hashとidempotency keyの既存receiptを検査する。
2. pure evaluatorのdecisionがallowであることを確認する。
3. capability IDが未消費であることを確認し、消費行を作る。
4. requestとdecisionを含む`pending` receiptを作る。
5. すべてを1 transactionでcommitする。

外部adapterはcommit済みreservationを受け取った後だけ副作用を実行する。standing policyで許可した
read／可逆writeもreceiptを作り、idempotencyを同じ経路で強制する。

低水準のreserve／実行開始／finalizeはmodule内に閉じ、実在adapterは`GovernedActionAdapter`を実装する。
`ActionGovernanceGate`だけが生成できる`ExecutionPermit`を受け取って実行するため、adapter境界から
transactional gateを迂回した呼出しを公開しない。

### 4. 外部結果はpendingへ一度だけ確定する

外部実行成功はexternal reference必須で`succeeded`、失敗は`failed`へ更新する。確定済みreceiptの
再確定と、failed後のcapability復活は認めない。process再起動後に残った`pending`は二重実行せず、
外部systemのidempotency keyやobject照会によるreconcile対象として列挙する。

adapter呼出し直前に`execution_started_at`をatomicに記録する。値がないpendingは外部実行前crash、
値があるpendingは外部実行開始後・確定前crashとして区別する。後者は作用が未発生とは仮定せず、
必ず外部照会へ送る。

### 5. 強制状態はruntime SQLiteに置く

`action_receipts`と`action_capability_uses`をruntime index DB schema 6へ追加する。これはKB noteや
remoteへ同期する知識正本ではなく、端末上の実行制御状態である。既存schema 3〜5からはnote本文、
埋め込み、distillation履歴を作り直さず非破壊で追加する。

## このwaveで行わないこと

- GitHub、決済、契約、配備adapterの接続
- HMAC keyの生成UI・Keychain保管・rotation
- remote verifier向けpublic-key署名
- pending receiptの外部system別自動reconcile
- exact requestを越えるmulti-use／batch capability

## 帰結

- 並行実行でも同じ単回capabilityのreservationは1件だけになる。
- crashと再起動をまたいでpending・成功・失敗・消費済み状態を保持する。
- requestやleaseの改変、不明issuer、期限切れ、複数useをfail-closedにできる。
- adapter用permitをgateだけが生成するmodule境界を持つ。既存adapterはまだないため、製品全体の
  実効性は最初の実在adapter接続時に検証する。

## 次の判定条件

最初の実在adapterを低riskなreversible writeへ接続し、reserve以外から低水準toolを呼べないmodule境界、
外部idempotencyとの対応、pendingの照会・確定を受入testで固定できたら、ADR-0014と本ADRを採用へ進める。
