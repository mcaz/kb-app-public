# ADR-0003: ファイル(Artifact)の保存構造と転送 — 実体は Vault Git の外、full 転送は同一 origin の Git LFS

- 状態: **採用**(2026-08-12 本人決定)
- 関連: [ADR-0001](0001-core-language.md) / [ADR-0002](0002-frontend-stack.md)(層の境界・i18n・型生成)/
  [requirements.md](../requirements.md) FR-C8(この ADR で改定)/ [contract.md](../contract.md) 契約5
- 正本ノート: KB `notes/kb-app-note-artifact-二層モデル-v1`(決定)と
  `notes/kb-app-ファイル機能の画面設計と転送方式-2026-08-12-判断-二層モデルの実装設計`(実装設計)

## 文脈

FR-C8 の添付は、ノートと同名のサイドカー `<id>.files/` に実体を置き、Vault Git に
そのままコミットする方式で実装済み(`crates/kb-core/src/vault.rs`)。ノート=1ファイルの
OKF 互換を壊さない利点があり、そこは変えない。変えるのは次の3点で、いずれも
現行実装に**概念そのものが無い**:

- **置き場所**: binary が Vault Git に入るため、履歴・clone・同期が単調に肥大化する。
  50MB 上限はその緩和策であって設計ではない
- **同一性**: 識別はファイル名だけ。同名は `-2` で回避しており、内容の同一性・
  来歴・版の区別が無い
- **持ち出し範囲**: 概念が無く、添付は必ず GitHub まで同期される。仕事のリポジトリから
  取り込んだファイルを「この端末だけ」に留める手段が存在しない

加えて、実装設計の過程で**移行そのものが最大のリスク**であることが判明した。
現行の添付は Vault Git の中にあるから全端末へ同期されている。決定どおり実体を
Git の外へ移すと、blob の転送機構が無い状態では、移行した端末以外で既存の添付が
一斉に取得不能になる。これは正本の受入条件13項目のどれでも防げていなかった。

## 決定

### 1. `sync_policy: full` と、後段送りの `external / full sync` は別概念

正本の「保存構造」節で後段送りにされているのは**取り込み・転送方式**としての
`external / full sync` であって、転送軸の値 `full` そのものではない。
`sync_policy: local_only | manifest_only | full` は MVP のモデルに含まれる。

### 2. 正式な full 転送は同一 Vault origin の Git LFS。実行環境は同梱する

> **2026-08-13 追記(実装で確定した3点)。**
> (a) `full` の実体は **LFS の置き場が持つ**。自前 CAS([`store`])が持つのは
> 「情報のみ」「同期しない」の2境界だけ — 両方に置くと二重保存になる。
> (b) 作業ツリーには実体を残さない。取り込みの最後に pointer へ戻す
> (実体は `git add` の時点で外の置き場に入っているので、消えるのは複製だけ)。
> (c) **LFS を通す書き込みは git CLI で行う。** `Vault::commit` は libgit2 で、
> libgit2 は LFS のフィルタを走らせないため、CLI が staged した pointer を
> 実体で上書きしてしまう(実測で踏んだ)。

blob を運ぶ機構を MVP に含める(**本人判断**: 複数端末を日常的に使うため、
「昨日の添付は見えるのに今日のは見えない」期間を作れない)。

- 接続先も認証も既存の GitHub 経路のまま。ユーザーに増える手順を作らない
- `git-lfs` はアプリに同梱する。非エンジニア配布のため、別途インストールを求めない
- LFS の local storage は既定の `.git/lfs`(Vault 配下)を使わず、`lfs.storage` で
  **repo 外 sidecar** へ移す
- Vault Git に入るのは数百バイトの pointer だけ。**raw binary は置かない**
- `content_hash` = raw bytes の SHA-256。LFS の OID と一致するので検証が二重にならない

### 3. manifest の取得と blob の取得を分離する

通常の pull では smudge を無効にし、必要な object だけを後から明示 fetch する。
blob の取得失敗は**端末ごとの `missing`** として可視化する — ノートの同期は止めない。

### 4. 既存 `<id>.files/` は読み取り専用の legacy transport。MVP では削除しない

移行は blob を動かさない二段階で行う:

1. 既存添付に `private + full + legacy` の manifest / ref を重ね、旧置き場を
   読み取り専用にする(新規書き込みを止める)
2. 小さな sentinel で LFS の往復と照合を検証してから既存 blob を stream import し、
   **upload 成功を確認してから** primary locator を切り替える。旧パスは fallback として残す

