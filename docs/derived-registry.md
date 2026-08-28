# 派生索引registryと自己修復の計測

- 実施日: 2026-08-28
- 対象: 派生索引registry(S-2)+ open時自己修復(S-3)+ embed stamp複合化(S-4)+ migration state machine(S-1)。branch `claude/derived-registry`
- 比較対象: `origin/main`(91b9bf2)。同一端末(Darwin arm64)・同一fixture・serial実行

## 変更

派生索引6種(fts_main / fts_tri / links / fts_anchor / note_relations / note_vecs)を
静的registry(`derived_index.rs`)へ宣言し、増分維持(ノート書込)と一括rebuild(修復)を
同じ `apply_change` / `rebuild` に統一した。open時に安価なhealth check(object存在・型、
fts_main・fts_triのnote IDカバレッジ両方向、governanceのauthority整合検証)を行い、壊れた
artifactはartifact単位transactionでDROP→CREATE→再導出する。durable table 6種
(notes / meta / note_exports / distillation_runs / action_receipts / action_capability_uses)は
enumに存在せず型上登録不能で、欠落はfail-closed(open失敗)。governance台帳
(note_relations)の修復・検証失敗中はnote writeだけをfail-closedにし、read / searchは
fallback+劣化表示で継続する。embed stampは `{producer}|sha256:{input_hash}` 複合になり、
title / description / bodyいずれの変更でも同一transactionで埋め込み行を無効化する。

**変えていないこと**: 検索アルゴリズム・ランキング・retrieval上限・SQL問い合わせは一切
変更していない。schema versionは8のまま(bumpなし)、durable tableのDDLも不変。派生objectの
DDLはregistryの `create_sql` を正本としてfresh作成と修復が共用するが、生成されるSQLite構造は
baselineと同一。挙動中立の証明は下の official / holdout 突合。

計測に伴う設計判断が2つある。

1. **ChangeCache**: registry分割で fts_main / fts_tri が検索テキスト(添付名のFS列挙込み)を
   各自計算すると、更新1回あたりの主要な加算コストになった。1回の `apply_note_change` 内で
   検索テキストとリンク抽出をlazyに共有する `ChangeCache` を導入した。各artifactが個別に
   計算した場合と同じ値を返し、rebuild同値性テストが意味の不変を固定する。
2. **governance検証はopen単位**: `require_governance_ready`(note write先頭のfail-closed
   ゲート)が毎write `validate_authority_index`(3 join query)を実行すると、10k notesで
   1 writeあたり約2.1 ms(gov_x100実測208 ms)を加算し、単一note更新medianの
   baseline比+15%予算を超えた。spec S-3の契約は「毎openで再検査・永続通知状態なし」
   なので、open時のhealth check / 修復+validateを通過した接続にTEMP tableのmarker
   (接続ローカル、DBファイルに何も残らない)を置き、write時のfull再検証はmarkerの
   無い接続(生Connection・修復失敗後)に限定した。object存在・型の検査は毎write残る
   (open後のDROPをその場で止める)。write自体の整合は従来どおり upsert 後の
   `validate_authority_write` が同transactionで検査する。この契約は
   `governance_content_revalidation_happens_at_open_not_per_write` テストが両側
   (生接続はfail-closedのまま / open済み接続は素通り)を固定する。

## 実測

### 検索品質(挙動中立の証明)

official(`retrieval-google-benchmark`)/ holdout(`retrieval-realistic-holdout`)の
計69面を baseline と変更後で実行し、`scripts/compare_retrieval_reports.py` で時間値
(`*_elapsed_us`)以外の全case metrics(candidate_ids / selected / recall / precision /
token / spill / gate判定 / degradation / summaries)を突合した。

| suite | 比較範囲 | 結果 |
| --- | --- | ---: |
| official 33面(control 15 + challenge 18) | 全strategy(top3 / linked_v1)全case | 完全一致 |
| holdout 36面(control 15 + challenge 21) | 全strategy(top3 / linked_v1)全case | 完全一致 |

linked_v1のmacro値(baseline実測=変更後実測): official control 100% / 73.3% / 772tok、
official challenge 100% / 37.3% / 1558tok、holdout control 100% / 55.7% / 1059tok、
holdout challenge 100% / 29.8% / 1792tok。**Aは検索挙動へ中立**である。

### 10k性能gate(release・serial実行)

baselineは同一端末で同じ計測コードを一時パッチとして当てた serial 実行(clean 2回+
負荷下2回)、変更後は本commitのgateの clean 3回連続pass(run2〜4)。負荷は時間へ
一方向にしか乗らないため、各指標は **clean runの最小値** を代表値とし、幅は散文へ残す。

