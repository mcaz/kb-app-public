# PoC 結果: Storage Contract backend 比較

実施日: 2026-08-16

実行:

```sh
KB_STORAGE_POC_RUNS=3 cargo run --release --manifest-path poc/storage-backends/Cargo.toml -- 1000 10000
```

環境: M1 Pro / `rustc 1.97.1 (8bab26f4f 2026-07-14)`。各値は3回の中央値。

## 結論

1. **現行 file-per-note は維持**する。93ノートの実データで直ちに困る性能ではなく、
   保存形式を移行するだけの根拠にはまだ足りない
2. **SQLite単独正本は現行のGit同期契約では棄却**する。速度と容量は優秀だが、別端末が
   別ノートを更新しただけでも同じbinary 1ファイルが衝突し、自動mergeできなかった
3. **分割event log + 派生snapshotを次の有力候補**にする。速度、fresh clone digest、
   破損検知、別端末の別ノート更新mergeをすべて通した
4. ただし**採用決定ではない**。高更新率でのlog肥大とcompaction、同一ノート競合の解決、
   crash直前のpartial write、schema evolution、実データの全意味要素を次のPoCで通すまで移行しない

## 共通fixtureと判定条件

同じ論理Note（安定ID、title、tags、body、version、provenance）を次の3方式へ保存した。

- `file-per-note`: YAML frontmatter + Markdown body、1ノート1ファイル
- `sqlite-canonical`: `knowledge.db` 1ファイル、transaction + `integrity_check`
- `segmented-event-log`: writerごとのNDJSON追記log + Git非追跡のJSON materialized snapshot

全方式で次を同じコードから検査した。

1. 初回全件書込と10%更新
2. 初回・更新Git commit
3. 1,000回のID point read
4. 全論理export
5. `git clone --no-local` 後のcold open / replay / integrity検査
6. clone前後の論理SHA-256 digest一致
7. 意図的な破損の検知
8. 同じbaseから2端末が**別ノート**を更新した場合のGit mergeと両更新の保全

検索索引・埋め込み・UIは正本backendの比較から外した。event logのpoint readとexportは
起動時に再生したmaterialized view（PoCでは`BTreeMap`）から行うため、file直読みとの数値差は
「派生view込みの候補構造」の差であり、serialization単体の比較ではない。

## 10,000ノート（1,000更新）の中央値

時間はms、容量はbytes。

| 指標 | file-per-note | SQLite正本 | 分割event log |
|---|---:|---:|---:|
| 初回書込 | 799 | 24 | 25 |
| 初回Git commit | 4,858 | 145 | 139 |
| 10%更新 | 84 | 16 | 9 |
| 更新Git commit | 727 | 146 | 149 |
| point read 1,000回 | 28 | 11 | 1 |
| 全export | 312 | 16 | 2 |
| fresh clone | 1,420 | 514 | 436 |
| clone後cold検証/再構築 | 598 | 56 | 109 |
| 正本容量 | 9,380,950 | 10,289,152 | 10,743,105 |
| 派生snapshot容量 | 0 | 0 | 9,551,951 |
| `.git` 容量（2 commits） | 6,115,795 | 3,891,399 | 3,794,589 |
| 正本ファイル数 | 10,000 | 1 | 1 segment（単一writer時） |
| clone digest一致 | PASS | PASS | PASS |
| 破損検知 | PASS | PASS | PASS |
| 別ノート同時更新merge | **PASS** | **FAIL** | **PASS** |

file-per-note比で、SQLiteは初回書込33倍、初回commit34倍、全export20倍、cold検証11倍。
分割event logは初回書込32倍、初回commit35倍、10%更新9倍、fresh clone3.3倍、cold検証5.5倍。
正本容量はSQLiteが約10%増、event logが約15%増だが、Git object容量はそれぞれ約36%・38%減った。

## 1,000ノート（100更新）の中央値

| 指標 | file-per-note | SQLite正本 | 分割event log |
|---|---:|---:|---:|
| 初回書込(ms) | 75 | 3 | 2 |
| 初回Git commit(ms) | 513 | 36 | 36 |
| 10%更新(ms) | 5 | 2 | 1 |
| 更新Git commit(ms) | 81 | 36 | 36 |
| fresh clone(ms) | 285 | 84 | 80 |
| clone後cold検証/再構築(ms) | 52 | 5 | 10 |
| 正本容量(bytes) | 937,593 | 1,044,480 | 1,073,870 |
| `.git`容量(bytes) | 619,837 | 416,966 | 405,032 |

個人KBの現行規模（実測93ノート）はこのさらに約1/10であり、現方式の性能だけを理由に
緊急移行する状況ではない。

## SQLite単独正本を棄却する理由

SQLiteはDB内transactionには強いが、Gitから見ると全ノートが1つのbinary blobである。
PoCでは同じbaseから左端末がnote A、右端末がnote Bを更新しただけでmerge conflictになった。
これはデータ内容の衝突ではなくcontainerの衝突で、現行の「GitHubを複数端末同期に使う」契約と
相性が悪い。

アプリ独自sync protocol、SQLite session changeset、server側transaction等を導入すれば解消余地は
ある。しかしその場合、clone可能なmergeable log/exportを別に正本として持つならSQLiteは
materialized viewであり、SQLite**単独**正本案ではなくなる。

## 分割event logが残った理由

単一の`events.ndjson`へ全端末がappendすると末尾でGit conflictになるため、PoCは
`events/<writer>.ndjson`へ端末/actorごとに追記する。別ノートの更新は別segmentとしてmergeされ、
clone後に全segmentをreplayして同じdigestへ戻った。

各Noteの`version`を因果ガードに使い、同じbase versionから同一ノートを二重更新した場合は、
segment順で片方を黙って勝たせず`causal conflict`としてreplayを失敗させる。検知はできるが、
ユーザーに見せる解決手順は未実装。

## 次のkill criteria

次は「分割event log + content-addressed immutable object + SQLite materialized view」を小さく試す。
以下のどれかに落ちたら採用しない。

1. 10回以上の更新roundで正本/Git容量が現行比2倍を超える前に、安全なcompactionができる
2. compaction前後、fresh clone、旧schema replayで論理digestが一致する
3. 同一ノート競合を検知し、両版を失わず、明示解決eventで収束できる
4. event末尾partial writeとsegment途中破損を区別し、最後の健全snapshotまで回復できる
5. Note、sources、relations、Artifact ref、統治情報、監査履歴をlosslessに表現できる
6. raw eventを人が直接読むことを強制せず、同梱CLIでinspect/diff/exportできる
7. 現行93ノートの実データを往復してStorage Contract v1 digestが一致する

## 再現コード

- `src/main.rs`: fixture生成、3 backend、Git clone/merge、digest、破損検知、中央値集計
- `Cargo.toml` / `Cargo.lock`: 固定依存

PoCは本番storageを変更しない。結果が次段を通るまで、現行Markdown + Gitを基準実装として維持する。
