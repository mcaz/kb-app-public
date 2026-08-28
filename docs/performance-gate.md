# 10k synthetic fixture 性能回帰gate

- 制定: 2026-08-16
- 対象要件: [requirements.md](requirements.md) NFR-3

## 目的

「10³〜10⁴ノートで検索が体感即時」を文章だけの目標にせず、代表的な読み取り経路と
MarkdownバックアップからのDB初回復元の桁違いの退行をCIで止める。この予算は端末ごとのSLAではなく、同じfixtureに対する
回帰gateである。1回のrunner揺らぎを追うために閾値を緩めず、変更時は複数回の実測を残す。

## 固定fixture

`search::tests::ten_thousand_note_performance_gate` が実行時に一時vaultを生成する。
実データや利用者のvaultは読まない。

- 10,000ノート、100カテゴリ、各カテゴリ100ノート
- 全ノートに固定時刻と2タグ、先頭以外には前のノートへのリンクを付与
- 1ノートだけに全文検索の番兵語を入れる
- 10,000件の1024次元単位ベクトルを派生索引へ投入
- fixture書き込み口は `#[cfg(test)]` のため製品buildには存在しない

semantic経路は、モデル配布状態やrunner CPUの差が大きいquery embeddingを含めず、
10kベクトルからのKNN retrievalを測る。モデル推論の配布受入は別の実機試験とする。

## 時間予算

専用testはrelease build・single test threadで実行する。繰り返し回数を含む値が予算であり、
単発の時間ではない。

| 指標                     |                    workload |    CI予算 |
| ------------------------ | --------------------------: | --------: |
| DB初回復元               | 10,000 Markdownをcold DBへ復元 | 30,000 ms |
| カテゴリ一覧             |                        20回 |    500 ms |
| 1カテゴリの50件page      |                         5回 |  2,000 ms |
| main + rescue全文検索    |                       100回 |    500 ms |
| 1024次元KNN              |                        10回 |  1,000 ms |
| 本文 + related + similar |                         5回 |  2,000 ms |
| 5 seed・2ホップ連鎖取得  |                        20回 |  2,000 ms |
| warm open(health check込み) |                     5回 |  2,000 ms |
| 単一note更新 median      |          100回の中央値(1回分) |     25 ms |
| 単一note更新 p95         |            100回のp95(1回分) |     50 ms |
| 全artifact一括rebuild    |        force_rebuild 6件合計 | 10,000 ms |

初回実装時のDarwin arm64 / rustc 1.97.1 release実測は、索引再構築15,243 ms、
カテゴリ20回82 ms、一覧5回443 ms、全文検索100回11 ms、KNN 10回263 ms、
詳細5回131 msだった。予算は共有runnerの揺らぎを吸収しつつ、アルゴリズムや問い合わせが
一桁悪化する変更を検知する幅に置いた。

連鎖取得は検索結果の上位5件から、出リンク最大2ホップ、被リンクを低優先度で最大50候補まで
展開し、推定10,000 token以内・最大10本文を選ぶ経路を測る。候補数と本文量は別上限で固定し、
グラフ密度が上がっても全ノート走査や全文投入へ退行しないことを通常テストでも検査する。

2026-08-18のDB正本化後も同じ端末で、DB初回復元15,689 ms、カテゴリ20回102 ms、
一覧5回452 ms、全文検索100回11 ms、KNN 10回261 ms、DB本文を使う詳細5回132 msだった。
リンク連鎖取得の初期実装後は、同じ10k fixtureでDB初回復元15,503 ms、連鎖取得20回2 ms
（ほかはカテゴリ104 ms、一覧466 ms、全文検索10 ms、KNN 264 ms、詳細131 ms）だった。

2026-08-28のretrieval実験base([retrieval-experiment-base.md](retrieval-experiment-base.md)、
索引・検索経路は不変)では、同じ端末でclean な`origin/main` 91b9bf2を無負荷時に3回測り、DB初回復元15,693 ms / 15,566 ms / 15,756 ms
(中央値15,693 ms)を実験の正典baselineとした。同じ端末でも他sessionの並列buildが走るload average 27〜30の
状態では20,641 ms / 59,450 ms / 34,004 msと予算超過を含む揺らぎが出たため、gateの判定と実験の比較は無負荷時の
3回中央値だけで行い、負荷下の値は汚染として記録に留める。

初回測定では索引再構築が20,348 msだった。1ノートごとのSQLite autocommitを単一transactionへ
変更すると15,105 msになったため、このtransactionは速度だけでなく途中失敗時のatomicityも
通常の回帰テストで固定する。

2026-08-28の派生索引registry導入時に、write経路とopen時自己修復の退行を止める4指標を
追加した。warm openは構築済みDBの再open(open毎のhealth check込み)5回、単一note更新は
本番write経路(governanceゲート→registry走査→埋め込み無効化→outbox→commit)100回の
median / p95(この2つだけ1回分の値が予算)、全artifact一括rebuildは自己修復と同じ
`force_rebuild` 6件の合計。同端末のserial 3回実測はwarm open 110〜117 ms、
更新median 5,444〜5,805 us、p95 6,150〜6,482 us、一括rebuild 614〜704 msで、
絶対値予算の根拠(baseline比+15% / +20%規則と揺らぎの吸収幅)は
[derived-registry.md](derived-registry.md) に残した。負荷下ではDB初回復元30秒予算を
baseline・変更後の両方が超え得ることも同日に記録している(serial再実行で判定する)。

## 実行

```bash
cargo test --release -p kb-core \
  search::tests::ten_thousand_note_performance_gate -- \
  --ignored --exact --nocapture --test-threads=1
```

通常の `cargo test --workspace` では重いfixtureを実行せず、GitHub Actionsの独立jobで必ず実行する。
閾値を変更する場合は同じrelease commandを3回以上実行し、CI runnerの結果と変更理由をPRへ残す。
