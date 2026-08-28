# 統合retrieval core(integration/retrieval-core)の検証

- 実施日: 2026-08-28
- 対象: branch `integration/retrieval-core`(base: `origin/main` `91b9bf2`)。
  R4合意([retrieval-integration-protocol.md](retrieval-integration-protocol.md) を正本とする
  統合仕様)の C-1〜C-5 を統合し、本書が C-6(受入検証)の記録
- 比較対象: `origin/main` `91b9bf2`。同一端末(Darwin arm64)・同一fixture・serial実行
- 計測正本: `$SCRATCH/measurements/core/`(base側は `$SCRATCH/measurements/base/`)

## 統合の変更(C-1〜C-5)

| 段 | 内容 | 主なcommit |
| --- | --- | --- |
| C-1 | A案全量: migration state machine / 派生索引静的registry / open時自己修復 / embed stamp複合化 / 敵対的レビューF1・F2修正 | `6bd01d6`(merge `claude/derived-registry` @76c799d) |
| C-2 | GPT共通実験base(評価harnessのみ): retrieval-benchmark 1.1.0 / retrieval-eval 2.1.0(family・gate_mode・routine surface・required_rank)・`evaluate_with`・`--profiles` / `--rerank`・凍結装置(frozenテスト2本+ `docs/retrieval-experiment-base/*.json`)。ContextCard実装は除外 | `5396d4a` `9433acd` |
| C-2' | rerank軸はcoreでは`off`のみ有効(`on` / `indexed` / `auto` は明確なエラー) | `37d03a7` |
| C-3 | 配信profileの実体(RetrievalPlan: SearchPolicy / RetrievalOptions / PassagePolicy / OutputShape)。host既定は R4 I-2 で `session_auto`(既定変更のrationaleは [retrieval-profiles.md](retrieval-profiles.md) 冒頭)。未知profile fail-closed・tool schema非露出・hook子processの明示指定は維持 | `5ab4060`(merge `mcaz/claude-retrieval-profile` @158a929)+ `41ee590` |
| C-4 | 派生artifactのformat / generation framework(`derived_lifecycle.rs`): format key・dirty generation barrier(旧バイナリの素のSQL writeでも発火するtrigger)・二段階retirement(Retired→GC-eligible)・composite readiness。**coreの既存6 artifactには適用しない**(適用禁止は `validate_artifact_name` が機械的に強制)。規約は [derived-artifact-lifecycle.md](derived-artifact-lifecycle.md) | `62bd042` |
| C-5 | 統一評価protocol([retrieval-integration-protocol.md](retrieval-integration-protocol.md)): primary 4 arm / 事前凍結リスト / 自然なbaseline failure原則 / entry lane昇格前提 / 採用gate / 隔離DB手順 / G0二種 | `8c945ae` |

## 非変更(意味の中立)

- **検索アルゴリズム・ランキング・retrieval上限・SQL問い合わせの意味は一切変えていない。**
  下の G0(厳格版)が構造完全一致で裏づける
- durable schemaは8のまま(bumpなし)。durable table 6種のDDLも不変
- 通常経路(open_db / MCP / hook / 通常CLI)は C-4 のlifecycle機構を一切呼ばない。
  install / publish / retire / gc はすべて明示操作で、coreではbarrier trigger 0本・
  lifecycle meta鍵 0個(凍結テストで確認)
- rerankは`off`固定。ContextCard・fts_entryの実装はcoreに存在しない(評価用ブランチへ)
- 配信profileはhost既定 `session_auto` = 現行の候補展開予算そのもの
  (`evaluation` profileとの一致は`retrieval_profile.rs`のtestで固定)。
  `session_explicit`等は明示選択のみで、tool schemaへ露出しない

## G0結果(core neutrality・厳格版)

`origin/main` `91b9bf2` のbaseline出力と、統合coreの出力を
`$SCRATCH/measurements/core/check_neutrality.py` で突合した(時間値 `*_elapsed_us` のみ除外)。

| suite | 面(case×surface) | 突合方式 | 判定 |
| --- | ---: | --- | --- |
| official(google) | 33 | 全共通fieldの深い完全一致(candidate順・selected順・token・degradation・body requirements・omitted reason・gate・macro summary) | **一致** |
| holdout(realistic) | 36 | 同上 | **一致** |
| profile-context(controls 9 + challenges 57) | 66 | base `linked_v1` vs core `top3` / `linked_v1` / `profile:session_auto:rerank_off` の構造field突合 | **一致** |

