# ADR-0010: 継続蒸留をsnapshot固定のread-only plannerから始める

- Status: Accepted
- Date: 2026-08-20

## 文脈

authority envelopeにより正本・記録・候補は機械的に区別できるようになったが、AIが継続的な
メンテナンスを安全に実行するには、対象集合と各入力版を固定した再現可能なplanが先に必要である。
検索結果を順番に読みながら都度判断すると、途中更新で対象が変わり、同じwaveの再実行でも候補や
判断が揺れる。逆に、planを人間の承認キューへすると、廃止済みのdraft運用と維持作業を復活させる。

本文の意味を解釈するsemantic executor、複数ノートのatomic supersede、legacy authorityのbackfillは
まだ実装されていない。plannerがそれらを推測で代行してはならない。

## 決定

### 1. 同一snapshotと決定的identity

plannerは既存SQLite DBを`READ_ONLY`かつ`PRAGMA query_only=ON`で開き、単一read transaction内で
全ノートの保存済みdocumentをID順に読む。schema作成・migration・Markdown復元・pull・sync・
埋め込み追従・care/outbox更新は行わない。

各entryはdocument全体の`input_hash`を持つ。snapshotは全`(note ID, input_hash)`から
`snapshot.digest`を作り、planはschema、planner profile、snapshot、全entryから`plan_id`を作る。
時刻やprocess固有値を材料に含めないため、同じDB状態の再実行はbyte-identicalになる。

### 2. mechanical-v1の判定範囲

本文類似度や文章の意味は判定しない。authority、typed relation、description有無から機械的に
証明できるsignalだけを、次の候補operationへ写像する。

- `keep`: 機械候補なし
- `normalize`: description不足
- `revise`: active canonicalへの`updates` / `contradicts`
- `extract`: recordの未処理lineage、更新、矛盾
- `merge_candidate`: 同じnamespace/scopeにactive canonicalがあるproposal
- `supersede_candidate`: 同じnamespace/scopeにactive canonicalがあるhistorical canonical
- `unresolved`: legacy authorityや、機械判定できないproposal
- `split_canonical`: schema上は予約するが、本文意味を要するためmechanical-v1では生成しない

relationの向きは保持し、record側ではactive canonicalへの送信edgeだけを更新材料とみなし、
canonical側では受信edgeだけをpending updateとみなす。逆向きを同じsignalへ読み替えない。
UID重複、active canonical重複、参照切れ、`supersedes`不整合は候補化せずplan全体を失敗させる。

### 3. planは監査記録であり承認状態ではない

planはユーザー承認待ちqueue、draft、実行権限ではない。MCP `plan_distillation`とCLI
`kb distill plan`はread-only previewだけを公開する。候補が出ても、この段階では既存の単一note
`update`と対象固定型二段階削除の境界を自動的に越えない。

将来のsemantic executorは、実行直前にplan schema/profile、snapshot digest、全対象のinput hashを
再照合し、1件でも変わっていれば古いplanを拒否する。複数ノートの正本遷移は専用transactionで
atomicに実行し、通常updateの連続呼び出しで代替しない。

## 強制点

- `kb-core::index::open_db_read_only`: 既存schema v4だけをread-onlyで開き、空DBも作らない
- `kb-core::distillation`: snapshot、validation、mechanical-v1、hash、決定的出力
- MCP: `plan_distillation`はread-only / idempotent annotationを持ち、remote pullを行わない
- CLI: `kb distill plan --format json|markdown`
- Schema: `schemas/distillation-plan.schema.json`
- 自動テスト: 同一snapshotのbyte同一性、変更時のhash更新、無書き込み、候補写像、MCP契約

## 帰結

- AIはメンテナンス対象を再現可能な単位で列挙できる
- previewを何度実行してもKBや同期状態を変えない
- legacyや意味判断が必要な対象を、もっともらしい自動変更へ変換しない
- 自律実行は後続executorのstale-plan拒否とatomic操作が揃うまで段階的に追加する
