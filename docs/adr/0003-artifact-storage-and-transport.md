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
`sync_policy: local_only | full` は MVP のモデルに含まれる。`manifest_only` は
2026-08-15 に廃止した。

### 2. 正式な full 転送は同一 Vault origin の Git LFS。実行環境は同梱する

> **2026-08-13 追記(実装で確定した3点)。**
> (a) `full` の実体は **LFS の置き場が持つ**。自前 CAS([`store`])が持つのは
> `local_only` だけ — 両方に置くと二重保存になる。
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

> **2026-08-14 追記(2件)。**
>
> 1. client repo 由来の既定は **`managed + local_only`** に変わった(決定11)。
>    実体はローカルストアへ複製し、外へは出さない。
> 2. 経路一本化は**実施済み**。`useNoteFileIntake.ts` は `api.attachmentAdd` ではなく
>    `api.fileAdd` / `api.fileAddFromClipboard` を呼び、`intake::take` へ合流している。
>    上の記述は改定前の状態を指すので、現状の根拠として読まないこと。

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
- `local_only` に固定上限を置かない。`full` のみ 100MB 警告・
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

### 11. 保存方法は一つ。実体は常に保管庫が持つ(2026-08-14)

`Keep`(`managed` / `linked`)の二択を**廃止**する。実体は常に保管庫が持つ。

`linked` は「参照は作れるが開けない」状態のまま実データで確認されている(残課題に記載)。
`Locator::Linked` の availability は `crates/kb-core/src/store.rs` で常に `false` を返し、
必ず「この端末にファイルがありません」と表示される。所在台帳を実装して成立させる道もあるが、
**落とす方を採る**。

- 新規 record の `Locator` は `Managed` だけ。`LegacyGit` と既存 `Linked` は
  **互換読み取り専用**で残し、新規生成と更新を許可しない
- client repo 由来は `managed + local_only`。**業務ファイルの実体が保管庫のローカルストアに
  複製されることを受け入れる**代わりに、所在台帳・availability 不能・開けない添付を捨てる。
  `local_only` なので Vault Git にも LFS にも出ない
- 「このリポジトリは他の端末から辿れない。コピーして保管してください」で弾く必要が無くなる。
  remote の無い repo のファイルも複製できる
- 取り込みで決めることが4つから3つに減る(閲覧区分・同期内容・役割)

**trade-off**: 大きいデータセットを二重に持ちたくない、という `linked` 本来の用途は失われる。
必要になったら multi-file manifest(後段)と合わせて設計し直す。

### 12. 取り込み口は二つ、着地は一つ(2026-08-14)

実体の持ち方は決定11 で一つになったが、**入り口は二つ**ある。

- **path 経路** — 手元のファイルを複製する。picker / drop / paste / CLI。
  streaming は決定8 のまま
- **content 経路** — 生成物を、元ファイルを介さずそのまま置く。bytes を直接受け取って
  store へ書く。**元ファイルが存在しない**ので複製ではない

**MCP には content 経路だけを渡す。MCP は path を受け取らない。**

理由は決定10 と同じ形(能力自体を渡さない)だが、守る対象が違う。MCP サーバーは
AI 本体より広いホスト権限でファイルを開ける。path を受ければ、外部文書に混入した指示が
任意のファイルを取り込ませ、`private + full` で LFS へ載り、後続のノート操作の
`auto_push` が未 push の Artifact commit と LFS object ごと外へ送る。

**`private` は転送を止めない。**`intake.rs` の保存先分岐は `policy.sync` だけを見ており、
`sensitivity` は関与しない。したがって「既定を private にしておけば安全」は成立しない。

allowlist や path 検証で塞ぐ設計は採らない。**能力自体を渡さなければ、検証漏れが無い。**

- content 経路にはサイズ上限を置く。IPC とメモリに全量が載るため、`full` の 100MB 警告とは
  別の、十分小さい値にする(具体値は未決)。clipboard 経路と同じ理由・別の文言で扱う
