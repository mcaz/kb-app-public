# PoC 結果: LFS 転送(lfs-transport)

実施日: 2026-08-13 / 実行: `./run.sh`(remote 未指定=ローカルの bare で代用)
環境: git 2.53.0 / git-lfs 3.7.1 (darwin arm64)

## 判定: **PASS(6/6)**。残るは認証の1項目のみ

ADR-0003 は full 転送に同一 origin の Git LFS を採ると決めたうえで、
**実測で倒れたら第2リポジトリ方式へ切り替える**乗り換え条件を3本付けていた。
**そのうち最大の弱点とされた「手動 clone で実体が落ちてこないか」が PASS。**
決定2 を維持してよい。

| # | 検証項目 | 判定 | 実測 |
|---|---|---|---|
| 1 | `content_hash` が LFS の OID と一致する | **PASS** | 3MB のランダムデータで `shasum -a 256` と pointer の `oid sha256:` が一致 |
| 2 | 保管庫に入るのは印(pointer)だけ | **PASS** | pointer は **132 バイト**(実体 3MB に対して 0.004%) |
| 3 | 実体の置き場を保管庫の外へ移せる(`lfs.storage`) | **PASS** | 外 1 件 / `.git/lfs` 0 件 |
| 4 | 後から設定しても既存の実体は移らない(順序の罠) | **PASS**(想定どおり) | 実体を作った後に張っても `.git/lfs` に残る |
| 5 | **手で普通に clone したとき実体が落ちてこない** | **PASS** | tracked な `.lfsconfig` の `fetchexclude = *` が効き、pointer のまま |
| 6 | 必要な実体だけ後から取れて、照合が一致する | **PASS** | `git lfs install --local` + `git lfs pull -I <path> -X ""` で取得、照合一致 |
| 7 | 既存の GitHub 認証を対話なしで使える | 未実施 | 本物の remote が要る |

### ローカル代用について

項目5・6 は**ローカルの bare リポジトリ**を remote にして測った。実体の取得と除外は
転送方式によらず同じ経路(smudge フィルタと fetch 設定)を通るので代用が成立する。
**認証(項目7)だけは代用できない**ので、本物の remote を用意したときに測る。

## 実装に効く発見(3件)

### 1. `lfs.storage` は「最初の実体を作る前」に張る

最初の測定は項目3 が FAIL したが、原因は LFS ではなく測る側の順序ミスだった
(`git add` で実体が `.git/lfs` へ書かれた**後**に設定していた)。
張り直しても既にある実体は移らない(項目4)。

→ **製品では「保管庫を作った直後・clone した直後」に張る。**
`connect.rs` の `ensure_merge_config` が clone した側でも毎回冪等に merge 属性を
張り直しているのと**同じ場所・同じ理由**。遅延して張ると、それまでに取り込んだ
実体が保管庫の中に取り残される。

### 2. 素の clone には LFS のフィルタが張られていない

`git clone` しただけの作業ツリーで `git lfs pull` すると
`Git LFS is not installed for this repository` で checkout が飛ばされる。

→ アプリが取得を担うなら、**clone した直後に `git lfs install --local` が要る**。
これは同時に項目5 の PASS を支えている仕組みでもある — 素の clone は
フィルタも `.lfsconfig` の除外も効いて、**実体を落とさない**。

### 3. 狙った実体を取るには `-X ""` が要る

`.lfsconfig` に `fetchexclude = *` を書いた状態で `git lfs pull -I <path>` を
実行しても取得されない。**include は exclude を上書きしない。**
`git lfs pull -I <path> -X ""` とするか、`lfs.fetchexclude` をローカルで空に
上書きする必要がある(どちらでも取得・照合一致を確認)。

→ これを知らずに実装すると「取り寄せる」を押しても何も起きない。
M1 の「取り寄せる」導線が黙って失敗する形になるので、**取得コマンドの
組み立てをコアに閉じ込め**、UI から直接 `git lfs` を叩かせない。

## 判定後にやること

- 転送層の実装へ進んでよい(ADR-0003 実装順 4)。上記3件は実装メモとして ADR へ残す
- 認証(項目7)は本物の remote を用意した時点で測る。
  ここが倒れたら乗り換え条件に該当するので、転送層をマージする前に確認する

```bash
./run.sh --remote git@github.com:<owner>/<scratch>.git
```
