# Artifact昇格後の監査

2026-09-08、[Issue #88](https://github.com/mcaz/kb-app/issues/88)。
Storage Contractの旧ファイル総数だけでは、未昇格と昇格後に保持するコピーを区別できなかった。

`audit_distillation`とCLI `kb distill audit --format json`のStorageReportは、従来の`legacy_files`に加え、
`legacy_inventory`を返す。昇格planを作り直す操作やファイル削除は必要ない。

| field | 数える対象 |
| --- | --- |
| `unpromoted_artifacts` | 現在のprimary locatorがLegacyGitのArtifact |
| `active_legacy_files` | 現在のLegacyGit locatorが使う物理path |
| `retained_legacy_files` | 昇格の証拠と現在の参照・旧実体が一致する保持専用path |
| `unclassified_legacy_files` | 上記へ証明できない旧実体path |

物理3分類の合計は`legacy_files`と一致する。Artifact件数は足さない。
共有pathは一度だけ数え、一つでもLegacyGitが使っていれば現役を優先する。
`unpromoted_artifacts=0`、`unclassified_legacy_files=0`、保持あり、Storage validであれば
「物理昇格完了・旧実体保持中」と読める。意味監査やremote backupも含めた全体gateのPASSとは別の結果である。

新しい昇格は、既存の`legacy-promoted`に加え、型付きPromotionPlanをdetailへ保持する
`legacy-promotion-proof`を同じmanifestへ記録する。schema・plan digest・正規path・destination、
Artifact ID/hash/size/version、ref revisionとalias一覧、旧実体path/hash/sizeを照合する。
現在のManaged locatorと最新の昇格証拠に整合しない実体は保持専用へ数えない。
構造化証拠の破損や、保持を記録した旧実体の欠損・改変は監査エラーとして返す。

旧版の散文eventだけから証拠を補完しない。同じhashの別ファイルも証拠にしない。
そのため、旧版で昇格済みのデータはplanが0件でも旧コピーが未分類となりうる。
新判定を通すための再昇格・履歴書換えは行わない。

cadence statusは`current_artifact_stamp`と`accepted_artifact_stamp`を本文checkpointと別に返す。
台帳・ref・aliasの変更、昇格、rollbackで`after_write`がdueとなる。
受入成功かつ監査前後のstamp一致時だけbaselineを進め、旧stateの未記録stampは一度監査する。
これは構造の受入であり、本文の意味判断が済んだことを表さない。

旧実体とLFSは保持し、削除条件・rollbackの境界を変更しない。物理数の減少を完了条件にしない。
以前のノート本文に残る移行前の散文の意味検出・自動書換えは今回の対象外。