- `origin` / `by` / `at` / media type / policy は**サーバー側で固定**し、MCP 引数にしない
- `note_id` が実在することを確認してから書く
- 応答には、実際に適用された policy・locator・artifact ID・version・ref・警告・同期の劣化を
  構造化して返す

将来 path 経路を MCP へ開く必要が出た場合も、生の絶対パスは受けない。
app 管理の staging に対する不透明なトークンか、ユーザーがその場で許可した
単一ファイルの capability を受ける。

### 13. 取り込み元を追跡する。検知は自動、版の判断は人間(2026-08-15)

決定11 で `linked` を落としたが、`linked` が持っていた価値**「元ファイルが変わったと分かる」**
は失いたくない。実体を複製した上で取り込み元を覚えておけば、その価値は実体付きで戻る。
開けない参照だった `linked` の上位互換になる。

**追跡情報は locator ではなく、端末ローカルの sidecar に置く。同期しない。**

- 持つもの: 取り込み元の絶対パス、取り込み時の `content_hash`、取り込み時の mtime と size
- **「locator に裸の絶対パスを持たせない」(決定6・正本の受入条件)には抵触しない。**
  この情報は実体の解決に一切使わないため。解決は `Locator::Managed { hash }` のまま
- 同期しない理由は二つ。端末ごとにパスが違って辿れないことと、
  **ローカルのディレクトリ構成が他端末や Git 履歴へ漏れる**こと
- 別の端末では追跡情報が無いだけ。実体は `full` で届いているので普通に開ける。
  「この端末では元ファイルの変化を追えません」と出す(freshness unknown の語彙に乗る)
- 追跡情報を持つのは **path 経路だけ**。content 経路(決定12)には元ファイルが存在しない

**検知は lazy でよい。常駐 watcher は置かない。**

ノートを開いたとき・明示的な確認操作・同期のついで、のいずれかで、記録した mtime と size を
比べる。同じならファイルを読む必要すら無いのでほぼ無料。違うときだけ hash を取り直す。

元ファイルが移動・削除されているのは異常ではない。エラーにせず「確認できません」として扱う。

**「別の内容かどうか」を機械が判定しない。**

検知した後に「これは更新か、それとも同じパスに置かれた別物か」を自動で判断しては**ならない**。
決定7 のとおり内容は不変で、版は `supersedes` で繋ぐ。そして **MVP に GC が無い**。
つまり誤って別ファイルを更新版として繋いだら、その版は**二度と消せない**。
全面書き換えと差し替えは機械的に区別できないので、この判定は本質的に外れる。

- **検知は自動**(hash が変わった、という事実だけを出す)
- **版として繋ぐか、別物として扱うか、無視するかは人間が決める**
- ノート本文への反映は AI が行ってよい(AI ノートは AI の持ち物)。ただし丸ごと書き直さず、
  古くなった記述を特定して直す。ノートは元ファイルの写しではなく解釈であるため

**副作用**: 版が積み上がるので、容量の単調増加が早まる。決定11 と合わせて、
空き容量の検査・累積使用量の表示・保存期間と GC の方針を実装順へ前倒しする(下記)。

### 14. 版は Git の履歴ではなく `supersedes` で管理する(2026-08-15・検討したが採らない)

**検討した案**: 同じファイルを追加するとき、実体を別の Artifact として持たせず、
Git のパス履歴で版を管理する。

**採らない。**理由は五つある。

1. **パス履歴では版が繋がらない。**`full` の pointer は既に Vault Git の中にある
   (`lfs::import` が `<vault>/<台帳>/lfs/<hash>` を staged し、作業ツリーでは pointer へ縮める)。
   つまり履歴自体はもうある。しかし**配置が hash 基準**なので、新しい版は別のパスになる。
   パスの履歴を辿っても前の版へは辿り着けない。繋いでいるのは manifest の `supersedes` である。
   識別子基準の配置(1つの論理ファイル = 1つの固定パス)へ変えれば繋がるが、
   **CAS の重複排除を捨てる**ことになる(同じ内容を2つのノートに付けると2パスになる)
