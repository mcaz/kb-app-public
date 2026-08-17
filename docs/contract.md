# アプリ契約 — 最低限の強制ルール(正本)

2026-08-10 制定。**このアプリが UI として成立するために強制する不変ルール**。
運用(どのタグをどう使うか・ノートの書き方の好み等)は可変であり、AI とユーザーの
会話で合意して KB 内の「タグ運用」ノート等に記録する — **この文書はその外側**にある。
契約を KB 内のノートに置くと KB の裁量で壊せてしまうため、定義はここ(アプリの docs)、
強制はコード(kb-core)、配布は server instructions が担う。

| # | 契約 | 強制点 |
|---|---|---|
| 1 | **すべてのノートはタグを1〜4個持つ**(タグはこのアプリの一次の整理手段) | propose / update / import は core の単一validationで個数・形(英小文字・数字・ハイフン、20文字以内)・語彙を検査 / 「タグ運用」ノートがあればその語彙表だけを正本とし、無い新規vaultだけ現用語からbootstrap / **語彙外の新語は拒否**(近い既存語を添えて返し、`allow_new_tags` を明示したときだけ通す) / 外部編集を含む既存違反は読み取りを止めずcareへ表示し、silent normalizeしない |
| 2 | **ノートの形式(OKF frontmatter)はアプリが管理**する。直接の形式いじりはしない | parse 検証・書き込みはコア API 経由のみ |
| 3 | **ノートは AI の領分**(update / remove は AI、削除指示は人間)。旧 `origin: human` ノートは互換読み取り専用 | 通常の書き込み口は propose / update / remove のみ。新規・移植ノートは `origin: agent`。CLI に人間用の new / edit / archive / delete を公開せず、コアの raw write は移植内部に閉じる |
| 4 | **同期・索引は派生**。壊れたら画面に出す(沈黙しない) | 続行可能な失敗は `data + Degradation[]` で返し、安定した `code` をUI/MCPへ表示 / 取得失敗を空配列へ変換して「0件」と偽らない / 続行不能だけをerrorにする |
| 5 | **ファイルの実体と持ち出し範囲はコアが守る**(2026-08-12 追加) | 新規取り込みは `managed` のみ(`Linked` は旧 record の互換読み取り専用) / path 経路は picker・drop・paste・CLI が同一のコア API に合流 / MCP へは content 経路だけを公開し path を受け取らない / `client_repo` 由来は `local_only` 固定で instructions・prompt から緩和不能 / **緩和の能力を MCP に公開しない**(能力の不在)/ content と作成時 provenance は不変(更新は新しい版)/ **新しい版は前の版の区分を引き継ぎ、渡された指定を見ない**(2026-08-13 追加)/ availability は同期せず端末ごとに導出 |
| 6 | **正本は特定の保存形式ではなく再現可能性契約(Storage Contract)で定義する**(2026-08-16 追加) | repository の論理状態を決定的な JSON snapshot へ export / SHA-256 digest で同一性を比較 / `storage verify` は読み取り専用で破損を黙殺しない / fresh clone から同じ digest と派生索引を再構築する受入テスト。現行の Markdown・OKF・Git は交換可能な adapter |
| 7 | **個人データを送る前に、接続先が認証済みの private repository であることを確認する**(2026-08-16 追加) | GitHub API の認証済み応答で private + push 権限を確認 / public・internal・未認証・404・通信失敗・判定不能は fail-closed / LFS を含む各 upload の直前に再確認 / 初回は「private repository を作る」と「既存 Vault を使う」を分ける / 既存 Vault は `.kb-workspace` が一致するときだけ現在の Vault に接続し、不一致を自動 merge・上書きしない |
| 8 | **AI は Vault の生ファイルを読まず、kb-app の取次口だけを使う**(2026-08-17 追加) | Codex は管理 `requirements.toml` の global deny-read と専用 permission profile、Claude Code は管理 `managed-settings` の OS sandbox で、登録 Vault と kb-app 端末設定を常時 deny-read / deny-write / unsandboxed escape 無効にする / 組み込み Read と shell の子 process の両方を拒否 / 両クライアントの管理 `UserPromptSubmit` hookは同じkb-app実行ファイルをMCP serverとして子起動し、モデル判断の前に `initialize → search → get` を実行 / OFFならinitializeでtoolsを公開せず無音終了 / hook失敗は該当なしへ変換せず劣化として届ける / 管理ポリシーが未導入・古い・競合・登録 Vault 不一致なら設定 UI の switch を操作不能にし、MCP も tools / prompts を公開せず fail-closed / ON は MCP だけを開き、生ファイル拒否を緩めない |

