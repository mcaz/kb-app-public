# アプリ契約 — 最低限の強制ルール(正本)

2026-08-10 制定。**このアプリが UI として成立するために強制する不変ルール**。
運用(どのタグをどう使うか・ノートの書き方の好み等)は可変であり、AI とユーザーの
会話で合意して KB 内の「タグ運用」ノート等に記録する — **この文書はその外側**にある。
契約を KB 内のノートに置くと KB の裁量で壊せてしまうため、定義はここ(アプリの docs)、
強制はコード(kb-core)、配布は server instructions が担う。

| # | 契約 | 強制点 |
|---|---|---|
| 1 | **すべてのノートはタグを1〜4個持つ**(タグは主題を横断する可変facetであり、正本判定には使わない) | propose / update / import は core の単一validationで個数・形(英小文字・数字・ハイフン、20文字以内)・語彙を検査 / 「タグ運用」ノートがあればその語彙表だけを正本とし、無い新規vaultだけ現用語からbootstrap / **語彙外の新語は拒否**(近い既存語を添えて返す) / 新語追加の`allow_new_tags`はtrusted UI / CLIの別承認経路だけに置き、AI用MCPのschemaから能力を除外し、未知引数として渡されても書込前に拒否 / 外部編集を含む既存違反は読み取りを止めずcareへ表示し、silent normalizeしない |
| 2 | **ノートの形式(OKF frontmatter)はアプリが管理**する。直接の形式いじりはしない | parse 検証・書き込みはコア API 経由のみ |
| 3 | **ノートは AI の領分**(update / 蒸留 / 削除判断と実行は AI、人間は方針を統治)。旧 `origin: human` ノートは互換読み取り専用 | 通常の書き込み口は propose / update と対象固定型の二段階削除だけ。個別の人間承認を要求せず、MCPはprepare_removeで対象ID・内容指紋へ固定した5分tokenと`required=true`のremoval_prepared eventを返し、同じnoteとtokenを受けるdestructive annotation付きcommit_removeだけが削除する。tokenは使用時に失効し、期限切れ・対象差し替え・準備後変更・二重実行をコアで拒否する。削除理由・対象・履歴を残し、Git履歴から回復可能にする。新規・移植ノートは`origin: agent`。CLIに人間用のnew / edit / archive / deleteを公開せず、コアのraw writeは移植内部に閉じる |
| 4 | **同期・検索索引は派生**。壊れたら画面に出す(沈黙しない) | 続行可能な失敗は `data + Degradation[]` で返し、安定した `code` をUI/MCPへ表示 / MCPは`structuredContent.conversation_events`へ`required=true`のdegradation・note link・終端状態を載せ、対応hostはモデルの最終文へ委ねず描画 / 取得失敗を空配列へ変換して「0件」と偽らない / Markdown出力失敗は`markdown_export`として残す / 続行不能だけをerrorにする |
| 5 | **ファイルの実体と持ち出し範囲はコアが守る**(2026-08-12 追加) | 新規取り込みは `managed` のみ(`Linked` は旧 record の互換読み取り専用) / path 経路は picker・drop・paste・CLI が同一のコア API に合流 / MCP へは content 経路だけを公開し path を受け取らない / `client_repo` 由来は `local_only` 固定で instructions・prompt から緩和不能 / **緩和の能力を MCP に公開しない**(能力の不在)/ content と作成時 provenance は不変(更新は新しい版)/ **新しい版は前の版の区分を引き継ぎ、渡された指定を見ない**(2026-08-13 追加)/ availability は同期せず端末ごとに導出 |
| 6 | **正本は特定の保存形式ではなく再現可能性契約(Storage Contract)で定義する**(2026-08-16追加、2026-08-18実行面改定) | 日常のAI・GUI・CLIはSQLite transactionから読み書き / 同じtransactionでdurable outboxを積みMarkdownへ出力 / MarkdownはObsidian表示・Gitバックアップ・fresh clone復元用で、通常syncは外部編集を暗黙importしない / DB binary自体はGit同期しない / repository の論理状態を決定的な JSON snapshot へ exportしSHA-256 digestで比較 / fresh cloneからDBと検索索引を再構築する受入テスト |
| 7 | **個人データを送る前に、接続先が認証済みの private repository であることを確認する**(2026-08-16 追加) | GitHub API の認証済み応答で private + push 権限を確認 / public・internal・未認証・404・通信失敗・判定不能は fail-closed / LFS を含む各 upload の直前に再確認 / 初回は「private repository を作る」と「既存 Vault を使う」を分ける / 既存 Vault は `.kb-workspace` が一致するときだけ現在の Vault に接続し、不一致を自動 merge・上書きしない |
| 8 | **AI は Vault の生ファイルを読まず、kb-app の取次口だけを使う**(2026-08-17 追加、2026-08-19 surface分離) | `client` actorの先頭segmentを`ClientSurface`へ厳密変換し、モデル名や`claude` / `gpt`の部分一致で能力を推測しない / Codex CLI は管理 `requirements.toml` の global deny-read と専用 permission profile、Claude Code は管理 `managed-settings` の OS sandbox で、登録 Vault と kb-app 端末設定を常時 deny-read / deny-write / unsandboxed escape 無効にする / 組み込み Read と shell の子 process の両方を拒否 / 両coding agentの管理 `UserPromptSubmit` hookは同じkb-app実行ファイルをMCP serverとして子起動し、モデル判断の前に `initialize → search(include_documents)` を一度実行して、上位5 seedからDB有向リンクを最大2ホップ展開し、最大50候補から予算内・最大10本文を同じDB snapshotで取得 / Claude Desktop・ChatGPT通常チャット面はkb-app MCPが生path能力を公開しないbroker境界とし、managed hookによる検索開始保証は主張しない / 未知surfaceはfail-closed / OFFでもinitializeはtools capabilityとON時と同じtool setを公開し、全tools/callをVault操作前に同一の構造化終端結果`kb_disabled`（`authoritative=true` / `retryable=false` / 空data）で拒否 / promptsは非公開 / hookはこの終端結果なら無音終了し、それ以外の失敗は該当なしへ変換せず劣化として届ける / coding agentで管理ポリシーが未導入・古い・競合・登録 Vault 不一致なら設定 UI の switch を操作不能にし、MCPも同じ終端結果でfail-closed / ONはMCPからデータを返せるようにするだけで、生ファイル拒否を緩めない |
| 9 | **現行の正本・記録・候補をpathや本文推測ではなくauthority envelopeで一意に判定する**(2026-08-20追加) | 新規proposeはcore発行の不変`note_uid`と、共通6namespace (`entities` / `initiatives` / `decisions` / `procedures` / `records` / `knowledge`)、role (`canonical` / `record` / `proposal`)、status (`active` / `historical` / `superseded`)、安定scopeを必須化 / legacyノートは明示移行までenvelope不在の読み取りを許す / 同じnamespace+scopeのactive canonicalはSQLiteの部分unique indexとStorage Contractの両方で1件に固定 / `note_uid`の差し替えを拒否 / typed relation (`derived_from` / `supports` / `updates` / `contradicts` / `supersedes` / `mentions`) は存在する`note_uid`だけを端点にし、自己参照・重複・参照切れを拒否 / `supersedes`は同じnamespace+scopeのactive canonicalからsuperseded canonicalへだけ結び、後継のないsupersededを拒否 / typed relationで参照中のノートは、参照元を整理するまで削除しない / 検索は候補集合を変えずactive canonicalをrecord・proposal・supersededより優先 |
| 10 | **継続蒸留の候補planは同一snapshotへ固定し、previewだけでは一切書き込まない**(2026-08-20追加) | plannerは既存schemaのSQLiteをread-only + query-onlyで開き、単一read transactionの全DB documentから各input SHA-256、snapshot digest、決定的plan IDを生成 / schema作成・migration・Markdown復元・pull・sync・care・outbox・埋め込み追従を行わない / authorityとtyped relationで機械的に証明できる候補だけを出し、本文意味が必要なlegacy分類・proposal判断・splitはunresolvedまたは予約値に留める / planは承認キューや実行権限にせず、将来executorは実行直前にsnapshotと全input hashを再照合して不一致なら拒否 / 複数ノートの正本遷移は専用transactionでatomicに行い、単一updateの連続で代替しない |

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
  `index.md`、検索索引、埋め込みは派生なので含めず、ノート、監査ログ、Git 管理される Artifact
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
  OSガードを再判定してから、公開されたsearchのDB本文同梱オプションだけを呼ぶ。候補探索と本文予算選択は
  DB内で完結し、Markdownを検索時に読み直さない。Claudeだけにあった旧Python hookはmanaged hook導入時に
  削除し、CodexとClaudeの差を残さない。
- 契約9の`proposal`は**人間の承認待ち状態ではない**。AIが蒸留中に正本候補と既存canonicalを
  区別するためのauthority roleであり、承認キュー・確定ボタン・下書きバッジ・状態filterを
  UIへ復活させない。ユーザーは方針を統治し、AIはその範囲で分類・統合・削除を実行する。
- 契約9のlegacy互換は恒久的な別体系ではない。envelopeの無い既存ノートは読めるが、新規proposeは
  必ずauthorityを持つ。物理path移動は安定UID・alias・transactional moveが揃う別段まで行わない。
- 契約10のplanは人間の承認状態を追加しない。`proposal`と同様、AIが自律メンテナンスを安全に
  分解・再現するための機械出力であり、受信箱・確定ボタン・waveごとの承認作業をユーザーへ戻さない。
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
