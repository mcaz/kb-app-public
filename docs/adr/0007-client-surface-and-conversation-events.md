# ADR-0007: client surfaceを分離し、必須表示をconversation eventで返す

- 日付: 2026-08-19
- 状態: 採用
- 関連: [contract.md](../contract.md) 契約1・4・8 / [Rule Delivery評価](../rule-delivery-evaluation.md)

## 背景

Claude Code / Codex CLIでRule Delivery Matrixを各80回実測したところ、Always + Topic + Eventが
最良だったが、Claude Codeは20件中18件、Codex CLIは20件中12件に留まった。特にnote link、
degradation、語彙外タグ、引数なし`get`は、instructionsやtool応答をモデルが理解して最終文へ
再掲する設計では安定しなかった。

また従来のclient判定はactor全体に`claude` / `gpt`が含まれるかを見るため、Claude Desktopを
Claude Code用managed sandboxへ、ChatGPTをCodex用policyへ誤分類していた。同じmodel familyでも
通常チャットとcoding agentでは、shell / file能力、hook、現在ノート文脈が異なる。

## 決定

- client actorの先頭segmentを`ClientSurface`へ厳密変換する。Claude Code、Codex CLI、
  Claude Desktop、ChatGPT、評価harness、unknownを別surfaceとして扱い、model名は判定に使わない。
- Codex CLI / Claude Codeは生ファイル能力を持つため、それぞれ対応する管理OS sandboxが有効な場合だけ
  MCPをONにする。Claude Desktop / ChatGPT通常チャット面は、kb-app MCPが生path能力を公開しない
  broker境界として扱う。unknownはfail-closedとする。
- initializeの`capabilities.experimental.kbApp`へsurface、raw-vault境界、検索開始方式、
  current-note可否、conversation event versionを返す。
- `get.note`はClaude Desktopだけ省略可能にし、Codex CLI / Claude Code / ChatGPTではschema上必須にする。
- AI用MCPの`propose` / `update`から`allow_new_tags`を除き、直接渡されても書込前に拒否する。
  これは未知語を通常tagsへ直接指定するoverrideの制限である。語彙ノート本文の更新は禁止しない。
  2026-09-08、実装と案内の不一致を修正。同日の本人指定で語彙の追加・統合・削除もAI判断に任せ、
  個別の本人確認を必須にしない。既存語を優先し、本人の明示訂正に従う。
  `tag_vocabulary`でworkspace ID・正本状態・候補を確認する。候補が1件でも自動採用せず、
  getで確認したnote UIDをwrite面の`set_tag_vocabulary_source`で明示指定する。
  初回は`expected_revision:null`、切替は現在の`source.revision`を必須とし、別workspaceや
  古いrevisionを拒否する。欠損・参照不可では別ノートや現用タグへ戻さない。
  指定結果を再取得し、第3段階ではmaintenance面の`plan_tag_vocabulary_change`で追加・説明変更・
  削除・統合/改名を計画し、write面の`apply_tag_vocabulary_change`へreceiptと実行IDを渡す。
  計画は原文を返さず最大20例・影響件数・blocker集計を示し、適用時の再照合後に語彙と利用ノートを
  一括確定する。新語の追加だけなら登録を再確認して通常tagsとして使う（[手順](../tag-vocabulary.md)）。
  通常updateで運用本文を修復できるが、使用中語の削除は正本切替・importを含む共通guardで拒否する。
  正本指定はノートの通常update集計へ混ぜず、保存後に書き出しが失敗しても
  `stored:true, export_pending:true`で保存済みと伝える。
  語彙一括適用も通常updateの件数へ水増しせず、`tag_vocabulary_changed`の必須eventで
  実行ID・変更件数・保存結果を返す。出力失敗でも`stored:true`を維持し、`pending_exports`を返す。
  待ち件数を確認できない場合は`null`と警告を返す。保存済みの操作を新しい実行IDで再送しない。
  read面の`list_tag_vocabulary_changes` / `get_tag_vocabulary_change`は要約とタグ/hashを
  既定20・最大100件でページ化し、原文と現在非参照のノートmetadataを返さない。
  第4段階ではmaintenance面の`plan_tag_vocabulary_rollback`へ元実行IDと理由を渡し、write面の
  `rollback_tag_vocabulary_change`へreceiptと新しいrollback IDを渡す。対象の現在版・正本指定・
  履歴・全snapshotを照合して全件復元し、元履歴へ復元記録を添える。必須eventの
  `tag_vocabulary_rolled_back`は元実行ID・復元ID・plan hash・復元件数・保存結果・出力待ちを返す。
  通常updateの件数へ混ぜず、応答喪失時は元実行IDの履歴で確認して別IDで再送しない。
  read面の`get_tag_vocabulary_stats`は端末の保存済み全期間から適用・復元と延べ変更ノート数、
  最近10実行、出力待ちを集計し、未計測の計画回数・拒否・実行時間・意味品質を明示する。
  変更/復元plan・履歴取得・運用集計は読取り専用で、DB準備・同期を伴わない。専用GUIは後続範囲とする。
- note link、degradation、KB OFF、tool errorを`structuredContent.conversation_events` v1へ
  `required=true`で返す。対応hostはモデルの最終文と独立して描画する。
- 2026-09-05本人指定: 起票・更新の報告には、参照リンク付きタイトルとnamespace/scopeを含める。
  text応答をそのまま一行報告にできる形にし、`note_created` / `note_updated` eventには保存後の
  authorityも載せる。legacyのauthority不在はnullとし、textでは未設定と示す。非対応host向けには
  instructionsでも会話への報告を求めるが、下記の保証境界は変わらない。

## 保証境界

この変更で通常チャットにmanaged pre-answer hookが生えるわけではない。Claude Desktop / ChatGPTは
tool discovery型のため、「関連時に必ず検索する」は引き続き保証外である。conversation eventも、
対応hostが描画するまでは構造化された配送契約であり、既存クライアントの最終文を強制しない。
ChatGPT向けremote MCP、OAuth、製品deep linkは別PoCとする。
`new_tag_write_available=false`は通常tagsの語彙検証を直接overrideできないことを表す。
語彙ノートの更新はAIが判断して行える。意味に基づく既存語の選択や増殖抑制はAIの判断であり、
この能力制限だけで品質を保証するものではない。

## 却下した案

- **model familyだけで分岐**: Claude DesktopとClaude Code、ChatGPTとCodexの能力差を表せない。
- **instructionsへ追記**: 実測でリンク・degradation・タグ規律の欠落が残った。
- **`allow_new_tags`を説明付きで公開**: Claude Codeがタグ運用を読んだ後でも語彙表にないタグを追加したため、
  通常経路から直接override能力を除く。語彙表を先に更新する経路は維持する。
- **全surfaceで引数なし`get`**: current-noteが存在しないcoding agentでtool errorを誘発した。