2. **容量は減らない。**LFS のオブジェクトは差分圧縮されない。版ごとに完全なコピーが増える。
   差分が効くのは Git 本体の packfile だが、バイナリを Git 本体に入れないことが決定2 そのもの
3. **`local_only` は Git に何も無い。**台帳は sidecar にあり、
   「保管庫の中にあるものだけ commit する」。結局、版管理の機構を二つ持つことになる
4. **決定5 の出典固定が壊れる。**版が「あるパスのあるコミット」になると、固定に (path, commit) が要る。
   `kb-artifact:<artifact_id>` が **Git に依存せず解決できる**ことが利点だったので、それを失う
5. **決定9 と衝突する。**Git の履歴は全端末で同じだが availability は端末ごとに導出する。
   「履歴に版がある」と「この端末で開ける」は別で、`lfs.rs` にも
   「pointer があるだけでは『ある』と言わない」と書いてある

**容量が動機なら道は別にある。**版をまたいだ重複排除が本当に要る段階になったら、
乗り換え先として既に定義してある **第2リポジトリ + chunk CAS + blobless partial clone** へ移る。
chunk 単位の CAS ならファイル種別を問わず効き、Git のセマンティクスは要らない。

**「履歴を Git で持つ」は台帳については既にそうなっている。**manifest は小さな JSON なので
Git の差分がよく効き、`Ledger::put` が commit している。「このファイルに何が起きたか」は
既に追える。足りないのは push だけで、commit 失敗を握り潰すのは
**契約4「同期は派生。失敗しても書き込み自体は成功」による意図的な設計**である
(欠けているのは `auto_push` を呼んでいないこと)。

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
3. store(`local_only` CAS と `full` LFS の物理分離・streaming hash・atomic import・再検証)
4. 転送(同梱 git-lfs の repo-local 設定・repo 外 storage・pointer 生成・
   upload-before-ref-push・skip-smudge pull・必要分の background fetch・進捗と劣化表示)
5. 書き込み経路の統合(path を扱う picker / drop / paste / CLI と、content だけを扱う
   MCP を同じ store / ledger のコア処理へ)
6. resolver(ref / 固定 id / legacy path alias / Tauri streaming protocol / 検索・get・Context Pack)
7. UI(ノート内ファイル欄・詳細・取り込み・緩和確認・検索結果・ホーム集計)
8. 移行の実行(dry-run 棚卸し → sentinel 試験 → 二段階移行 → dual-read →
   多端末往復・quota 失敗・競合・大容量時のメモリ上限を検証)