`sensitivity` を `shared` と推定しない(Git に入っていた事実から共有可能性は導けない)。
元が client repo だったかも推定不能なので `client_repo: false` と断定しない。
Git 履歴は書き換えない。`git lfs migrate` は使わない。

受入条件を1つ追加する: **移行前に全端末から取得できた既存添付は、移行によって
取得可能性を低下させない。**

### 5. 参照は ref で追い、出典は id で固定する

- 通常の添付と本文リンクは workspace scope の `artifact_ref` で最新版に追従する。
  本文リンクの形は `kb-artifact-ref:<名前>`
- **版を重ねたら参照を付け替える**(2026-08-13 実装で確定)。追従は自動では起きない —
  新しい版は別の `artifact_id` なので、`ref` を向け直さない限り本文リンクは古い版を
  指したままになる。取り込みが差し替え元を受け取ったとき、その版を指していた参照を
  `ledger.ref_for` で引いて `point_to` する。**名前は変えない**(参照名を変えると
  本文リンクが切れるので、それは詳細画面の明示操作)
- 出典(`sources[].resource`)は `kb-artifact:<artifact_id>` で**版を固定**する。
  証拠としての出典が後から別内容に変わるのを防ぐため。OKF のフィールドは
  そのままで、独自フィールドを追加しない
- 本文中の既存 `/…files/…` リンクは**自動書換えしない**。移行時に
  「旧相対パス → artifact_ref」の alias を作り、Markdown レンダラが vault root へ
  文字列連結して直接開く現在の処理をやめて、両形式をコアの resolver へ渡す。
  frontmatter はアプリの所有物なので `sources[].resource` は移行時に更新する

### 6. 既定値と、経路の一本化

- 通常の新規ファイル: `private + full`
- client repo 由来: `linked + local_only`。repo identity を確定できないときは linked を
  作らず managed を案内する。linked の対象は **repo ID + repo-relative path のみ**
  (安定 URI は後段。取得・認証・availability 判定が対象ごとに異なるため)
- **すべての取り込み経路を同じ kb-core API へ合流させる**。現行の貼り付け・
  ドラッグ&ドロップは `app/src/hooks/useNoteFileIntake.ts` が `api.attachmentAdd` を
  直接叩いており、ダイアログを経由しない。ハードゲートを UI 側に置く設計は
  この時点で穴が空く。拒否と locator 検証はコアで行い、UI の無効化は補助とする

### 7. 内容は不変。更新は新しい版。「削除」は関係の解除

content と作成時の provenance は上書きできない。内容変更は新しい `artifact_id` +
`supersedes`。`artifact_ref` の更新は `expected_version` 必須で、**自動再試行も
強制上書きもしない**。UI の「削除」は「このノートから外す」であり、blob は消えない
(MVP に GC が無いため、消えると言ってはいけない)。

> **2026-08-13 追記。** 前の版が残る以上、**一覧は最新版だけを返す**必要がある
> (`Ledger::list_for_note`)。素直に一覧すると同じファイルが版の数だけ並ぶ。
> 履歴は `supersedes` を辿って読む。

### 8. 取り込みは path ベースの streaming。サイズ上限は境界ごとに変える

- フロント → Tauri は **path・保存方法・policy・role・ref 名だけ**を渡す。
  `FileReader.readAsDataURL()` と base64 IPC は production から廃止する
  (元データ・base64 文字列・decode 後 buffer が同時に載るため)
- kb-core が固定サイズ chunk で `read → hash → temp write` し、fsync 後に atomic rename。
  進捗は Tauri イベントで通知する
- `local_only` / `manifest_only` に固定上限を置かない。`full` のみ 100MB 警告・
  2GB 拒否(接続先の単一ファイル上限が根拠)。**現行の 10MB / 50MB は
  GitHub 同期の保全が根拠であり、実体が Vault Git を出た時点で失効する**
- quota 不足はサイズ内でも起きるので、サイズ超過とは別の typed error として分類する
- クリップボード画像だけは streaming 不能。大きすぎる場合は拒否して
  「ファイルとして保存して取り込む」を案内する。これは Artifact のサイズ制限ではなく
  clipboard 経路の OOM 防止なので、文言でも分ける

### 9. availability は同期せず、端末ごとに導出する

**正本の訂正**: manifest の「`expected_version` で更新できる field」に `availability` を
含める設計は、多端末環境で成立しない(同じ Artifact がこの端末では local、
別端末では missing なのに、同期される field に書けば上書き合戦になる)。

