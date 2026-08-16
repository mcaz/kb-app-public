# ADR-0005: private backup は「新規作成」と「既存 Vault へ参加」を分ける

- 状態: **採用**(2026-08-16 本人決定)
- 関連: [contract.md](../contract.md) 契約7 / [requirements.md](../requirements.md) FR-A6 /
  [ADR-0003](0003-artifact-storage-and-transport.md) / [ADR-0004](0004-storage-contract.md)

## 文脈

Full Artifact は Git LFS を介して個人ノートの実体を GitHub へ送る。URL の構文や `git push`
の成功だけでは、接続先が private かは分からない。また「アプリが private repository を作る」
だけでは、2台目以降で既存 Vault と別の repository を作り、履歴を分裂させる。

repository の名前・URL・ローカル path は変更可能で端末ごとにも異なる。Vault の同一性には
Git で追跡される `.kb-workspace` の永続 ID が既にある。

## 決定

1. 初回のバックアップ導線を、次の2操作へ明示的に分ける。
   - **新しく作る**: アプリが private repository を作成し、private + push 権限を再確認してから
     現在の Vault を初回 push する。
   - **既存 Vault を使う**: ユーザーが明示的に選んだ private repository を clone し、Storage
     Contract を検査してこの端末へ復元する。候補を自動選択しない。
2. 既にローカル Vault がある状態で既存 repository を接続する場合、両者の
   `.kb-workspace` が同じときだけ通常の pull/rebase へ進む。異なる ID で双方にデータがある場合、
   自動 merge・上書き・push を行わず、「別 Vault として登録」か明示的な import/migration に分ける。
3. repository に `.kb-workspace` が無い、Storage Contract が壊れている、GitHub API で private +
   push 権限を確認できない場合は、通常のバックアップ先として接続しない。
4. privacy の確認は接続時だけでなく、ノートと LFS object の各 upload の直前にも認証済み
   GitHub API で行う。public / internal / 401 / 404 / network error / ambiguous はすべて fail-closed。
   gate の失敗は同期 sidecar の latch として残し、後続の正常な pull では消さない。認証済みAPIで
   private + push 権限を再確認できたときだけ解除する。non-fast-forward 後の retry push も別の
   upload として直前に再検査し、最初の検査結果を使い回さない。
5. fresh clone では LFS の暗黙 smudge に依存せず、tracked manifest の `full` object を列挙して
   全件 fetch し、`content_hash` を照合する。これを終えるまで「復元済み」と表示しない。
6. 復元は検査・clone・Full object検証・登録の進捗をTauriイベントで表示する。取得済みobjectは
   `.kb-workspace` 単位の端末LFS storeに残し、再試行時にhashが一致するものだけを再利用する。
   中断等で不完全なobjectが残った場合は、そのobjectだけを破棄して再取得する。一時cloneのpathを
   再開状態の正本にはしない。
7. GitHub APIとGit/Git LFSの失敗は、認証・権限・privacy・通信・repository欠損・quota・
   LFS未導入・remote object欠損・hash不一致・Storage Contract違反・workspace不一致・競合等の
   typed reasonへ境界で分類する。画面と同期sidecarは同じreasonを使い、providerのstderrだけを
   利用者向け文言にしない。
8. GitHub 認証は OAuth App の device flow とし、`repo` scope の範囲を開始前に表示する。
   token取得後は `/user` で account を検証してから、access token・任意のrefresh token・期限・
   account IDをOSキーチェーンへ保存する。client secretとtokenを設定ファイルへ置かず、401を
   受けたcredentialは削除して再サインインを求める。
9. Git/Git LFSにはキーチェーンから読んだtokenをprocess限定の`http.extraHeader`として渡す。
   command line・remote URL・`.git/config`へtokenを残さない。API検査後はGitHubが返したcanonical
   HTTPS clone URLを使い、入力がSSH URLでも同じOAuth transportへ正規化する。

## 結果

- 2台目が重複 Vault を作る事故と、別 Vault を誤って混ぜる事故を同じ identity gate で防げる。
- repository の visibility を後から変更した場合、次の upload は停止できる。ただし既に public に
  なったデータをアプリが取り消せるわけではないため、重大な劣化として案内する。
- OAuth App client IDが未設定、未サインイン、失効中の環境ではバックアップが止まる。ローカルの
  書き込みは契約4どおり成功させるが、「同期済み」「復元可能」とは扱わない。
- 2台目の初回復元でも同じアプリ内サインインを先に通すため、system Gitのcredential helperや
  `gh` CLIの設定有無に依存しない。
- 復元の再試行は検証済みobjectを再利用するため、大容量Vaultでも最初から全件を取り直さない。
  一方、Git履歴のcloneとStorage Contract検査は毎回やり直し、古い一時cloneを信用しない。

## 機械化回帰（2026-08-17）

- localhostのHTTP fixtureで、private + push許可の成功応答と、401／403／404／通信断／不正JSONの
  `BackupFailureKind` 写像を実リクエスト境界から固定した。
- bare Git remoteを使い、gate失敗時にremote refが進まないこと、成功pull後もlatchと画面用errorが
  残ること、private再確認後だけpushと解除が成立することを固定した。
- non-fast-forwardのrebase retryではgateが2回呼ばれ、各pushの直前に検査されることを固定した。

## 実受入(2026-08-16)

- `mcaz` 所有の OAuth App `kb-app` を登録し、Device Flow と期限付き access token を有効化した。
  client secret は作成していない。
- `repo` scope を認可する画面を確認し、`/user` の検証後に `mcaz` としてOSキーチェーンへ保存できた。
  Organizationへのアクセス要求は行っていない。
- 非機密 fixture 専用の `mcaz/kb-app-oauth-acceptance-20260816` をアプリのAPI経路で作成し、
  `private=true`、`visibility=private`、`permissions.push=true` を再確認してから初回pushした。
- 同じHTTPS clone URLから別ディレクトリへ復元し、Storage Contractとfixtureノートの内容を確認した。
  source / restored双方のremote URLとGit設定を走査し、tokenや認証headerが残っていないことを確認した。
