# 統一retrieval評価protocol(C-5 / R4 I-7)

- 確定日: 2026-08-28(R4討議 I-7 amendの反映)
- 位置づけ: 両実験系譜(Claude主体 `claude/*` / GPT主体 `mcaz/*`)の意味レーン
  (ContextCard・fts_entry)を、**統合core上で同じ評価契約で比較する**ための正本。
  統合coreそのものは検索意味を変えない(docs/retrieval-integration-core.md 予定)。
  意味レーンの実体は評価用ブランチに置き、通常runtime・MCP・hook・通常CLIからは
  選択不能とする(R4 I-5)
- 関連: docs/retrieval-experiment-base.md(共通base B0)/
  docs/retrieval-profiles.md(profile分離)/
  docs/derived-artifact-lifecycle.md(artifact framework C-4)/
  docs/retrieval-evaluation.md(評価器)

## 1. primary comparison — 4 arm

primary比較は次の4 armに固定する。

1. **baseline** — 統合coreの現行検索(profile: `session_auto` 相当の既定)
2. **ContextCard only** — note_context + link_anchors によるrerank層のみ有効
3. **entry only** — fts_entry(retrieval entries)によるcandidate生成のみ有効
4. **ContextCard + entry** — 両方有効

- **profileとroutine rerankは診断軸として別表に分離する。** 全profile×全rerank×
  全search variantのCartesian productを主結果へ混ぜない(母数と採用gateが読めなく
  なるため)。診断表はprimary 4 armと同じsnapshot・query setで別掲する
- ContextCardは「candidate生成不能」を理由に不採用としない — rerank層として独立に
  評価する。ただし現状G2 FAILのため、default採用の根拠は存在しない(R4 I-7)
- routine rerank_on既定化は評価対象外(−1.12ptの回帰が再現済み — R4 §4)

## 2. 事前凍結リスト

比較の実行**前**に、以下をすべて凍結しdigestを記録する。凍結後の変更は
「意図的rebaseline」としてのみ許し、理由と差分を残す。

| 項目 | 内容 |
|---|---|
| snapshot digest | 評価対象DB(隔離複製)のdurable note stateのlogical digest |
| query set | 全query文字列と件数。追加・削除・改変は禁止 |
| required / relevant / excluded | query毎の判定集合(excludedは「出てはいけない」ノート) |
| seed_required | seed段階で必須のノート(suite versionで許否を管理) |
| query family | 各queryの家族分類(集計単位) |
| surface | 評価面(official / holdout / entries等) |
| body requirement | 本文提示の要求水準 |
| expected polarity | 肯定・否定・撤回・引用の期待極性 |
| evaluator / artifact format | 評価器のversionと各armが読むartifact format key(C-4) |
| 採用gate | §5の一覧と閾値。事後の閾値調整は禁止 |

fixture・generator側は共通base(B0)の凍結装置を使う: generator source digest・
展開済みJSON digest・suite schema version・fixture digest・各armの構造出力digest・
rebaseline理由(docs/retrieval-experiment-base.md §3)。

## 3. 自然なbaseline failureの原則

- 実KB評価のquery setには、**baselineが自然に失敗するquery**(現行検索で
  required候補を取り逃す実例)を10件集めることを目標とする
- **10件集められない場合、人工的なfailure caseで件数を補完しない。** その場合は
  「実KBで意味レーンの必要性を証明できなかった」という結果として記録し、
  採用判断の根拠にこの不足自体を使う
- failure caseの発見手順(検索ログ・会話からの採取)はqueryの凍結前に完了する。
  凍結後の「都合のよい追加」を禁止する

## 4. entry laneのprimary昇格前提

fts_entry armは、次の**すべて**を満たすまでprimary比較へ入れない
(それまでは診断armとして別掲):

1. 否定・撤回・引用のcontrol suiteでexcluded増加0(既知blocker: excluded 0→3)
2. polarity guardによるpositive recallの回帰0
3. producer stamp / generation dirty barrier(C-4のframework)が実装済みで、
   旧バイナリのwriteでもdirty化することがテストで固定されている
