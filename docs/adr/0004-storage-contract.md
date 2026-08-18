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

- `kb-core::storage_contract` が repository の論理 snapshot v1 を厳密に生成する
- `kb storage verify` が読み取り専用で検査し、digest と件数を JSON で返す
- `kb storage export [--output PATH]` が比較用の JSON を返す
- snapshot はノート(frontmatter + body)、監査ログ、Git 管理される Artifact manifest / ref /
  alias、旧添付の path / size / SHA-256 を含む
- `index.md`、`.kb/index.db`、埋め込み、端末ローカルの availability は派生なので含めない
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
DBへ移した。AI・GUI・CLIの通常読取とpropose／update／removeはSQLite transactionを境界とし、
同じtransactionでdurable outboxを積んでMarkdownへ出力する。MarkdownはObsidian表示、Git
バックアップ、fresh clone復元、lossless検証を担う。

通常syncはMarkdownの外部編集を暗黙にDBへ取り込まない。明示importと、cleanなworktreeへの
Git pull後だけがMarkdownからDBへ入る経路である。未commitのノートMarkdownが外部編集されて
いる場合、同期は上書きせず競合として止める。

自動retrievalは検索後に最大3回Markdownを読む方式を廃止し、`search(include_documents)`の
1 callで検索結果と上位本文を同じSQLite接続から返す。索引同期と埋め込み追い付きも検索時の
1回だけで、候補本文ごとには繰り返さない。

この段階は複数端末のmerge可能な交換表現をMarkdownのまま維持する。writer別分割event log＋
content-addressed immutable objectへの移行は、P1のkill criteriaを通した後の別判断とする。
