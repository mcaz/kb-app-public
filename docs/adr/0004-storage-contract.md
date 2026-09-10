# ADR-0004: 正本を保存形式ではなく Storage Contract で定義する

- Status: Accepted
- Date: 2026-08-16

## 文脈

起草時は「プレーンな Markdown + Git」を正本とした。これは内容を直接読め、GitHub から
clone でき、特定アプリへロックインされにくいという優れた初期値だった。一方で Markdown、
SQLite、ベクトルDB、Wikilink のどれを採るかは手段であり、それ自体は利用者価値ではない。

Claude、GPT、Kimi、DeepSeek 等の基盤モデルは、学習済み重みの内部表現、コンテキスト、
推論時のキャッシュ、製品側の検索を組み合わせる。しかしその内部表現は、個人KBの全内容を
利用者が監査し、repository として複製し、同じ状態を再構築するための公開 storage protocol
ではない。モデル内部を模倣することより、KB側の不変条件を先に定義し、その条件の範囲で
検索・保存の最良実装を交換できるようにする。

## 決定

正本を特定のファイル形式ではなく、次の **Storage Contract** で定義する。

1. **Inspectable** — 利用者が、専用DBツールなしでも公式 export を読んで全論理内容を確認できる
2. **Clone-reproducible** — fresh clone から同じ論理状態を復元し、索引を作り直せる
3. **Deterministic** — 同じ論理状態は順序や派生キャッシュに左右されず同じ SHA-256 digest になる
4. **Lossless migration** — backend 交換の前後で論理 export を比較でき、差分を説明できる
5. **Derived means disposable** — 索引・埋め込み・キャッシュを消しても正本の情報を失わない
6. **Boundary-aware** — Git で運ぶ情報と `local_only` を混同せず、再現範囲を明示する

現行の Markdown + OKF + Git は、この契約を実装する最初の保存 adapter として継続する。
形式移行はこのADRだけでは実施しない。

## P0 の強制点

- `kb-core::storage_contract` が repository の論理 snapshot を厳密に生成する。
  2026-09-08: 語彙正本の指定を持つ保管庫はv2としてworkspace_id・note_uid・revisionを含める。
  未指定の保管庫はv1の出力とdigestを維持する。指定はDBと専用outboxへatomicに保存し、
  Git追跡の`.kb-tag-vocabulary.json`からノートと同じtransactionで復元する。
- `kb storage verify` が読み取り専用で検査し、digest と件数を JSON で返す
- `kb storage export [--output PATH]` が比較用の JSON を返す
- snapshot はノート(frontmatter + body)、監査ログ、Git 管理される Artifact manifest / ref /
  alias、旧添付の path / size / SHA-256 を含む
- `index.md`、埋め込み、端末ローカルの availability は含めない。`.kb/index.db`自体も
  export対象外だが、現在は派生索引だけでなく再生成できないローカル台帳も持つため、
  DB全体を削除可能なcacheとは扱わない
- 壊れた tracked manifest / ref、参照切れ、workspace ID の欠落・競合は黙って捨てず失敗する
- 新規 vault は最初の commit から `.kb-workspace` を持つ。検査は ID を新規発行・修復しない
- fresh clone の digest 一致と、clone 側での索引再構築・検索を自動テストする

## ファイル実体の再現範囲

- `full`: manifest と LFS pointer は repository、bytes は同じ origin の Git LFS から取得する
- `local_only`: 意図的に repository へ入れないため clone の対象外
- 旧 `<note-id>.files/`: repository 内の bytes を clone し、snapshot は hash で同一性を確認する

P0 export は論理比較用であり、それ単独で bytes や Git 履歴を復元する backup bundle ではない。
`local_only` を含む完全 bundle、履歴の一括搬出、LFS 到達性のオフライン検査は次段の契約とする。

## 次の候補を選ぶ判定表

| 候補 | 期待する利点 | 採用前に証明すること |
|---|---|---|
| 現行 Markdown + Git | 直接可読・差分・既存資産 | ノート数増加時の書込/走査性能と競合体験 |
| SQLite を正本化 | transaction・問い合わせ・単一ファイル | inspect/export、Git差分の代替、破損復旧、clone後の同一digest |
| append-only event log + snapshot | 監査・同期・再生 | event schema進化、compact、部分破損、再生時間、利用者向け閲覧 |
| CRDT / log-structured store | 多端末同時編集 | 複雑性と容量に見合う実競合、決定的export、履歴説明可能性 |

候補は同じ受入 fixture に対し、正しさ、clone/rebuild時間、検索・更新速度、容量、実装複雑性を
測る。現行 adapter を上回らない限り移行しない。

## P1 backend 比較（2026-08-16）

[storage-backends PoC](../../poc/storage-backends/RESULTS.md) で、file-per-note、SQLite単独正本、
writer別の分割event log + 派生snapshotを1,000／10,000ノート・各3回で比較した。

- 3方式ともfresh cloneの論理digest一致と破損検知に合格
- SQLiteと分割event logは10,000ノートの初回書込・commit・exportでfile-per-noteより桁違いに高速
- SQLite単独正本は、2端末が別ノートを更新しただけでbinary DBがGit merge conflictになり棄却
- 分割event logは別ノート更新を別segmentでmergeし、両版を保持して同じdigestへ収束した