4. cap打ち切り(entry数上限到達)時に、silent truncationせずbaselineへfallbackし
   degradationを明示する

## 5. 採用gate一覧

各armは以下のgateを**全部**通過した場合のみ採用候補になる。1つでも落ちたら
「その軸の改善では採用しない」であって「gateを緩める」ではない。

| gate | 基準 |
|---|---|
| G0構造一致 | §7の二種(core neutrality / experiment stability) |
| candidate / selected recall | required候補の取りこぼし増加0(challenge family含む) |
| excluded増加0 | 否定・撤回・引用controlでexcludedノートの露出が増えない |
| answer-level blind評価 | arm名を伏せた回答品質比較でbaseline劣後なし |
| token | avg / p95 tokenの変化を報告(改善は複数query familyで再現すること) |
| query p95 | 検索latencyのp95が予算内(docs/performance-gate.md) |
| rebuild / update cost | artifact rebuildと単一note更新が性能gate予算内 |
| degradation / stability | 新規degradationの常在なし・fallback経路の明示 |
| routine rerank | 有効化する場合、baseline比のranking低下0 |

profile既定の変更(session_explicit化)はこのprotocolの対象外で、別PRの4条件
(docs/retrieval-profiles.md 冒頭)に従う。

## 6. 実KB評価の隔離DB手順

実vaultへ直接派生表を追加しない。評価は必ず隔離DB複製で行う:

1. **実DB snapshot digestの固定** — 停止状態の実DBからdurable note stateの
   logical digestを取り、記録する
2. **隔離DBへの複製** — durable note state(notes / meta必須鍵 / note_exports等の
   正本)だけを隔離評価DBへ複製する。派生objectは複製先で各armが自分のformatで
   構築する
3. **4 armを同一snapshotで実行** — baseline / ContextCard only / entry only /
   combined を同じ隔離DB snapshotに対して走らせる(順序による汚染を避けるため、
   armごとに複製から再出発してもよい。その場合も元digestは同一であること)
4. **元DBのdigest不変確認** — 評価完了後、実DBのdurable / derived objectの
   digestが手順1の記録と一致することを確認する。1 bitでも動いていたら評価は無効

## 7. G0の二種

「G0」は次の二種を区別して運用する。

### 7.1 Core neutrality G0(baselineの構造完全一致)

統合coreの適用前後で、baseline armの出力が**構造完全一致**すること。対象は
既存69面(official / holdout)とprofile-context suite、および将来のentries suiteの
baseline arm。一致させる項目:

- ranked hit IDsと順序
- seed IDs / candidate IDs / selected IDs
- token
- degradation
- body requirements
- omitted reason

検出器: `frozen_baseline_reports_match_the_current_control_suites` /
`session_auto_profile_matches_linked_v1_structure_on_control_suites`(+
`scripts/check_report_neutrality.py`)。

### 7.2 Experiment stability G0(各arm出力の凍結)

treatment arm(ContextCard / entry / combined)はbaselineと一致させる必要はない —
変えるためのarmである。代わりに**現在の各arm出力を凍結**し、無関係な変更で
動かないことを保証する。凍結対象:

- 各armのseed / candidate / selected順
- recall / precision / excluded
- token
- low-signal fallbackの発動
- **既知の失敗(negation failure等)も削除せず凍結する** — 修正は「改善のための
  意図的rebaseline」としてだけ行い、差分と理由を残す。失敗fixtureを消して
  G0を通すことはできない
- degradation

凍結手順は各suiteの統合前に定義し、最初のdigestを基準点にする。過去に凍結手順が
なかったことは、今から凍結する妨げにならない(R4 §3)。

## 8. 判断の出口

- 4 armの比較結果はここに列挙したgateの通過表とともに
  `$SCRATCH/measurements/`系の計測記録と評価用ブランチのdocへ残す
- **意味レーンの採用(production ON)は統合core PRに同梱しない。** baseline中立を
  主張するcore PRと、意味を変える採用PRを分離する(R4 I-8)。採用判断は
  実KB 4 arm比較+全gate通過の後、別PRで行う
