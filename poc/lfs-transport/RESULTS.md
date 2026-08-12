# PoC 結果: LFS 転送(lfs-transport)

実施日: 2026-08-13 / 実行: `./run.sh` と `./run.sh --remote https://github.com/mcaz/lfs-poc.git`
環境: git 2.53.0 / git-lfs 3.7.1 (darwin arm64)

## 判定: **PASS(7/7)**。乗り換え条件は3本とも立たない — 決定2 を維持する

ADR-0003 は full 転送に同一 origin の Git LFS を採ると決めたうえで、
**実測で倒れたら第2リポジトリ方式へ切り替える**乗り換え条件を3本付けていた。
**最大の弱点とされた「手動 clone で実体が落ちてこないか」も、認証も PASS。**
乗り換え条件(手動 clone の抑止・認証の再利用・枠の説明)は3本とも立たないので、
決定2(同一 origin の Git LFS)を維持する。

| # | 検証項目 | 判定 | 実測 |
|---|---|---|---|
| 1 | `content_hash` が LFS の OID と一致する | **PASS** | 3MB のランダムデータで `shasum -a 256` と pointer の `oid sha256:` が一致 |
| 2 | 保管庫に入るのは印(pointer)だけ | **PASS** | pointer は **132 バイト**(実体 3MB に対して 0.004%) |
| 3 | 実体の置き場を保管庫の外へ移せる(`lfs.storage`) | **PASS** | 外 1 件 / `.git/lfs` 0 件 |
| 4 | 後から設定しても既存の実体は移らない(順序の罠) | **PASS**(想定どおり) | 実体を作った後に張っても `.git/lfs` に残る |
| 5 | **手で普通に clone したとき実体が落ちてこない** | **PASS** | tracked な `.lfsconfig` の `fetchexclude = *` が効き、pointer のまま |
| 6 | 必要な実体だけ後から取れて、照合が一致する | **PASS** | `git lfs install --local` + `git lfs pull -I <path> -X ""` で取得、照合一致 |
| 7 | 既存の GitHub 認証を対話なしで使える | **PASS** | 使い捨ての private repo へ push 成功。資格情報の入力を求められない(3.0MB / 296KB/s) |

### 測り方

項目1〜4 はネットワーク不要。項目5〜7 は最初ローカルの bare リポジトリで代用して測り、
そのあと **GitHub 上の使い捨て private リポジトリ**(`mcaz/lfs-poc`)で測り直した。
両方で同じ結果。認証だけは代用できないので、本物の remote での実測が判定になる。

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

## 判定後にやったこと

- 転送層を実装した(`crates/kb-core/src/lfs.rs`)。上記の発見は ADR-0003 決定2 へ追記済み
- 実装中にさらに2件踏んだ。どちらもこの PoC では見えず、コードを書いて初めて出た:
  - **`Vault::commit`(libgit2)は LFS のフィルタを走らせない。** CLI が staged した
    pointer を `index.add_path` が実体で上書きする。LFS を通す書き込みは git CLI で行う
  - **属性が張られていないまま `add` すると実体がそのまま Git に入る。** 順序に依存する
    事故なので、取り込みの冒頭で設定を担保し、commit 後に pointer であることも確かめる

## 残っていること

枠(quota)の超過・停止時の見え方は未実測。実際に 10GiB を使い切る必要があるため、
**分類済みのエラーとして扱えること**をコード側の設計で担保し、実測は行っていない。
