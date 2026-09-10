# 派生artifactのformat/generation framework(C-4 / R4 I-4)

- 実装: `crates/kb-core/src/derived_lifecycle.rs`(統合core、2026-08-28)
- 根拠: R4討議 I-4(amend)。「durable schema versionとderived artifact formatの分離」は
  approveだが、旧writerによるstale化・incompatible format・削除時の世代間ping-pongを
  防ぐ規約が必須、という条件を機構に落としたもの
- 適用範囲: **coreのregistry artifact(fts_main / fts_tri / links / fts_anchor /
  fts_events / note_relations / note_vecs)には適用しない**。機構+テストのみをcoreへ置き、最初の
  利用者は評価用ブランチの `fts_entry_v1` / `note_context_v1` / `link_anchors_v1`。
  既存registry object名・durable table名は `validate_artifact_name` が拒否する

## 責務の分離

| 層 | 宣言するもの | 置き場所 |
|---|---|---|
| DB schema version | durable/runtime構造の互換性 | `meta['schema']`(現在 8) |
| artifact format | 派生object固有の互換性 | `meta['artifact_format:<name>']` |
| ready generation | どのsource状態から構築されたか | `meta['artifact_generation:<name>']`(共有sequence `artifact_generation_seq` から採番) |
| dirty flag | 最終publish以後のnotes変化 | `meta['artifact_dirty:<name>']`(barrier triggerが書く) |
| retirement | 廃止の段階 | `meta['artifact_retirement:<name>']`(`retired` / `gc_eligible`) |

この規律を満たす限り、派生artifactの追加はdurable schema bumpなしで行える。
ただし「schema bump不要」は「互換性管理不要」ではない — 互換性の責任をartifact
formatへ移しただけである(R4 I-4)。

## 必須規約1: format key と versioned object名

- readerは `check_availability(conn, name, expected_format)` が `Ready` を返した
  ときだけartifact objectへqueryする。format不一致・未publish・dirty・retiredの
  objectは**queryしない**(旧readerが旧SQLで新formatを開く事故を作らない)
- **非互換のformat変更は、同名objectのin-place上書きで行わない**。新しい
  versioned object名で構築する:

  ```text
  fts_entry_v1  →(非互換変更)→  fts_entry_v2
  ```

  旧objectは残置したまま新objectをshadow buildし、readerの期待formatを切り替え、
  旧objectは二段階retirement(下記)で片付ける。互換の範囲内の変更(行の再導出だけで
  済むもの)は同名のままrebuild+publishでよい
- format keyだけの軽量guardには `declared_format` を使える

## 必須規約2: dirty generation barrier

旧バイナリは新artifactを更新しない。schema 8のまま併存すると、note更新後も
新バイナリが構築したentry/contextが残り、staleを黙って返す。これを塞ぐのが
dirty generation barrier:

- `install_dirty_barrier` がregistry所有のSQLite trigger 3本
  (`artifact_barrier_<name>_{insert,update,delete}`、対象は `notes`)をinstallする。
  **triggerはDBファイル内に住むため、この機構を知らない旧バイナリのwriteでも
  発火する**(`plain_sql_note_writes_from_another_connection_mark_the_artifact_dirty`
  が生接続の素のSQLで固定)
- dirty中のartifactは `check_availability` がNotReadyを返し、readerはbaseline検索へ
  fallbackして劣化 `artifact_not_ready`(`degradation_for`)を応答へ載せる。
  結果を空と偽らない
- rebuild完了時は**同一transaction内**で `publish_ready_generation` を呼ぶ。
  dirty解除・format宣言・generation発行が構築と不可分になる(autocommit接続は拒否)。
  rebuild transactionはimmediateで始める(deferredのread→write昇格は並行writerと
  SQLITE_BUSYで衝突し得る。失敗してもdirtyのままなので安全側に倒れる)
- barrier未installのartifactへのpublishは拒否する(「dirty barrierなしの
  versionless machine artifact」= R4 §4危険リスト5を機構で禁止)

## 必須規約3: 二段階retirement

「registryから外したからDROP」はmixed-version環境で再生成・再削除のping-pongを
作る(R4 §4危険リスト7)。廃止は必ず二段階:

1. `retire_artifact` → **Retired**: 新readerは無視する。objectは残置(通常openの
   自己修復も触らない)。旧バイナリが再生成しても実害はreaderが遮断する。
   Retired中の再publishは拒否
2. `mark_gc_eligible` → **GC-eligible**: compatibility window経過後の明示操作。
   Retiredを経ていないartifactには適用できない
3. `gc_drop` → 明示maintenanceでのみDROP。対象は「artifact名そのもの、または
   `<name>_` prefixを持つ」objectに限る。それ以外の名前は未知objectとして拒否する

**registryは未知objectを絶対にDROPしない。** 既存registryの自己修復
(`check_and_repair`)と強制rebuild(`force_rebuild`)がspec登録objectしか
DROPしないことは `repair_and_rebuild_never_drop_unknown_objects`
(derived_index.rs)が登録外のtable・index・trigger+行の保全まで含めて固定する。

## composite capability readiness(coreでは未使用)

複数artifactを合成するcapability(例: ContextCard = note_context && link_anchors)は
`composite_readiness` で判定する:

```text
ContextCardCapability.ready =
    NoteContext.ready
    && LinkAnchors.ready
    && same_source_generation
```

- 同じ `publish_ready_generation` 呼び出しで発行したartifact群は同一generationを
  共有する(共有sequenceから1値を配る)
- 片方だけ再publishするとgenerationが割れ、capability全体がfallbackになる —
  **別generationの産物を混ぜない**(R4 §2)。部分ready利用はしない(R4 §4危険リスト8)

## coreが変えないこと

- 既存6 artifactはこの機構を使わない(barrier trigger 0本・meta鍵 0個)。
  通常openの経路・検索意味・G0構造一致に影響はない
- 通常openで実験artifactを自動生成するregistry登録は行わない(R4 §4危険リスト4)。
  install/publish/retire/gcはすべて呼び出し側(評価用ブランチ・明示maintenance)の
  明示操作である
