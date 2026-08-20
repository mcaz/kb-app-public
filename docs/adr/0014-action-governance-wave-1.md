# ADR-0014: Action Governance wave 1 — 外部副作用を共通判定モデルへ通す

- 状態: 評価中
- 日付: 2026-08-21
- 関連: Issue #72、ADR-0009（AI自律削除）

## 背景

AIクライアントが行う作成・更新・削除・課金・契約・公開・配備を、GitHub等の
サービスごとの個別承認規約だけで制御すると、対象固定、費用上限、期限、単回性、
実行証跡の意味が分岐する。文章上の「確認不要」も、対象や期間を限定できなければ
強すぎる権限になる。

現行のAI自律削除は、ノートIDと内容fingerprintへ結びついた短命・単回tokenを使い、
対象差し替え、期限切れ、replayを拒否している。これはサービス非依存の副作用統制へ
一般化できる先行実装である。

## 決定

wave 1では `kb-core::action_governance` に次を置く。

1. `ActionRequest`: actor、client surface、操作、完全修飾対象、危険度、可逆性、
   費用上限、冪等キー、期限、理由を表す。
2. `ActionPolicy`: 許可操作、対象prefix、可逆writeの自動許可、費用上限を表す。
3. `CapabilityLease`: request hash・操作・対象へ固定した、期限付き・使用回数付きの
   個別権限を表す。
4. `PolicyDecision`: `allow` / `deny` / `require_human` と機械可読reasonを返す。
5. `ExecutionReceipt`: 実行済みrequestと冪等キーを記録し、replayを拒否する。
6. evaluatorはI/Oを持たない純粋関数とし、denyを既定にする。

read-onlyとpolicyで明示した可逆writeだけをstanding policyで許可する。破壊、課金、
契約、公開・security-sensitiveな操作は、requestへ一致するcapabilityがなければ
`require_human` とする。不正・期限切れ・scope不一致・replayは `deny` とする。

機械契約は `schemas/action-governance.schema.json` を正本とし、Rust型とexampleを
テストで結ぶ。request hashは、object keyを辞書順にしたcompact JSON（UTF-8、末尾改行
なし）のSHA-256として固定する。現行削除planは `ActionRequest::for_note_removal` で
写像を固定する。

## このwaveで行わないこと

- GitHub、決済、契約、配備など外部サービスのadapter実装
- capabilityの署名・発行UI・永続store
- 現行MCP削除経路の置換
- KBノートを実行権限の正本にすること

KBは判断根拠・履歴を保持できるが、強制判定は実行経路上のコードと永続receiptが担う。
外部adapterを接続するwaveで、adapterが必ずevaluatorを通ることを機構として固定する。

## 帰結

- サービス固有adapterの前に置ける、共通のfail-closed判定語彙が得られる。
- target、期限、費用、replayに対するテストをconnector非依存で再利用できる。
- 現段階では内部の評価用APIであり、既存MCP contractと動作は変更しない。
- capability署名とreceipt永続化がないため、wave 1だけでは外部副作用を強制統制しない。

## 次の判定条件

実在する1つのadapterを接続し、(1) evaluator迂回ができない、(2) receiptが再起動後も
replayを止める、(3) capabilityの発行者と署名を検証できる、の3点を満たした時点で
「評価中」から採用へ進める。
