# ADR-0013: 対象指定のsemantic変更もsnapshot固定planを必須にする

- Status: Accepted
- Date: 2026-08-21

## 文脈

`mechanical-v1` plannerはauthorityとtyped relationから証明できる候補だけを列挙する。この制限により
決定的で安全なplanになる一方、全文監査で見つかる欠落参照の自己完結化や古い説明の修正は、対象が
`keep`のままになる。executorで`keep -> revise`を無条件に許すとclosed-world契約を失い、通常`update`へ
迂回するとplan/apply/rollbackを必須にする監査方針を満たせない。

## 決定

`plan_targeted_distillation`（CLIは`kb distill plan-targeted --input`）を追加する。呼び出し側は全文確認済みの
既存note ID、`normalize` / `revise` / `extract`、一行理由を渡す。plannerは次をread-only planへ固定する。

- 全DB documentのsnapshot digestとnote count
- 対象documentのinput hash
- requested operationと理由
- `targeted-v1` planner profile

planは要求対象だけをentriesに含む。`revise`はactive canonical、`extract`はrecord、全操作はauthority・note_uidを
持つ`origin: agent`だけに限定する。重複対象、空・複数行理由、存在しないnote、role不一致はplan段階で拒否する。

`apply_distillation`は`targeted-v1` requestを受けた場合、execution内のnote・operation・理由から同じtargeted planを
write transaction内で再構成し、plan IDとsnapshotを照合する。target本文は従来どおりexecutorのoperation別制約、
タグ、relation、authority一意性を通す。理由やoperationをapply時に変えるとplan IDが一致せず、書込前に停止する。

## 非目標

- executorで任意の`keep -> revise`を許すこと
- record本文、authority、identity、originの変更
- create、delete、merge、supersede、split、path移動
- targeted planを承認tokenや秘密値として扱うこと

## 帰結

機械signalに現れない小規模semantic修正も、snapshot固定・atomic apply・直後rollbackの同じ回復境界で扱える。
通常planの決定性とoperation一致契約は維持される。
