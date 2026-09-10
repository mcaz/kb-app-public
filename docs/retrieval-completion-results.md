# 完了・過去結果の検索

- 対象: [Issue #93](https://github.com/mcaz/kb-app/issues/93)
- 契約: [contract.md](contract.md) 契約9、[ADR-0009](adr/0009-canonical-authority.md) §5
- 記録日: 2026-09-08

## 問題と変更範囲

完了確認や過去の結果を尋ねたとき、現行の一般方針が実行結果より上位になると、保存済みの結果を
見落とす。検索語の一致だけでなく、質問が求める対象と結果の根拠を一緒に評価する。

検索の候補抽出と各経路の融合後に同じ順位方針を使う。短い表示用snippetに結果語が見えるかだけで
判定せず、取得した本文に基づく根拠を使う。先に件数で落ちた候補は最終順位の変更では戻らないため、
必要な結果記録が候補に残ることから検証する。

authority envelope、SQLite schema、保存済みノートは変更しない。ContextCardのrerank、リンク展開、
本文の予算選択を別の方式へ置き換える変更も含めない。

## 順位の判断

1. 完全タイトル一致は従来どおり最優先にする。
2. 完了確認・過去結果の質問では、title・description・scopeに一致する対象語の数を先に比較し、
   同程度の対象一致では、本文に結果の根拠があるactive/historical recordまたはinitiative canonicalを優先する。
   一般方針や結果のない計画を、過去のノートという理由だけでは昇格しない。
3. 結果には完了・成功だけでなく未完了・失敗・未確認を含める。検索順位は結果の所在を示すもので、
   作業の完了や成功を確定するものではない。
4. 完了条件・分類方法の質問を結果確認と混同しない。現行方針への明示質問と意図なしqueryの
   active canonical優先を維持する。過去結果が上位でも、そのauthorityは変更しない。
5. 候補抽出時と融合後に同じ根拠を使い、main・anchor・semantic・rescueの経路によって
   順位方針を変えない。既存の多様化と通常参照可否の境界を維持する。

日英の明示的な質問表現から質問語を除き、対象語で主経路の候補を取る。本文の根拠は語彙による
検索信号であり、意味の完全な判定ではない。結果名詞だけ、疑問文、予定・仮定だけの文を加点せず、
「完了した。次の監査は予定」のように実績と次の予定が共存する本文は文ごとに評価する。
未知の言い回し、対象名の別表記、対象が本文にしかない記録は、この信号だけで拾えるとは限らない。

完了照会の主経路・anchor・rescueには既存の候補倍率8倍を使い、semanticはその件数を入力して
既存のKNN過剰取得を維持する。全ノートの本文を走査せず、経路の候補だけをDBから確認し、
融合後に多様化と最終件数制限を適用する。追加の本文取得が失敗した場合は従来順位へ戻し、劣化を返す。

## 再現と対照

公開リポジトリには人工の対象名・本文だけで再現fixtureを作る。私有KBの本文やノート識別子を
fixtureへコピーせず、実MCPでの再確認と合成fixtureの検査を分ける。

| ケース | 確認する挙動 |
| --- | --- |
| 対象作業が完了したかという質問 | 対象の結果記録が一般方針より上位になり、本文を取得できる |
| 過去に不明だった対象の調査結果を尋ねる質問 | 判明した名称などの結果を持つ対象記録を優先する |
| 未完了・失敗・未確認の結果 | 成功と読み替えず、質問に対応する結果記録として取得する |
| 別対象の完了報告 | 完了語だけで目的の対象より上位にしない |
| 結果を書いていない計画・完了条件 | 結果の根拠として扱わない |
| 分類方法・現行方針への質問 | 対応する説明・現行正本を維持する |
| 完全タイトル一致・意図なしquery | 従来のlocator優先・authority既定順を維持する |

受入では候補の順位だけでなく、selected本文の先頭側に対象記録が残ることと、本文・出力予算を
守ることを確認する。Codex・Claude Code・ChatGPTの3 surfaceを同じfixtureで比較し、既存controlの
結果も確認する。surface別の合成評価を、各実クライアントでの受信確認と同一視しない。

## 測定状況

基準は`7914341`（GitHub main `e640c899`と同一tree）、変更版は本書を追加したcommit。
同じ再現fixtureを先に基準の検索へ加え、Lumenの結果記録が5位となる失敗を確認してから修正した。

| 検証 | 状況 |
| --- | --- |
| 合成fixtureの変更前・変更後比較 | Lumenの完了質問で結果記録が5位→1位。Vegaの分類結果も変更後1位 |
| 現行方針の対照 | 受付方針、結果語を含む分類方法の両質問で期待するcanonicalが1位 |
| 候補・selected本文・予算の3 surface確認 | 4ケース×3 surface×3反復。top3/linked_v1の両方で対象本文が先頭、順序安定。劣化・本文欠損・予算超過・spillなし |
| 副経路と本文取得 | anchor/rescue単独の候補を最終1件でも保持。RRFへ与えた合成KNN候補も本文で再順位。本文取得失敗は劣化を表示 |
| 結果根拠・対象の対照 | 未完了/失敗/未確認、別対象、metadataだけの結果、予定・条件・疑問、予定どおり完了した文を検査 |
| 10k release性能gate | 完了照会100回132ms（予算500ms）、keyword100回12ms、linked context20回8ms。既存の全性能予算もPASS |
| アプリ反映後の実MCPによる再現質問 | 未実行 |

再現テストは`crates/kb-core/src/retrieval_eval.rs`の
`completion_and_result_evidence_reaches_early_bodies_on_all_surfaces`、副経路は
`crates/kb-core/src/search.rs`の`completion_results_from_secondary_routes_use_full_body_before_limit`。
語彙の対照は`crates/kb-core/src/search/result_query.rs`のunit testに置く。

```bash
cargo test -p kb-core result -- --nocapture
cargo test --release -p kb-core ten_thousand_note_performance_gate -- --ignored --nocapture
```

実クライアントでの受信、意味検索モデルの推論品質、私有KB全体の改善率はこの合成試験から主張しない。
2026-08-23の[query intent実測](retrieval-query-intent.md)は今回の結果へ流用しない。

性能fixtureは10k件のうち40件を39計画＋1結果記録とし、件数5・劣化なし・結果1位を検査した。
`semantic=false`でモデル導入状況から独立させているため、132msにはモデル推論の時間を含めない。
単発のローカルrelease測定であり、分位点や実クライアントの応答時間ではない。

引き渡し時の全体検査はfrontend 255件、Rust 957件、fmt、clippy、契約co-change 6件がPASS。
Rustの既存private gate通信テスト4件は、sandbox内と許可付き再実行の両方でローカルポートの
`Operation not permitted`となった。4件以外のworkspace検査は最後まで実行したが、
全workspace成功とは扱わない。アプリ反映前に同じ版のテスト実行ファイルで4件を再確認する。