- チェック総数 **23,203、FAIL 0**(`check_neutrality.out`)
- base側のprofile-context計測は、同一suiteをorigin/mainが読める1.0.0 / 2.0.0へ機械変換
  (`downconvert_profile_context.py`: `profiles` / `rerank` / `family` / `gate_mode` /
  `queries.routine` の除去のみ。query・required / relevant / excluded・body要求はbyte不変)して
  `wt/base`(origin/main)のバイナリで生成した
- core側だけに存在する面はroutine surfaceの3面(routine-template-01..03)のみ。
  gate_mode=candidates指定の3 caseもgate判定含め差は出なかった
- report envelopeの差は version表示(1.0.0→1.1.0 / 2.0.0→2.1.0)と純増fieldのみ:
  `profiles` / `rerank` / `fixture_digest` / `suite_schema_version` / `families` /
  `required_rank`系 / `gate_mode` / summary内 `gate_passed`・`gate_failed_cases`・
  `tokens_per_required`系 / `passage`(strategy設定表示)。既存fieldの欠落・値差は0
- 凍結・中立性guardテスト(release・`--exact`):
  `frozen_baseline_reports_match_the_current_control_suites` /
  `session_auto_profile_matches_linked_v1_structure_on_control_suites` /
  `profile_context_fixture_records_known_family_results` /
  `hook_profile_search_matches_the_pre_profile_default_path`(hook経路byte一致pin)/
  `core_rejects_rerank_on_with_a_clear_error` — **すべてok**

experiment stability G0(treatment armの凍結)はcoreにtreatment armが存在しないため対象なし。
凍結手順は [retrieval-integration-protocol.md](retrieval-integration-protocol.md) §7.2 のとおり
評価用ブランチ側で最初のdigestを基準点にする。

## 性能(10k gate、release・serial・2 run)

`cargo test --release -p kb-core search::tests::ten_thousand_note_performance_gate -- --ignored --exact --nocapture --test-threads=1`
を無負荷帯(load average 4〜6)で連続2回実行した。出力は
`$SCRATCH/measurements/core/perf-gate-run1.txt` / `perf-gate-run2.txt`。

| 指標 | CI予算 | run1 | run2 |
| --- | ---: | ---: | ---: |
| DB初回復元(index_rebuild) | 30,000 ms | 15,516 ms | 15,334 ms |
| home_db_read ×20 | 2,000 ms | 322 ms | 314 ms |
| category_list ×20 | 500 ms | 98 ms | 95 ms |
| note_list ×5 | 2,000 ms | 436 ms | 445 ms |
| keyword_search ×100 | 500 ms | 12 ms | 11 ms |
| semantic_search(KNN) ×10 | 1,000 ms | 231 ms | 257 ms |
| note_detail ×5 | 2,000 ms | 117 ms | 115 ms |
| linked_context ×20 | 2,000 ms | 2 ms | 2 ms |
| warm open ×5(A項目) | 2,000 ms | 148 ms | 145 ms |
| 単一note更新 median(A項目) | 25 ms | 4 ms(4,991 us) | 5 ms(5,201 us) |
| 単一note更新 p95(A項目) | 50 ms | 6 ms(6,719 us) | 6 ms(6,261 us) |
| 全artifact一括rebuild(A項目) | 10,000 ms | 648 ms | 637 ms |
| embed_pendingスキャン ×100(A項目) | 1,500 ms | 820 ms | 833 ms |

**A項目(warm open / note更新 / artifact rebuild / embed_pending_scan)は全部予算内**。
C-4の新機構はcoreの通常経路から呼ばれないため、write経路・open経路への加算はない
(barrier未installならtriggerは存在しない)。

## テスト集計