| 指標 | baseline | 変更後 | 差分 | CI予算 |
| --- | ---: | ---: | ---: | ---: |
| DB初回復元 | 15,918 ms | 16,145 ms | +1.4% | 30,000 ms |
| main+rescue全文検索 100回 | 12 ms | 12 ms | ±0 | 500 ms |
| 1024次元KNN 10回 | 222 ms | 232 ms | +10 ms | 1,000 ms |
| warm open 5回(health check込み) | 16 ms | 110 ms | +94 ms | 2,000 ms |
| 単一note更新 median | 5,184 us | 5,444 us | **+5.0%** | 25 ms |
| 単一note更新 p95 | 5,789 us | 6,150 us | **+6.2%** | 50 ms |
| 全artifact一括rebuild(6件) | — | 614 ms | — | 10,000 ms |

単一note更新は clean runの幅でも baseline 5,184〜5,521 us(median)/ 5,789〜6,227 us
(p95)に対し、変更後 5,444〜5,805 us / 6,150〜6,482 us で、**受入基準
(median +15% / p95 +20% 以内)を満たす**。warm openの+94 msはopen毎のhealth check
(object存在・型 約15 object、fts_main・fts_tri×両方向のIDカバレッジ4 query、
governanceのauthority整合検証)で、1 openあたり約19 ms。全文再parse監査を行わない
「安価な検査のみ」の設計どおり、read系予算の枠内に収まる。DB初回復元の+1.4%は
registry走査への統一(関数ポインタ経由・NoteChange構築)の間接費で、fixture再構築
20秒級の揺らぎより小さい。なお負荷下では既存のDB初回復元30秒予算をbaseline・変更後の
両方が超過することがある(baseline 30.3〜36.4秒 / 変更後 33.3秒を記録)。これは
本変更と独立の既知の揺らぎで、効果判定はserial clean runで行った。

governance write gateを毎write full検証で実装した中間版は、単一note更新へ
1 writeあたり約2.1 msを加算し(gov_x100実測208 ms)、median 7,829 us(+51%)と
予算を超過した。上記「変更」2.のopen単位検証へ変更した結果が本表である。

### gate絶対値予算の根拠

受入基準(単一note更新 median +15% / p95 +20%、対baseline比)は同一端末のside-by-side
実測で判定した(上表)。gateへ入れる値は端末・runner間で比較できないため、既存gateの
流儀(桁違いの退行を検知する絶対値)に合わせて次のように置いた。

- **単一note更新 median 25 ms / p95 50 ms**: baseline実測(clean時 median 5.2〜5.5 ms、
  p95 5.8〜6.2 ms。負荷下はp95が16.8 msまで揺れる)へ+15% / +20%を適用した相当値に、
  共有runnerの揺らぎ(実測で約2.5倍)を乗せた幅。clean実測値の約4〜8倍で、
  問い合わせ追加・O(n)化のような一桁悪化を確実に止める。毎write full検証の中間版
  (median 7.8 ms)はこの絶対値予算では止まらない — 相対判定(+15%)は同一端末の
  side-by-side実測が担う、という役割分担である。
- **warm open 5回 2,000 ms**: 他のread系5〜20回予算と同じ幅。health checkが全文再parse
  監査のような重い検査へ退行したら超過する(実測の15倍超)。
- **全artifact一括rebuild 10,000 ms**: 実測の約10倍。DB初回復元(30秒)より軽い操作で
  あることを固定する。

## 受入テスト(実事故の自己修復)

このstageの受入テストは合成ベンチだけでなく、**過去に実際へ起きた索引事故の再現**である
(spec S-5)。5種を `derived_index.rs` のテストが固定する。

1. fts_main+fts_tri欠落 → open時修復・検索復活・2回目openは無通知
2. fts_anchor単独欠落 → 修復
3. note_relations欠落 → document再構築+validate、失敗時はnote writeのみfail-closed
4. durable table欠落 → open失敗(空表で隠さない)
5. rebuild途中失敗 → 該当artifactのみrollback、durable論理行は1 bitも不変

## 残課題

- **cold rebuild予算の負荷感度**: DB初回復元の30秒予算は端末負荷下でbaseline・変更後の
  両方が超過し得る(実測で最大36秒)。本変更の退行ではないが、CIでのflake要因として
  残る。超過時はserial再実行で判定する運用(performance-gate.md)を維持する。
- **embed stampの二段階rollout**: 旧形式stampは全pendingになり背景で再埋め込みされる。
  旧バイナリと新バイナリの併存は不可(旧側が旧形式で書き戻す)— コード内コメントに明記済み。
- **conflict復旧・蒸留read-only経路は修復なし**: `open_db_recovery` と蒸留の限定復旧接続は
  意図的にhealth check / repairを通さない(限定復旧の性質を維持)。
- **governance full検証の細分化**: 現在はopen単位のfull検証。将来10^5規模で
  `validate_authority_index` 自体が重くなる場合は、差分validate(書込note近傍のみ)へ
  細分化する余地を残してある。
