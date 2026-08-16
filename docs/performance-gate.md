# 10k synthetic fixture 性能回帰gate

- 制定: 2026-08-16
- 対象要件: [requirements.md](requirements.md) NFR-3

## 目的

「10³〜10⁴ノートで検索が体感即時」を文章だけの目標にせず、代表的な読み取り経路と
索引再構築の桁違いの退行をCIで止める。この予算は端末ごとのSLAではなく、同じfixtureに対する
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
| 索引再構築               | 10,000ノートをcold DBへsync | 30,000 ms |
| カテゴリ一覧             |                        20回 |    500 ms |
| 1カテゴリの50件page      |                         5回 |  2,000 ms |
| main + rescue全文検索    |                       100回 |    500 ms |
| 1024次元KNN              |                        10回 |  1,000 ms |
| 本文 + related + similar |                         5回 |  2,000 ms |

初回実装時のDarwin arm64 / rustc 1.97.1 release実測は、索引再構築15,243 ms、
カテゴリ20回82 ms、一覧5回443 ms、全文検索100回11 ms、KNN 10回263 ms、
詳細5回131 msだった。予算は共有runnerの揺らぎを吸収しつつ、アルゴリズムや問い合わせが
一桁悪化する変更を検知する幅に置いた。

初回測定では索引再構築が20,348 msだった。1ノートごとのSQLite autocommitを単一transactionへ
変更すると15,105 msになったため、このtransactionは速度だけでなく途中失敗時のatomicityも
通常の回帰テストで固定する。

## 実行

```bash
cargo test --release -p kb-core \
  search::tests::ten_thousand_note_performance_gate -- \
  --ignored --exact --nocapture --test-threads=1
```

通常の `cargo test --workspace` では重いfixtureを実行せず、GitHub Actionsの独立jobで必ず実行する。
閾値を変更する場合は同じrelease commandを3回以上実行し、CI runnerの結果と変更理由をPRへ残す。
