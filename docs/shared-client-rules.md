# 対応AI間の共通運用と確認できる範囲

2026-09-08。契約の正本は [contract.md](contract.md)。各AIの個人用AGENTS.md・CLAUDE.mdを
手編集することを導入条件にせず、アプリの登録と共通コアで記載方式・タグ操作を揃える。

## 接続の手順

「繋ぐ」で使用するAIの登録を検査し、選択中のKBに登録する。登録後はAI側でMCPを
再接続して新しい会話を開始し、アプリの再検査で登録と接続先の固定を確認する。
Codex・Claude Codeの自動検索には、別途「KB利用」の管理保護設定も必要になる。
新しいPCでも同じ手順を行い、workspace IDと語彙正本の識別情報を比較する。

古い単一MCP登録や実行ファイルはread・write・maintenanceへ修復する。ユーザーscopeの
設定を更新する前にバックアップを作り、他のMCPや設定を保持する。不正JSON/TOML、
読めない設定、別scopeの上書き、管理ポリシーの競合・判定不能は停止し、理由を表示する。
これらを回避して管理設定やプロジェクト固有設定を書き換えない。
設定形式は [Codex MCP](https://learn.chatgpt.com/docs/extend/mcp?surface=cli)・
[Codex設定](https://learn.chatgpt.com/docs/config-file/config-reference)・
[Claude Code MCP](https://code.claude.com/docs/en/mcp)・
[Claude管理MCP](https://code.claude.com/docs/en/managed-mcp)を参照する。
この診断が扱うのはユーザーscopeと既知の静的管理設定であり、任意のproject設定・CLI override・
MDM・稼働プロセスの完全検査ではない。修復直前に変更を再検査するが、別プロセスが同じ
advisory lockに従わない場合の最終置換直前の競合まで完全には防げない。バックアップを保持する。

## 対応経路

| client | macOSでの登録形式 | 自動検索の必要能力 | この変更で検査できること |
| --- | --- | --- | --- |
| Codex | user config.toml のmcp_servers | 管理OS sandbox・UserPromptSubmit hook・固定workspace | 登録内容、管理設定、hook出力の観測と版 |
| Claude Code | user .claude.json のmcpServers | 管理OS sandbox・UserPromptSubmit hook・固定workspace | 登録内容、管理設定、hook出力の観測と版 |
| Claude Desktop | claude_desktop_config.json のmcpServers | MCP tool選択。管理hookによる必須検索は対象外 | 登録内容、固定workspace、通常MCPで共通規則を公開 |

Windowsの配布・管理保護・実クライアント受入は別の実装項目であり、この表から対応完了とは
判断しない。ChatGPTのgatewayや未登録clientもこのローカル登録UIの対象には含めない。
PATH上のCLI版、kb-appのpackage版、モデルの自己申告を稼働client版の確認に使わない。
実クライアント版と受信は、確認できる証拠がない限り未確認と表示する。

## 規則の識別と観測

initializeの`rule_identity`は、コンパイルした契約文書と実際の案内それぞれのSHA-256、
client/tool surface、kb-app版を返す。OFFの案内は別のhashを持ち、通常initializeはKBを開かない。
ONのsearch/get/tag_vocabularyに付く`workspace_rule_identity`は同じDB snapshotで得た
workspace ID、語彙正本の状態・UID・指定revision・document SHA-256である。
指定revisionは本文updateでは変わらないため、本文の一致にはdocument hashを使う。
未指定・欠損・利用不能・診断失敗を現在版とみなさない。

hookは子MCPのsearch応答の識別情報を、既存の出力準備とstdout結果の台帳へ保存する。
本文・prompt・設定の秘密情報は保存しない。画面の「現在の規則と一致」は過去30日以内の
最後のhook出力について版が一致するという意味であり、現在動作中の接続確認ではない。
子MCPのinitialize案内はhook本文へ転送していないので、共通案内をhostへ届けた証拠にもならない。
出力準備だけ、stdout失敗、古い版、版のない旧観測をそれぞれ区別する。
同じミリ秒の出力に異なる状態が並ぶ場合、hashの順を実行順とみなさず順序未確認とする。

受信確認は未確認のまま残す。起票・更新の応答件数も別欄に表示し、現行規則でタグ追加・
統合・復元まで受入済みとは扱わない。観測台帳が読めない場合は件数を未確認とする。
複数PCの識別情報が同じでも、モデルの意味判断が同じになるという保証はしない。

## 受入の区分

合成KBのコア試験では、対応surface間の必須項目・タグ上限・未知語の拒否、語彙変更の
plan照合・適用・統合・復元を確認する。これはコアの機械的な統一性の試験である。
実AIが共通規則を受け取り、同じ課題で適切な操作を選ぶかはRule Delivery評価の対象であり、
コア試験の成功で置き換えない。正式な受入記録にはOS・実client版・実model・規則digest・
語彙digest・tool trace・再接続後/別PCの別を残す。未実行項目は未確認のまま残す。

既存のworkspace不一致・保存形式・タグ・stale planの拒否を緩めない。識別情報は診断用で、
古い版を示すclientからの宣言だけで保存契約を迂回したり、設定を自動変更したりしない。
