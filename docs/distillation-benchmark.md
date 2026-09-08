# 自動蒸留の段階別計測

2026-09-07。数万件を前提とした設計検討用の探索的ベンチマーク。
性能予算を固定するCI gateではない。既存の検索・保存の10k gateとは測る経路が異なる。

## 実行

実KB・実AIを使わず、一時ディレクトリへ合成したノートで測る。
releaseビルドを使い、他のビルドや負荷試験を併走させない。

```bash
KB_DISTILL_BENCH_NOTES=10000 \
KB_DISTILL_BENCH_BODY_BYTES=4096 \
KB_DISTILL_BENCH_RELATIONS=2 \
KB_DISTILL_BENCH_SAMPLES=3 \
KB_DISTILL_BENCH_APPLIED_SAMPLES=3 \
cargo test --release -p kb-core \
  auto_distillation::tests::performance_benchmark::synthetic_distillation_stage_timings \
  -- --ignored --exact --nocapture --test-threads=1
```

辞書キャッシュを利用する環境では既存の `LINDERA_BUILD_DICTIONARY_CACHE_DIR` を設定する。
ノート数は100〜50,000、本文は256〜65,536 bytes、関係数は0〜8、反復数は1〜10。
更新ありの試料数は既定0で、明示したときだけ全MarkdownとGit基準版を構築する。
大量ファイルの準備には時間がかかるが、計測対象から分けて出力する。

## fixture

- 全件historical record。100件を同じscopeに置く。正本・添付・埋込みはない。
- 固定本文、固有タイトル・UID、3種類の検索語、前方のノートへのmentionsを持つ。
- 初期のノート・検索・関係索引は隔離DBへ一括構築する。蒸留登録triggerは有効。
- 全体整合性、件数、最初の書出し待ち0件を検証してから計時する。
- OSキャッシュは消さず、同じプロセスで異なるノートを順次処理する。

## 出力の範囲

JSONLに構築・初回open・warm open・各試料を分離して記録する。
未加工ログとともに、コードの版・fixture hash・端末・負荷・ビルド条件を残す。

| 出力                                   | 含まれる処理                                                  |
| -------------------------------------- | ------------------------------------------------------------- |
| `due_scan_ms`                          | 期限を迎えた再確認の登録。試料では対象0件                     |
| `claim_ms`                             | 待ちから1件を取得してleaseを確定                              |
| `prepare_ms`                           | 同一snapshotから最初の入力を準備                              |
| `extend_ms`                            | 追加検索と候補の全文取得。3回の条件では全文が1件から4件になる |
| `complete_validation_commit_export_ms` | 検証、DB確定、書出し。変更なしでは書出し対象0件               |
| `local_total_ms`                       | 上記の内部処理。更新あり試料ではdue scanを含まない            |
| `index_open_ms`                        | workerが毎件行うDB openを別計測                               |

更新あり試料は、全MarkdownとGit基準版の準備後、原記録のdescriptionをnormalizeする。
本文・タイトル・タグ保持、完了状態、実行履歴、書出し待ち0件を検証する。
remoteは設定せず、外部同期は実行しない。

AI応答、CLI起動・モデル確認、workerの2秒待機は `local_total_ms` に含まれない。
AI呼出し数とAI待機時間は0として明示する。0は実AIが即答したことを意味しない。
3標本では中央値と範囲を使い、p95や運用上の完了保証は示さない。

このfixtureは、全件数によって内部処理がどれだけ増えるかの切り分け用。
混在した正本、長文の分布、集中した被リンク、競合更新、AIの品質や応答時間を
代表する試験には使わない。
