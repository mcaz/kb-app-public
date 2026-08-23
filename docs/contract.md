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
| 8 | **KBを利用するAIは Vault の生ファイルを読まず、kb-app の取次口だけを使う**(2026-08-17 追加、2026-08-19 client surface分離、2026-08-22 tool surface分離・Codex開発モード追加) | `client` actorの先頭segmentを`ClientSurface`へ厳密変換し、モデル名や`claude` / `gpt`の部分一致で能力を推測しない / MCP tool surfaceは`read`（search / get / recent）、`write`（propose / update / attach / 二段階remove）、`maintenance`（競合解消・蒸留・initiative・旧Artifact移行）へ分け、各serverは一覧にないtoolの直接callもVault操作前に`tool_surface_mismatch`で拒否 / 自動retrievalの子MCPは`read`固定 / strict modeではCodex CLI は管理 `requirements.toml` の global deny-read と専用 permission profile、Claude Code は管理 `managed-settings` の OS sandbox で、登録 Vault と kb-app 端末設定を deny-read / deny-write / unsandboxed escape 無効にする / 組み込み Read と shell の子 process の両方を拒否 / 両coding agentの管理 `UserPromptSubmit` hookは同じkb-app実行ファイルをMCP serverとして子起動し、モデル判断の前に `initialize → search(include_documents)` を一度実行して、上位5 seedからDB有向リンクを最大2ホップ展開し、最大50候補から予算内・最大10本文を同じDB snapshotで取得 / Claude Desktop・ChatGPT通常チャット面はkb-app MCPが生path能力を公開しないbroker境界とし、managed hookによる検索開始保証は主張しない / 未知client surfaceはfail-closed / OFFでもinitializeはtools capabilityとON時と同じtool surfaceを公開し、全tools/callをVault操作前に同一の構造化終端結果`kb_disabled`（`authoritative=true` / `retryable=false` / 空data）で拒否 / promptsは非公開 / hookはこの終端結果なら無音終了し、それ以外の失敗は該当なしへ変換せず劣化として届ける / coding agentで管理ポリシーが未導入・古い・競合・登録 Vault 不一致なら設定 UI の switch を操作不能にし、MCPも同じ終端結果でfail-closed / ONはMCPからデータを返せるようにするだけで、生ファイル拒否を緩めない / **明示的なCodex開発高速モードだけを一時例外**とし、strict modeが正常な状態から管理者認証で遷移、Codexだけを`:danger-full-access`へ切替、CodexのMCP・自動retrievalを同時にfail-closed、Claude Codeのstrict guardは維持 / 検証・テスト・リリース前はstrict modeへ戻し、`release-check`は戻っていなければ失敗する |
| 9 | **現行の正本・記録・候補をpathや本文推測ではなくauthority envelopeで一意に判定する**(2026-08-20追加、2026-08-23 query intent・anchor text・relation ranking追加) | 新規proposeはcore発行の不変`note_uid`と、共通6namespace (`entities` / `initiatives` / `decisions` / `procedures` / `records` / `knowledge`)、role (`canonical` / `record` / `proposal`)、status (`active` / `historical` / `superseded`)、安定scopeを必須化 / legacyノートは明示移行までenvelope不在の読み取りを許す / 同じnamespace+scopeのactive canonicalはSQLiteの部分unique indexとStorage Contractの両方で1件に固定 / `note_uid`の差し替えを拒否 / typed relation (`derived_from` / `supports` / `updates` / `contradicts` / `supersedes` / `mentions`) は存在する`note_uid`だけを端点にし、自己参照・重複・参照切れを拒否 / `supersedes`は同じnamespace+scopeのactive canonicalからsuperseded canonicalへだけ結び、後継のないsupersededを拒否 / typed relationで参照中のノートは、参照元を整理するまで削除しない / 検索は完全タイトル一致を最優先とし、明示された現行・履歴・記録・理由intentをauthorityのstatus・role・namespaceへ対応付け、標準Markdown linkのanchor textは全query term一致時だけリンク先を昇格する弱い派生索引とし、intentもanchor一致も無い候補間ではactive canonicalをrecord・proposal・supersededより優先 / retrievalのseed展開では根拠・系譜・変更・矛盾・後継を表すtyped relation、通常Markdown link、`mentions`の順で候補化する |
| 10 | **継続蒸留の候補planは同一snapshotへ固定し、previewだけでは一切書き込まない**(2026-08-20追加) | plannerは既存schemaのSQLiteをread-only + query-onlyで開き、単一read transactionの全DB documentから各input SHA-256、snapshot digest、決定的plan IDを生成 / schema作成・migration・Markdown復元・pull・sync・care・outbox・埋め込み追従を行わない / authorityとtyped relationで機械的に証明できる候補だけを出し、本文意味が必要なlegacy分類・proposal判断・splitはunresolvedまたは予約値に留める / planは承認キューや実行権限にせず、将来executorは実行直前にsnapshotと全input hashを再照合して不一致なら拒否 / 複数ノートの正本遷移は専用transactionでatomicに行い、単一updateの連続で代替しない |
| 11 | **semantic蒸留waveはplan全体を再照合し、全件成功または0件に固定する**(2026-08-20追加) | executorは同じwrite transaction内でplan schema・profile・ID、snapshot digest・note count、全対象input hash・operationを再計算し、不一致をwrite前に拒否 / request SHA-256のexecution IDとSQLite一意制約で二重実行を拒否 / v1はauthority付きAIノートの`normalize`(descriptionのみ)、active canonicalの`revise`、recordの`extract`(description・relationsのみ)へ能力を限定し、record本文・note UID・authorityを不変にする / 全note・索引・durable outbox・実行前後document・audit rowを1 transactionへ積み、途中失敗は全rollback / rollbackは現在の全snapshotと対象documentがexecution直後から不変の場合だけ全件を実行前planへ戻し、二重rollbackと後続変更後のrollbackを拒否 / create・delete・merge・supersede・split・legacy backfillは入力能力として公開せず、削除は対象固定型二段階操作を維持 |
| 12 | **継続蒸留の再開集合は増分化しても、受入判定は現在の全体状態へかける**(2026-08-20追加) | auditは自己digestを再照合したcheckpointと現在planを不変`note_uid`（legacyだけpath fallback）で比較し、追加・変更・削除・移動を分離 / worksetは現存差分、全non-keep・risk候補、`depends_on`の双方向閉包に固定し、baseline無しは全件 / gateはworksetだけでなく現在の全plan、unresolved・risk、Markdown outbox、Storage Contract、local Gitの未backup commitを検査 / auditはread-only DBを使い、pull・network I/O・migration・索引・care・outbox更新を行わない / checkpointとgate PASSは実行権限ではなく、executorの全snapshot再照合を省略しない |
| 13 | **継続蒸留cadenceは最後に受入成功したcheckpointから再開し、失敗ではbaselineを進めない**(2026-08-20追加) | workspace IDごとの端末ローカルstateをprocess間lockとatomic replaceで永続化 / accepted checkpointとの差分を追加直後、24時間、7日、30日の固定laneで評価 / 初回は全laneをdueにし、追加直後は直接差分、日次は依存閉包、週次はactive canonical、月次は全件をreview scopeへ加える / 全体gate PASS時だけcheckpointとlane完了時刻を更新し、失敗時は旧checkpointを維持してaudit ID・failed checkを持ち越す / cadenceはKB・索引・outbox・remoteを変更せず、semantic writeとexecutorの再照合を自動化・省略しない |
| 14 | **完了initiativeのauthority遷移は本文蒸留と分離し、対象固定のatomic waveで閉じる**(2026-08-21追加) | `plan_initiative_closure`はAI管理のactive canonical initiativeだけを対象にnote ID・note UID・input hash・全DB snapshot・active→historical・一行理由を固定 / `apply_initiative_closure`は同じwrite transaction内でplanを再構成し、全対象一致後にauthority statusだけを変更して本文・title・description・tags・relations・namespace・role・scope・note UIDを不変にする / stale plan・改ざん・二重実行・対象外authority・部分成功を拒否 / `rollback_initiative_closure`は全snapshotと全対象documentがapply直後から不変の場合だけ全件をactiveへ戻す / 直接updateの連続でこの複数ノート遷移を代替しない |
| 15 | **自動retrievalは長文全体ではなくqueryに合うpassageをモデルへ渡す**(2026-08-23追加) | 推定4,000 token以下の文書は従来どおり全文 / 超過文書はfrontmatterを保ちMarkdown見出しsectionへ分割 / 巨大sectionまたは見出し無し本文は2,400 byte以下へ分割 / 完全query一致、異なるquery語のcoverage、本文順で決定的に順位付け / 重複passageを除き、1文書あたり最大3 passage・推定3,600 tokenへ縮約 / 全体の候補上限50、本文上限10、推定10,000 token予算は維持 |

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
- Codex開発高速モードは一般的なON/OFFではなく、ローカル開発中の承認待ちを減らす明示的な
  一時状態である。`:danger-full-access`と`approval_policy=never`を管理要件で固定し、状態は設定UIとCLIへ
  警告表示する。CodexからのKB brokerを止めることで「KBを使いながら
  生ファイル境界だけ解除」という曖昧な組合せを作らない。切替後はCodexを再起動し、検証・配布前には
  strict modeへの復旧と`kb settings dev-mode release-check`を必須にする。
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
- 契約11のexecutionも人間承認状態ではない。AIが候補全文と方針を確認して構造化requestを作り、
  stale plan拒否とatomic rollbackを機構で受け持つ。destructive annotationは確認UIの要求ではなく、
  clientが書き込み能力を正しく分類するために付ける。
- 契約12のremote backup検査は、remote設定の有無とlocal Gitのaheadだけを読む。監査の決定性と
  closed-world性を保つため、fetch・pull・GitHub APIによるprivate性、到達性、remote側最新性の能動確認は
  行わない。それらはupload直前の契約7と、後続のactive remote health評価で別に強制する。
- 契約13の端末ローカルstateは正本noteやバックアップではなく、再開点と期限の制御状態である。
  壊れたstateを初期状態として黙って扱わずfail-closedにし、明示的に復旧するまでcheckpointを進めない。
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
