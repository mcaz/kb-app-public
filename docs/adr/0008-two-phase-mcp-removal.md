# ADR-0008: AIの自律削除を対象固定tokenによる二段階操作にする

- 日付: 2026-08-19
- 状態: 採用
- 関連: [contract.md](../contract.md) 契約3・4 / [ADR-0007](0007-client-surface-and-conversation-events.md)

## 背景

ChatGPT通常チャットからSecure MCP Tunnel経由で一時ノートを削除したPoCでは、削除と履歴保存は
成功したが、削除対象を実行前に示すメッセージや確認がないまま直接`remove`が実行された。
tool descriptionの「削除前に一言添える」はモデルとhostの解釈に依存し、固定評価T4の
「削除対象を明示してから実行」をsurface横断で保証できなかった。

## 決定

- MCPから直接`remove`を除き、`prepare_remove`と`commit_remove`へ分割する。
- `prepare_remove`は`origin: agent`を確認し、ノートID・タイトル・内容指紋へ固定した256-bit tokenを
  process memoryへ保存する。有効期限は5分とし、MCP processの再起動でも失効する。
- `prepare_remove`は対象と期限をtext・structuredContentの両方へ返し、
  `removal_prepared`を`required=true`のconversation eventとして配送する。
- `commit_remove`は同じnoteとtokenを必須とし、destructive annotationを付ける。tokenは成否にかかわらず
  1回の使用で失効させ、期限切れ、対象差し替え、準備後の内容変更、二重実行を削除前に拒否する。
- agentノートの蒸留・メンテナンスではAIが削除理由を判断し、個別の人間承認なしに
  `prepare_remove`から`commit_remove`へ進める。人間は個々の削除でなく、AIが自律整理する方針を統治する。
- 実削除と所有ガードは引き続きkb-coreの`agent_delete_note`へ合流し、履歴を残す。

## 保証境界

二段階化は人間承認ではなく、削除対象を固定した準備応答が破壊的tool callより前に存在することを機構保証する。
destructive annotationはhostへ操作の性質を正直に伝えるが、hostが独自の確認UIを挟むかはkb-appの保証外である。
対応hostはrequired conversation eventを描画する。自律性の保証点は個別確認の省略ではなく、AIが削除を判断して
両toolを呼べる能力、対象差し替え不能、監査履歴、Git履歴からの回復可能性に置く。

## 却下した案

- **tool descriptionだけを強める**: 実PoCで無視され、機構保証にならなかった。
- **削除ごとに人間確認を要求する**: 全ノートをAIが所有し継続蒸留する製品境界と矛盾し、メンテナンスを人間へ戻す。
- **destructive annotationだけを付ける**: host実装差があり、対象固定・期限・再実行防止を担えない。
- **永続tokenをDBへ保存する**: MCP再起動をまたぐ承認は不要で、漏えい時の有効期間と状態管理を増やす。
- **対象IDだけを二回渡す**: prepare後の対象差し替えや内容変更を検出できない。
