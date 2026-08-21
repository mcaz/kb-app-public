# ADR-0014: 完了initiativeを専用のsnapshot固定waveで閉じる

- Status: Accepted
- Date: 2026-08-21
- Issue: #91

## Context

semantic executor v1は既存ノートの本文・description・tags・relationsを安全に蒸留できる一方、
authority envelopeの変更を意図的に拒否する。これは一般的なidentity変更やsupersedeを混ぜないための
正しい境界だが、実作業を終えたinitiativeを`active`のまま残すか、直接`update`を連続実行するしかなかった。
後者は複数ノートを同じsnapshotとrollback境界へ固定できない。

## Decision

MCPへ次の専用3操作を追加する。

- `plan_initiative_closure`: AI管理のactive canonical initiativeだけを対象に、note ID、note UID、
  input hash、全DB snapshot、`active → historical`、一行理由を決定的planへ固定する
- `apply_initiative_closure`: planを同じwrite transaction内で再構成して全対象を照合した後、
  authority statusだけをhistoricalへ変更する
- `rollback_initiative_closure`: apply直後の全snapshotと全対象documentが不変の場合だけ、全対象を
  実行前documentへ戻す

実行履歴は既存の`distillation_runs`へ保存し、ノート更新・索引・Markdown outbox・履歴記録は
既存の`queue_put`経路を共有する。DB schemaは増やさない。

## Safety boundary

- namespaceは`initiatives`、roleは`canonical`、statusは`active`、originは`agent`に限定する
- apply後はstatusとgenerated来歴以外を変えない。title、body、description、tags、relations、
  namespace、role、scope、note UIDは不変にする
- stale plan、対象差替え、note UID・status・理由の改ざん、二重applyをwrite前に拒否する
- 複数対象は1 transactionで全件成功または0件にする
- rollbackも全snapshotを照合し、後続の無関係な変更があっても拒否する

## Consequences

完了initiativeを監査可能な小規模waveとして閉じられる。一般のauthority変更、supersede、merge、
namespace・scope変更は引き続き対象外であり、専用transactionなしに能力を拡張しない。