> **2026-08-14 追記。** 決定11・12 を受けて、7 の「取り込みダイアログ」は**作らない**。
> 未実装のまま仕様から落とす(保存方法が消えて3項目になり、既定で置いて後から直す方が速い)。
> ただし `local_only`・`transcript` を**最初から**指定したい場面は残るので、
> モーダルではない事前選択か、取り込み後のインライン変更のどちらかは要る。
> **不可逆な転送境界を常に後決めにはしない**(一度 `full` で入れたものを後から狭めても、
> Git と LFS の履歴からは自動で消えない)。
>
> 決定12 に向けた順序を 9 以降として足す。
>
> 9. **読み取り面**(`get` に Artifact を含めるか、read-only の一覧を追加)
> 10. **コアの硬化**(note 実在確認・プロセス間ロック・auto_push と劣化応答・
>     path 経路の regular file 検査と同一 FD での snapshot)
> 11. **content ベースの `attach`**(新規のみ。`supersedes` は含めない。
>     policy / confirmed / origin / by / at は受け取らない。応答は構造化)
> 12. **`detach`**(`artifact_id` + `note_id` + `expected_version` 必須。
>     関係が無いときは成功扱いにしない。本文リンクは別途残ることを応答に含める)
> 13. **意味付けの API**(表示名・役割・検索対象の更新、参照名の作成と改名。
>     policy は含めない。役割と検索対象は原子的に更新する)
> 14. **`supersedes` の公開**(前版の policy 継承・関係・version・ref revision・
>     多重実行と並行実行を試験してから)
>
> **2026-08-15 追記。** 決定11・13 で容量が単調増加するため、次の段を**前倒しする**。
> 14 の後ではなく、実体を複製し始める前に入れる。
>
> - **容量の可視化と保全** — 空き容量の検査、累積使用量の表示、開くたびに作られる
>   temp 複製の掃除、保存期間の方針。GC は残課題から実装順へ移した
> - **取り込み元の追跡**(決定13) — 端末ローカル sidecar への記録、lazy な変化検知、
>   変化の提示。**版として繋ぐ判断は人間に残す**ので、`supersedes`(14)より前に出せる

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

- ~~**参照(`Linked`)の所在を引く台帳**。repo ID から手元のパスを引けないので、
  取得状態を確かめられず、開くこともできない(2026-08-13 に実データで確認)。
  リポジトリの所在を持つ端末ローカルの台帳が要る~~ →
  **2026-08-14: 新規 `linked` を廃止して解消**(決定11)。台帳は作らない。
  既存 `Linked` record は互換読み取り専用で残す
- **AI に読み取り面が無い**。MCP の `get` は旧 `<id>.files/` だけを返し、新 Artifact を
  返さない(`crates/kb-core/src/mcp.rs`)。artifact ID・version・role・ref・実際の policy・
  availability を AI が取得できないため、`detach` も意味付けも成立しない。
  **書き込み(決定12)より先に出す**
- **Artifact の書き込みが push されない**。`Ledger::put` は commit するが失敗を捨て、
  `auto_push` もしない。取り込みの成功が「他端末へ運ばれた」ことを意味しない。
  upload-before-ref-push と劣化応答を仕上げる必要がある
- **`local_only` の Artifact への本文リンク**。manifest / ref は端末ローカルにしか無いのに、
  本文へ書いた参照文字列だけは Git で同期される。別端末では解決できず、名前も漏れる。
  `local_only` はファイル欄にだけ出し、同期されるノート本文へは書かない規則が要る
- **GUI と MCP の同時書き込み**。MCP は Tauri の `AppState` mutex の外にある別プロセスで、
  ref 衝突確認 → manifest 書き込み → ref 書き込みが一操作としてロックされていない。
  MCP を開くと競合頻度が上がるため、保管庫単位のプロセス間ロックと ref revision の
  実 CAS が要る
- **ノートを消したときに Artifact 側の関係が残る**。削除済みノート ID が台帳に残るため、
  ノートの生存期間と Artifact の関係の同伴規則を決める
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
- ~~実削除を伴う GC は後段~~ → **2026-08-15: 実装順へ前倒し**(決定11・13)。
  実体を常に複製し、かつ版が積み上がるため、容量が単調増加する。
  `detach` は実体を消さず、ノートを消しても Artifact の関係は残る。
  開くたびに temp 領域へ表示名付きの複製を作って消していない経路もある。
  空き容量の検査・累積使用量の表示・temp の掃除・保存期間の方針を、
  単一の巨大ファイルでディスクが枯渇する前に入れる
- dataset の multi-file manifest、license / retention、
  OCR / embedding の自動生成は後段(正本の後段送りと同じ)
- **取り込み元の追跡(決定13)の保存場所**。端末ローカルの sidecar に置くことは決めたが、
  台帳の sidecar と同居させるか別ファイルにするかは未決。
  Artifact が消えたときに追跡情報も一緒に消える経路が要る