- manifest が持つのは desired な `sync_policy`・locator・`content_hash`
- fetch 結果・last verified・直近エラーは**ローカル sidecar**
- `local | missing | unavailable_by_policy` は API・検索・`get`・Context Pack が**都度計算**する
- ホームの件数集計もこの端末で計算する。`expected_version` は availability に適用しない

> **2026-08-13 追記(算出の入力を訂正)。** 実装は `sync_policy` だけで実体の持ち主を
> 決めていたが、**持ち主を決めるのは locator** で、区分は「どこまで運びたいか」という
> 別の軸だった。そのため保管庫の中に実物がある旧添付(`LegacyGit`)まで missing になり、
> **移行を入れた瞬間に「移行したら開けなくなる」形**になっていた。
> 算出は locator で分岐する: `Managed` は区分ごとの置き場、`LegacyGit` は保管庫の中の
> 実ファイル、`Linked` は**確かめられない**。
>
> `Linked` を Local と言わない側に倒したのは、リポジトリ ID から手元のパスを引く仕組みが
> 無いため(下の残課題)。これは「元の場所にあるのに『この端末にありません』と出る」
> という別の不正確さを残すが、**在るとも無いとも言えないものを在ると言うより害が小さい**。

### 10. 緩和する能力は対話的な UI にだけ与える

`sensitivity` / `sync_policy` の厳格化は常に可能、緩和は確認を1段挟む。
ただし「確認1段」だけでは caller が確認済みフラグを立てられるため、
**緩和の能力自体を MCP に公開しない**(強制の階段の最上段=能力の不在)。
client repo は確認画面へ入る前にコアが拒否する。確認文には
「後で範囲を狭めても、同期先や Git 履歴から自動では消えない」ことを含める。

> **2026-08-13 追記(実装で見つかった裏口)。** 「新しい版として追加」は新規の
> 取り込みと**同じ経路**なので、素直に実装すると呼び出し側の `policy` がそのまま効き、
> 確認を一度も通さずに `local_only` を `full` にできてしまう。したがって
> **版を重ねるときは前の版の policy を引き継ぎ、渡された指定を見ない**
> (client repo による厳格化だけは、この上からさらに効く)。
>
> 一般化すると、**制限を「その操作」に付けると同じ結果へ到達する別の操作から漏れる**。
> 禁止は操作単位ではなく到達点(= 範囲が広がった状態)で考える。

## 影響

- FR-C8 は「`<id>.files/` が恒久方式」から「legacy transport(移行対象)」へ改定される
- `crates/kb-core/src/vault.rs` の `add_attachment` / `remove_attachment` /
  `list_attachments` は Artifact API へ置き換わる。`remove_attachment` が実ファイルを
  消す現在の挙動は、不変性を壊すため Artifact には流用できない
  → **2026-08-13 実施**。`add_attachment` / `remove_attachment` と 10MB / 50MB の上限を
  削除し、旧経路への**書き込みを閉じた**(決定4 の「新規書き込みを止める」の実体)。
  `list_attachments` だけ残す — 移行までノート内のファイル欄と索引が読む
- `app/src/components/molecules/AttachmentBar` は `FilePanel`(organism)へ置き換え。
  `ScopeChip` のような**ドメイン型を知る atom は作らない**(ADR-0002 決定5)。
  汎用 `StatusPill` を再利用し、対応付けは molecule 以上で行う
- i18n は `files` 名前空間を作らず `notes` + `common` に置く(locales が画面ごとの
  名前空間で構成されているため)。独立したファイル画面を作る時に初めて検討する
- `registry.rs` は名前と path しか持たない。**安定した workspace ID** を先に定義する
  必要がある(`artifact_ref` の一意性がこれに依存する)

## 実装順

1. 正本の更新(この ADR・FR-C8・contract.md・受入条件をテストへ)
2. モデル(workspace ID / manifest / ref / SHA-256 / policy / locator / expected_version / typed error)
3. store(3境界の物理分離・streaming hash・atomic import・再検証)
4. 転送(同梱 git-lfs の repo-local 設定・repo 外 storage・pointer 生成・
   upload-before-ref-push・skip-smudge pull・必要分の background fetch・進捗と劣化表示)