| 対象 | 結果 |
| --- | --- |
| kb-core lib(`cargo test --offline -p kb-core --lib`) | 395 passed / 4 failed / 3 ignored(failは既知のgithub連携4本のみ。新規失敗0) |
| kb-cli(`cargo test --offline -p kb-cli`) | 12+1 passed |
| kb-app tauri(`TAURI_CONFIG='{"bundle":{"externalBin":[]}}' cargo test --offline -p kb-app`) | 19 passed |
| 実事故再現5種([derived-registry.md](derived-registry.md) 受入テスト) | 5/5 ok(`missing_primary_fts_tables_are_rebuilt_and_search_recovers` / `missing_fts_anchor_alone_is_rebuilt` / `missing_note_relations_is_rebuilt_from_documents_and_writes_resume` / `governance_repair_failure_blocks_note_writes_but_keeps_search` / `repair_failure_rolls_back_that_artifact_and_leaves_durable_rows_untouched`) |
| F1 / F2(敵対的レビュー確定所見) | ok(`deleting_a_supersedes_source_is_rejected_before_it_orphans_the_target` / `steady_state_trusts_write_time_invalidation_without_rehash`) |
| 未知object保全(C-4-3 / R4危険7) | ok(`repair_and_rebuild_never_drop_unknown_objects`: check_and_repairとforce_rebuild全artifactが登録外table / index / trigger+行へ触れない) |
| 旧バイナリwriteのdirty化(R4危険5) | ok(`plain_sql_note_writes_from_another_connection_mark_the_artifact_dirty`) |
| fmt / clippy | `cargo fmt --check` clean / `cargo clippy --offline --workspace --all-targets` warning 0 |

既知の失敗はgithub連携の4本のみ(`origin/main`から継続、ネットワーク依存)。新規失敗0。

## R4危険リスト8項の除外宣言

R4 §4で「統合coreへ入れると危険」とされた8項は、いずれも本branchに**入れていない**。

| # | 危険項目 | coreでの扱い |
| --- | --- | --- |
| 1 | host既定のsession_explicit化 | 入れない。host既定は`session_auto`(candidate recall 85%回帰の既知)。`session_explicit`は明示選択のみ、再判断は別PRの4条件([retrieval-profiles.md](retrieval-profiles.md)冒頭) |
| 2 | routine rerank_on既定化 | 入れない(−1.12pt再現済み)。rerank軸自体がcoreでは`off`のみ有効で、`on`はエラー |
| 3 | 否定文保護なしfts_entry production ON | 入れない。fts_entry実装自体がcoreに存在しない。昇格前提4条件はprotocol §4 |
| 4 | 通常openで実験artifactを自動生成するregistry登録 | 入れない。registryは既存6 artifactのみ。lifecycle機構のinstallは明示操作で、通常openは新objectを作らない |
| 5 | dirty barrierなしのversionless machine artifact | 機構側で禁止方向へ倒した。format key必須・barrier未installは`BarrierMissing`でNotReady(fail-closed)・publishはrebuildと同一transaction強制 |
| 6 | 同名objectのin-place非互換format変更 | 規約で禁止(versioned object名 `_v1`→`_v2`。[derived-artifact-lifecycle.md](derived-artifact-lifecycle.md)) |
| 7 | registry外れartifactの自動DROP | しない。二段階retirement(Retired→GC-eligible→明示maintenanceの`gc_drop`のみ)。未知objectは絶対にDROPしない(テストで固定) |
| 8 | ContextCardの部分ready利用 | 型で遮断(`composite_readiness`: 全artifactがReadyかつ同一generationのときだけReady)。coreでは未使用 |

## 次段(評価用ブランチ)の予告

意味レーンの実体は本branchの上の評価用ブランチへ載せる。coreはそのための機構と評価契約だけを提供する。

- ContextCard系(note_context + link_anchors)とentry系(fts_entry)を1バイナリで実装し、
  activationは`kb eval`のclosed-world内部modeのみ(MCP / hook / 通常CLIから選択不能)
- 登録手順は `install_dirty_barrier` → immediate transactionでrebuild →
  `publish_ready_generation`(deferredはread→write昇格のSQLITE_BUSY衝突があり得る —
  失敗してもdirtyのままで安全側)。readerは`check_availability`がReadyのときだけquery、
  NotReadyは`degradation_for`(`artifact_not_ready`)を応答へ。ContextCardは
  `composite_readiness`で部分readyを遮断
- 比較は [retrieval-integration-protocol.md](retrieval-integration-protocol.md) の
  primary 4 arm+事前凍結+採用gateで行い、実KBは隔離DB複製(§6)のみ。
  隔離DBのdurable digest対象を定義する際、lifecycle系meta鍵
  (`artifact_format:` / `artifact_generation:` / `artifact_dirty:` / `artifact_retirement:` /
  `artifact_generation_seq`)の扱いを凍結時に固定すること(coreでは0個だが、
  評価ブランチはinstall後にmeta行を持つ)
- **意味採用(production ON)は別PR**(R4 I-8)。baseline中立を主張する本branchのPRと、
  意味を変える採用PRを分離する
