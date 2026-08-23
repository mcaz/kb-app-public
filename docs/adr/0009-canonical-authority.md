# ADR-0009: 正本判定を安定UIDとauthority envelopeへ移す

- Status: Accepted
- Date: 2026-08-20

## 文脈

継続蒸留をAIへ任せるには、「いま参照すべき正本」「観測・会話の記録」「まだ正本ではない候補」
を機械的に区別できなければならない。従来のpath、タグ、タイトル、本文類似度だけでは、同じ主題の
複数ノートがどれも現行に見え、AIごとに統合先や削除判断が分散する。pathをIDとしてrelationを張ると、
renameや将来のnamespace移動で来歴も切れる。

一方、過去に廃止したdraft・confirm・承認キューを復活させると、ユーザーへ維持作業を戻してしまう。
必要なのは人間承認状態ではなく、AIが自律メンテナンスで使う正本authorityである。

## 決定

### 1. 安定identity

新規ノートにはcoreが26文字ULIDの`note_uid`を発行する。`note_uid`はpathと独立し、作成後の変更・
削除を拒否する。既存ノートは明示的な移行waveまで`note_uid`とauthorityの両方が無いlegacyとして
読み取れるが、片方だけの状態は許さない。

### 2. 共通6namespace

ノートの主たる役割を次の6つへ固定する。

| namespace | 内容 |
|---|---|
| `entities` | 人・組織・製品・場所など継続的な対象 |
| `initiatives` | 目標・プロジェクト・改善計画など進行する仕事 |
| `decisions` | 採用した方針・判断・制約 |
| `procedures` | 再利用する手順・運用・チェックリスト |
| `records` | 会話・調査・観測・実行結果など時点付き記録 |
| `knowledge` | 上記以外の再利用可能な説明・知見 |

物理pathはこのADRでは移動しない。namespaceは機械判定層であり、将来のmoveはaliasとtransactionを
備えた別操作にする。

### 3. authority envelope

`authority`は以下を持つ。

- `namespace`: 共通6namespace
- `role`: `canonical` / `record` / `proposal`
- `status`: `active` / `historical` / `superseded`
- `scope`: 同じ主題・適用範囲を表す安定key

`records` namespaceと`record` roleは常に対にする。`proposal`は`active`だけを許す。
同じ`namespace + scope`の`active canonical`はSQLiteの部分unique indexとStorage Contractの
双方で1件に固定する。

`proposal`は下書き、レビュー待ち、ユーザー承認待ちではない。AIが既存canonicalを壊さずに候補を
識別するための内部分類であり、専用の承認UI・状態filter・確定ボタンは作らない。

### 4. typed relation

relationの端点はpathでなく`note_uid`にする。v1では次を許す。

- `derived_from`: 根拠から導出した
- `supports`: 根拠・観測が支持する
- `updates`: 追加情報・更新である
- `contradicts`: 内容が矛盾する
- `supersedes`: 現行canonicalが旧canonicalを置き換える
- `mentions`: 弱い言及関係

自己参照、同じedgeの重複、存在しないtargetを拒否する。`supersedes`は同じnamespace/scopeの
active canonicalからsuperseded canonicalへだけ結ぶ。後継edgeの無いsuperseded canonicalも
Storage Contract違反にする。

typed relationで参照されているノートは、参照元を更新してedgeを外すまで削除できない。これにより、
AIの自律削除が根拠・矛盾・後継の鎖を黙って切らない。

### 5. retrieval

完全タイトル一致をlocatorとして最優先にする。queryに現行・履歴・記録・理由を示す明示語がある
場合はauthorityのstatus・role・namespaceとの整合を加点し、それ以外の候補間ではactive canonicalを
legacy、record、historical、proposal、supersededより優先する。typed relationは既存Markdown linkと
同じ有向グラフとして自動retrieval・関連ノート・グラフ表示へ加える。標準Markdown linkのanchor
textはリンク先へ紐づく派生FTS索引にし、本文OR検索より弱い信号として全query term一致時だけリンク先を
昇格する。retrievalのseedから有向グラフを展開するときは、意味を明示する`derived_from`・`supports`・
`updates`・`contradicts`・`supersedes`を通常Markdown linkより先にし、弱い`mentions`を最後にする。
同じtier内はノートID順に固定し、結果を決定的にする。検索候補はauthority順位を確定した後、
同じscope・role・status、または同じrole・status内で同一正規化本文か先頭2,048正規化文字の文字trigram
Jaccardが85%以上のclusterごとに最上位1件だけを残す。本文が同じでもrole・statusが異なるfacetは保持する。
これは検索結果の多様化だけで、類似度によるノートの自動mergeは行わない。

## 互換性と段階移行

- legacyノートは従来どおり検索・取得できる
- 新規MCP `propose`はauthorityを必須にする
- `update`でlegacyノートへ初めてauthorityを付ける際にcoreが`note_uid`を発行する
- 物理namespace移動、alias、複数ノートを一括遷移するatomic supersede、蒸留policy executor、
  既存ノートの一括backfillは別段とする
- atomic supersedeが無い段階では半端なsuperseded状態を通常の単一note更新で作らない

## 強制点

- `kb-core::authority`: 型、scope、envelope、relation検証
- `kb-core::index`: schema v4、UIDとactive canonicalのunique index、relation index
- `kb-core::note_store`: UID不変、参照先実在、supersedes整合、参照中削除の拒否
- `kb-core::storage_contract`: export全体で重複・参照切れ・後継不整合を検査
- MCP schema: proposeのauthority必須、get/search/recentのauthority返却
- 自動テスト: legacy互換、frontmatter round-trip、v3→v4移行、検索順位、relation retrieval、
  削除境界、Storage Contract違反と正しいsupersession

## 帰結

- AIは統合先と削除可否をpath・タグ・本文推測だけで決めなくてよくなる
- 記録を残したまま、利用時には現行canonicalを優先できる
- renameや将来のbackend交換でもlineageを維持できる
- 新規proposeの呼び出し側はauthorityを必ず選ぶ必要がある
- 既存ノートはbackfill完了までlegacyとauthority付きが混在する
- canonical置換は複数ノートの原子的遷移が必要なので、単一updateの寄せ集めでは実行しない
