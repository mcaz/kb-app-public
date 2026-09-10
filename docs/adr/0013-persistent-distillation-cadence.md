# ADR-0013: 継続蒸留cadenceは受入済みcheckpointを端末ローカルで保持する

- 状態: 採用
- 決定日: 2026-08-20
- 関連: 契約13、ADR-0010〜0012、GitHub Issue #61

## 背景

ADR-0012でcheckpointと増分worksetは作れたが、baselineの保管と「いつ、どの深さで再監査するか」は
呼び出し側に残っていた。会話中断後にbaselineファイルを人が選ぶ運用では、追加直後・日次・週次・月次の
継続運転を再現できず、最後に通った受入状態と単に観測した失敗状態も区別できない。

一方、時刻だけでsemantic executorを自動起動すると、全文判断とsnapshot固定requestの作成を飛ばしてしまう。
cadenceの責務は監査対象を選び、checkpointを安全に進めるところまでに限定する必要がある。

## 決定

### 1. checkpointはworkspace ID単位の端末ローカル状態に置く

`kb-app.distillation-cadence-state/v1`をアプリデータ領域へ置き、Vault pathやレジストリ名ではなく
`.kb-workspace`の不変IDで分離する。Vault、Markdown、remote repositoryへ同期しない。書き込みは
process間lockの内側で同一directoryの一時fileから置換し、途中JSONと同時runを防ぐ。

### 2. 受入gateがPASSしたときだけbaselineを進める

cadence runは保存済みcheckpointを`audit`へ渡す。actionable、unresolved、risk、Markdown outbox、
Storage Contract、local Git backupの全体gateが1つでも失敗した場合、checkpointと完了時刻を更新しない。
失敗audit ID・lane・failed checkだけを状態へ残し、次回も同じ差分を未処理として返す。

2026-09-08、Issue #88: 本文checkpointとは別に`accepted_artifact_stamp`を保存する。
manifest/ref JSONとaliasesの存在・内容の識別値を監査前後で比較し、一致した監査の値だけを受け入れる。
監査中の変更・監査後の読取失敗では`artifact_metadata_stable`が失敗し、checkpoint・台帳baseline・完了時刻を保持する。
監査前から台帳を読めない場合はrunをエラーとして停止し、baselineを更新しない。
毎発話の確認でLFSや旧添付のpayloadを再hashしない。payloadの検査はStorage Contract監査時に行う。
これは監査前後の一致確認であり、外部プロセスの変更をロックで排除したsnapshotを保証するものではない。

### 3. laneは固定深度として扱う

- `after_write`: accepted checkpointまたはArtifact台帳stampとの差分があればdue。追加・変更・移動を直接確認し、削除時は依存閉包も含める。昇格・rollback・ref/aliasだけの変更も監査する。
- `daily`: 24時間ごと。増分workset全体を確認する。
- `weekly`: 7日ごと。増分worksetに全active canonicalを加えて統合漏れを確認する。
- `monthly`: 30日ごと。全noteを鮮度・archive・delete候補の確認範囲にする。

端末timezoneと月の日数で結果が揺れないよう、v1の月次は暦月でなく30日間隔にする。初回は全laneをdueとし、
全体受入を通ったcheckpointをbaselineにする。明示laneは診断・再実行用で、省略時はdue laneを1回のauditへまとめる。

旧v1 stateで台帳baselineが欠落している場合は、現在値を受入済みと補完せず一度dueにする。
新コードは旧stateを読めるが、新field保存後のstateは旧バイナリの`deny_unknown_fields`により読めない。
バイナリだけのダウングレード互換を保証しない。旧hook出力のstatusはstamp未確認として読み込める。
本文plannerの派生cacheは維持し、cacheから読む場合も台帳stampと期限を再確認する。

### 4. cadenceはsemantic writeの権限ではない

cadence runが変更するのは端末ローカルstateだけで、KB、索引、care、outbox、remoteを変更しない。レビュー範囲の
全文確認と意味判断はAIが行い、normalize／revise／extractは従来どおりsnapshot固定executorへ渡す。
merge／supersede／split／deleteの境界も変えない。

## 公開面

- core: `distillation_cadence::status` / `run`
- MCP: `distillation_cadence_status` / `run_distillation_cadence`
- CLI: `kb distill cadence status` / `run [--lane ...]`
- machine contract: cadence run/state JSON Schema

## 結果

- 会話ごとにbaselineファイルを探さず、最後の受入成功点から再開できる。
- 失敗を観測しただけで差分が消えず、Storage Contractやbackup劣化を直した後に同じworksetを再監査できる。
- OS常駐schedulerが無くても、各AI surfaceが同じdue判定を呼べる。将来launchd等を追加しても、時刻・状態・
  gate判断をhost側へ複製せず同じcore APIを呼ぶだけでよい。
