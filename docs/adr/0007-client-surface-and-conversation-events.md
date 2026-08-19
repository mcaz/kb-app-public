# ADR-0007: client surfaceを分離し、必須表示をconversation eventで返す

- 日付: 2026-08-19
- 状態: 採用
- 関連: [contract.md](../contract.md) 契約1・4・8 / [Rule Delivery評価](../rule-delivery-evaluation.md)

## 背景

Claude Code / Codex CLIでRule Delivery Matrixを各80回実測したところ、Always + Topic + Eventが
最良だったが、Claude Codeは20件中18件、Codex CLIは20件中12件に留まった。特にnote link、
degradation、未合意タグ、引数なし`get`は、instructionsやtool応答をモデルが理解して最終文へ
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
  新語追加能力はtrusted UI / CLIの別承認経路に限定する。
- note link、degradation、KB OFF、tool errorを`structuredContent.conversation_events` v1へ
  `required=true`で返す。対応hostはモデルの最終文と独立して描画する。

## 保証境界

この変更で通常チャットにmanaged pre-answer hookが生えるわけではない。Claude Desktop / ChatGPTは
tool discovery型のため、「関連時に必ず検索する」は引き続き保証外である。conversation eventも、
対応hostが描画するまでは構造化された配送契約であり、既存クライアントの最終文を強制しない。
ChatGPT向けremote MCP、OAuth、製品deep linkは別PoCとする。

## 却下した案

- **model familyだけで分岐**: Claude DesktopとClaude Code、ChatGPTとCodexの能力差を表せない。
- **instructionsへ追記**: 実測でリンク・degradation・タグ規律の欠落が残った。
- **`allow_new_tags`を説明付きで公開**: Claude Codeがタグ運用を読んだ後でも未合意タグを追加したため、
  通常経路から能力自体を除く。
- **全surfaceで引数なし`get`**: current-noteが存在しないcoding agentでtool errorを誘発した。