したがって現行形式はまだ移行せず、次段は「分割event log + content-addressed immutable object +
SQLite materialized view」のcompaction、同一ノート競合、partial write、schema evolution、実データ
lossless round-tripを検証する。詳細数値とkill criteriaはPoCレポートを正本とする。

## 帰結

- Markdown を守ることと、知識を守ることを分離できる
- 検索方式や保存形式を、モデルや流行ではなく同じ受入条件で比較できる
- JSON export は非常口かつ migration oracle になる
- P0 時点では保存速度は変わらない。性能改善は計測後の backend 選定で行う
- `local_only` を含む「cloneだけで完全復元」はできないため、完全 bundle が未完であることを
  UI/CLIで隠さない必要がある

## P2 ローカル実行面をSQLiteへ移す（2026-08-18）

本人決定により、SQLite単独正本をGitへ入れる案は引き続き棄却したまま、日常の実行面だけを
DBへ移した。AI・GUI・CLIの通常読取とpropose／update／二段階削除はSQLite transactionを境界とし、
同じtransactionでdurable outboxを積んでMarkdownへ出力する。MarkdownはObsidian表示、Git
バックアップ、fresh clone復元、lossless検証を担う。

通常syncはMarkdownの外部編集を暗黙にDBへ取り込まない。明示importと、cleanなworktreeへの
Git pull後だけがMarkdownからDBへ入る経路である。未commitのノートMarkdownが外部編集されて
いる場合、同期は上書きせず競合として止める。

自動retrievalは検索後に最大3回Markdownを読む方式を廃止し、`search(include_documents)`の
1 callで上位5 seed、DB有向リンク最大2ホップ、最大50候補を組み立て、推定10,000 token以内・
最大10本文を同じSQLite snapshotから返す。索引同期と埋め込み追い付きも検索時の1回だけで、
候補本文ごとには繰り返さない。推定4,000 token超の長文はfrontmatterを残したままMarkdown見出し単位、
見出しが巨大または無い場合は2,400 byte以下へ分割し、query語coverage順の最大3 passage・推定3,600
tokenへ縮約する。12,000 tokenはCodex hookのspill安全上限であり、目標投入量ではない。

この段階は複数端末のmerge可能な交換表現をMarkdownのまま維持する。writer別分割event log＋
content-addressed immutable objectへの移行は、P1のkill criteriaを通した後の別判断とする。

### 語彙一括変更のローカル履歴（2026-09-08）

schema v12の`tag_vocabulary_runs`と`tag_vocabulary_run_notes`は、語彙変更の実行ID、
workspace、指定した正本UID/revision、計画hash、操作、理由、client、時刻、件数、および
変更した全ノートのUID（legacyは未設定）、変更前後の原文/hash/タグを保持する。
ノートと既存outboxと同じtransactionで確定する端末ローカルのdurable台帳とする。
保存後のMarkdown export失敗で台帳を取り消さず、ノートの出力待ちとして再試行する。

原文はDBにだけ保存し、履歴APIは要約とノートのmetadataだけを既定20件・最大100件で
ページ化して返す。現在非参照となったノートのmetadataはUIDと元ノートIDで除外する。
通常のimport・派生索引修復では台帳を消さない。schemaが要求する表の欠損や制約破損は
空表の再生成で隠さない。限定復旧では現存する台帳の原文/hash/件数を検査して保持し、
導入前の旧schemaに両表が存在しない場合だけ、検証後の復旧transactionで空表を追加する。

この台帳はrepositoryのlogical snapshotへ加えないため、v1/v2の再現範囲は変わらない。
Gitには通常のノート原文と実行ID・理由を残すが、fresh cloneは過去のローカル台帳を
再生成しない。第4段階の復元は同じworkspaceに残るローカル台帳を対象とする。
schema13の`tag_vocabulary_rollbacks`は元実行へ外部キーと一意制約を持ち、復元ID・元実行ID・
計画hash・理由・client・時刻・件数を保持する。元実行と前後原文の台帳は書き替えない。
復元された全ノートとoutbox・復元台帳を同じtransactionへ確定し、出力失敗でも保存済みとして保持する。
schema12からは旧2台帳を維持して復元表を追加する。通常openでは固定DDLを検査し、診断・限定復旧では
元実行への参照と復元件数を照合する。新schemaの欠損を空表へ再生成せず、復旧では全行hashで保持を確認する。
別端末・fresh cloneから自動復元できる可搬manifestや履歴の一括搬出は、引き続き後続段とする。

## P3 authorityを論理snapshotの不変条件へ加える（2026-08-20）

[ADR-0009](0009-canonical-authority.md)に従い、ノートの`note_uid`、authority envelope、typed relationを
Storage Contractの論理内容へ加えた。legacyノートは`note_uid`とauthorityの両方が無い状態だけを
互換読み取りとして許す。envelope付きノートについては、UID重複、同じnamespace+scopeのactive
canonical重複、relation参照切れ、自己参照・重複edge、不正なsupersedes、後継の無いsupersededを
snapshot生成時に失敗させる。

SQLiteはschema v4で同じ項目とrelationを索引化するが、正本は引き続き決定的snapshotとfresh clone
再現性で定義する。したがってDBのunique indexだけを保証点にせず、Markdownから復元した状態にも
同じStorage Contract検査を適用する。