- タグ無しノートは契約違反状態として**お手入れの気づき**に出す(修復は Claude への依頼で)
- 契約5 の「新しい版は前の版の区分を引き継ぐ」は、確認を1段置いても
  **同じ結果へ到達する別の操作から漏れる**ため(「新しい版として追加」は新規取り込みと
  同じ経路で、渡された policy を効かせると確認を通らない緩和になる)。
  強制点は `kb-core` の `intake::take`、検査は `a_new_version_cannot_widen_the_boundary`
- 契約5 に**個別の受入条件までは書かない**(この文書は「最低限」であるため)。
  Artifact の受入条件14項目は [ADR-0003](adr/0003-artifact-storage-and-transport.md) と
  kb-core のテストが持つ。ここに置くのは、instructions では守れず**能力・スキーマ・コアで
  必然にすべきもの**だけ(設計原則の階段の上2段に当たるもの)
- 契約6 の snapshot は**バックアップファイルそのものではなく比較・検査用の論理表現**。
  `index.md` と `.kb/index.db` は派生なので含めず、ノート、監査ログ、Git 管理される Artifact
  台帳・参照、旧添付の path/size/hash を含める。`full` の実体は同じ origin の Git LFS から
  取得し、明示的に `local_only` とした実体は clone 再現の対象外。後者を別端末へ運ぶ完全 bundle
  は ADR-0004 の次段で定義する
- 契約7で Vault の同一性を repository 名・URL・ローカル path から推測しない。それらは変更可能で
  端末ごとにも異なるため、Git で運ばれる `.kb-workspace` の永続 ID だけを使う。新しい端末では
  既存 repository を clone して Storage Contract を検査し、`full` の全 LFS object を取得・hash
  照合してから復元完了とする。接続後に repository が public 等へ変更された場合、以後の upload は
  止めて重大な劣化として表示する(既に公開されたデータをアプリが取り消せるとは扱わない)
- 契約8の拒否対象には、登録済み Vault の実体 path と canonical path、既定の `~/kb`、
  `settings.json` / `registry.json` を置く kb-app の端末設定ディレクトリを含める。AI が別名 path、
  `cat`、Python、kb CLI、設定ファイル改変のいずれを選んでも同じ OS 境界で止まる。管理アプリ、
  kb-app MCP、本人が使う通常の terminal は AI client の sandbox 外なので影響を受けない。policy は
  policy file と root までの全 directory が root 所有かつ group / other 書き込み不可まで検査し、
  内容だけ同じ user 所有 file や差し替え可能な親 directory は有効と扱わない。
- 契約8の自動retrievalはserver instructionsによる自発的なtool選択とは別に実行する。hookは
  CLI検索・設定ファイル・Vaultを直接読まず、MCPのinitializeでそのクライアントのON/OFFと
  OSガードを再判定してから、公開されたsearchとgetだけを呼ぶ。Claudeだけにあった旧Python
  hookはmanaged hook導入時に削除し、CodexとClaudeの差を残さない。
- 契約の変更はこの文書の改定+コアの強制点の変更として行う(instructions だけの変更は不可)
- **語彙の正本は設定ファイルでなく「タグ運用」ノートの `## 語彙` 節**。
  どの語を使うかは運用(会話で合意し KB のノートに記録する)であり、この文書は
  「語彙の外に出るときは明示せよ」という形だけを強制する。語彙表の読めない行は
  黙って捨てずお手入れに出す(契約4と同じ理由)。
  語彙ノートがまだ無い新規vaultだけは現用タグからbootstrapし、語彙ノートがあるvaultでは
  外部編集や旧データで混入した現用タグを正式語彙へ自動昇格させない。
  2026-08-12 追加 — 60ノートに112語・1回きり61%まで増殖し、常駐文章では止まらないと
  実測されたため(強制の階段: 常駐文章 → スキーマ・コア検証)

### 契約に**入れない**もの(2026-08-11 本人決定)

**「下書き(draft)」を状態として持たない。** 暫定・要確認・レビュー待ちといった扱いは
**ただのタグ**であり、そのためにアプリへ専用機能(承認キュー・確定ボタン・下書きバッジ)を
作るのは越権。どう扱うかは**ユーザーと AI が会話で決める運用**に属する。
同じ理由で、状態による絞り込み UI もアプリは持たない(タグ絞り込みのみ)。

## 設計原則(2026-08-10 本人決定)

**確定すべき挙動は AI の理解力に期待せず、機構で必然にする。** 文章(instructions)に
残してよいのは、裁量が本質の挙動(何を検索するか・何を残す価値と見るか等)だけ。
強制の手段は強い順に: 能力の不在(ツール非公開)> スキーマ・コア検証 > フックの
事前/事後ブロック(Claude Code)> ツール応答・エラーでのその場教育 > 常駐文章。
新しい規律を足すときは、まずこの階段の上から検討する。
