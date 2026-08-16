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

## 結果

- 2台目が重複 Vault を作る事故と、別 Vault を誤って混ぜる事故を同じ identity gate で防げる。
- repository の visibility を後から変更した場合、次の upload は停止できる。ただし既に public に
  なったデータをアプリが取り消せるわけではないため、重大な劣化として案内する。
- GitHub 認証が未実装・失効中の環境ではバックアップが止まる。ローカルの書き込みは契約4どおり
  成功させるが、「同期済み」「復元可能」とは扱わない。
- 復元の再試行は検証済みobjectを再利用するため、大容量Vaultでも最初から全件を取り直さない。
  一方、Git履歴のcloneとStorage Contract検査は毎回やり直し、古い一時cloneを信用しない。