5. 書き込み経路の統合(picker / drop / paste / CLI / MCP を同一コア API へ)
6. resolver(ref / 固定 id / legacy path alias / Tauri streaming protocol / 検索・get・Context Pack)
7. UI(ノート内ファイル欄・詳細・取り込み・緩和確認・検索結果・ホーム集計)
8. 移行の実行(dry-run 棚卸し → sentinel 試験 → 二段階移行 → dual-read →
   多端末往復・quota 失敗・競合・大容量時のメモリ上限を検証)

**進捗(2026-08-13)**: 1〜6 完了。7 はノート内のファイル欄まで
(取り込み・外す・取り寄せる・新しい版として追加、旧添付は読み取り専用で併記)。
残りは詳細画面・取り込みダイアログ・緩和確認・検索結果・ホーム集計と、8 の移行。
**移行が済むまで旧添付は台帳に載らない**ので、Artifact の規則(役割・取得状態・
競合検知・検索)は旧データに効かない。

## 検討した代替案

- **第2 Git リポジトリ(`kb-files.git`)を CAS の裏に置く**: 既存の同期機構(flock・
  非対話 git・fail-open な `auto_push`)を引数化するだけで再利用でき、境界=リポジトリで
  構造的に分離できる。ただし大容量対応に独自の chunk protocol が必要で、
  binary 用途に LFS を使うという一般的な解から離れる。**乗り換え先としては維持**(下記)
- **Vault リポジトリの orphan ブランチ(`refs/blobs`)**: 追加設定ゼロだが、refspec を
  絞らないと手動 clone で全部落ちる。壊れ方が読みにくく、事故が静かに起きる
- **S3 / R2 等の汎用オブジェクトストレージ**: CAS としては最適だが、アカウント・
  バケット・鍵の3点をユーザーに作らせるため非エンジニア配布と両立しない。
  任意の追加 backend として後段なら価値がある
- **iCloud Drive / Syncthing 等に委ねる**: アプリから同期状態を検証できない。
  部分同期が静かに起き、競合時に複製が生まれ、照合と availability の意味が壊れる
- **Vault Git を正式な full store として温存する**: 移行の安全性は最も高いが、
  却下済みの「binary を Vault Git に直接保存する案」を恒久的に復活させる。
  既存分に限った読み取り専用の legacy transport なら、逸脱を移行の一項目に閉じ込められる
- **既存添付を移行せず旧経路のまま読む**: role・availability・競合検知・検索規則が
  旧データに一切効かなくなる

## 乗り換え条件(実測で1本でも倒れたら決定2を撤回する)

- **手動の fresh clone で LFS の自動取得を確実に抑止できない**(この方式の最大の弱点。
  `.lfsconfig` による除外・アプリ側の skip-smudge・明示 fetch の3層で塞ぐ想定だが、
  macOS / Windows で PoC し受入条件にする)
- 同梱 `git-lfs` が既存の GitHub 認証を macOS / Windows で安定して再利用できない
- quota・課金停止をアプリが十分に説明・分類できない

撤回する場合は単純な第2リポジトリではなく「第2リポジトリ + chunk CAS +
blobless partial clone」を採る。

## 残課題

- **参照(`Linked`)の所在を引く台帳**。repo ID から手元のパスを引けないので、
  取得状態を確かめられず、開くこともできない(2026-08-13 に実データで確認)。
  リポジトリの所在を持つ端末ローカルの台帳が要る。**`linked` を使い物にする前提**
- **旧 legacy ファイルの削除条件**。登録済み全端末での取得確認が理想だが、
  端末レジストリが無い。Artifact MVP では削除せず fallback を保持する
- ~~**安定した workspace ID の設計**(`registry.rs` の改定)~~ →
  実装時に**置き場所が違う**と判明した。`registry.json` は設定ディレクトリにある端末ローカルの
  台帳で同期されないため、そこへ置くと同じ保管庫が端末ごとに別 ID を持ち、
  Git で運ばれてきた台帳の参照先と噛み合わない。ID は保管庫と一緒に運ばれる必要があるので、
  **保管庫直下の追跡ファイル `.kb-workspace`** に置く(`.kb/` は索引 DB 用に ignore 済み)。
  同時初回起動の競合は `merge=union` で行を残し、読むときに古い方へ寄せて自己修復する
  (`crates/kb-core/src/workspace.rs`)
- LFS の quota / 帯域の使用量取得と表示。取得できない場合に「無制限・無料」とは
  表示しない。アプリが課金設定や budget を自動変更しない
- dataset の multi-file manifest、実削除を伴う GC、license / retention、
  OCR / embedding の自動生成は後段(正本の後段送りと同じ)
