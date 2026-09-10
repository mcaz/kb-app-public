# アプリ契約 — 最低限の強制ルール(正本)

2026-08-10 制定。**このアプリが UI として成立するために強制する不変ルール**。
運用(どのタグをどう使うか・ノートの書き方の好み等)は可変であり、AI とユーザーの
会話で方針を決めて KB 内の「タグ運用」ノート等に記録する。2026-09-08の本人訂正により、
タグの個別の付与・新設・統合・削除はAIが判断し、本人合意を都度の必須条件にしない。
**この文書はその外側**にある。
契約を KB 内のノートに置くと KB の裁量で壊せてしまうため、定義はここ(アプリの docs)、
強制はコード(kb-core)、配布は server instructions が担う。

| # | 契約 | 強制点 |
|---|---|---|
| 1 | **すべてのノートはタグを1〜4個持つ**(タグは主題を横断する可変facetであり、正本判定には使わない) | propose / update / import は core の単一validationで個数・形(英小文字・数字・ハイフン、20文字以内)・語彙を検査 / 正本はworkspace_id+note_uidの明示指定で固定し、未指定かつ候補なしだけ現用語からbootstrap / **語彙外の新語を通常tagsへ直接指定した書込は拒否**(近い既存語を添えて返す) / `allow_new_tags`による直接overrideはAI用MCPのschemaから除外し、未知引数として渡されても書込前に拒否 / read MCPの`tag_vocabulary`はvalidationと共通の取得処理で指定状態・候補・指定ノートID・語彙・不正項目・強制有無を返す / write面の`set_tag_vocabulary_source`は接続先workspaceを照合し、対象UID・現在版を同じtransactionで照合して指定とdurable outboxを保存する / 読取の自動指定・題名による再選択・欠損からのfallbackをしない / 指定中と未出力の旧指定ノートの削除・通常参照不可化を共通write/importで拒否 / 語彙ノート本文は通常updateで変更でき、その後の通常書込は更新された語彙を検証する。使用中の登録語を本文編集・正本切替だけで削除することは共通write/import guardで拒否し、importは全ノート同期後の最終状態で判定する。語彙の新設・統合・削除もAIが判断し、個別の本人合意を必須にしない（2026-09-08本人訂正）。既存語の再利用・重複抑制と本人の明示的な訂正への追従は運用であり、必要性の意味判断をコアが検査するものではない / 外部編集を含む既存違反は読み取りを止めずcareへ表示し、silent normalizeしない |
| 2 | **ノートの形式(OKF frontmatter)はアプリが管理**する。直接の形式いじりはしない | parse 検証・書き込みはコア API 経由のみ |
| 3 | **ノートは AI の領分**(update / 蒸留 / 削除判断と実行は AI、人間は方針を統治)。旧 `origin: human` ノートは互換読み取り専用 | 通常の書き込み口は propose / update と対象固定型の二段階削除だけ。個別の人間承認を要求せず、MCPはprepare_removeで対象ID・内容指紋へ固定した5分tokenと`required=true`のremoval_prepared eventを返し、同じnoteとtokenを受けるdestructive annotation付きcommit_removeだけが削除する。tokenは使用時に失効し、期限切れ・対象差し替え・準備後変更・二重実行をコアで拒否する。削除理由・対象・履歴を残し、Git履歴から回復可能にする。新規・移植ノートは`origin: agent`。CLIに人間用のnew / edit / archive / deleteを公開せず、コアのraw writeは移植内部に閉じる |
| 4 | **同期・検索索引は派生**。壊れたら画面に出す(沈黙しない) | 続行可能な失敗は `data + Degradation[]` で返し、安定した `code` をUI/MCPへ表示 / MCPは`structuredContent.conversation_events`へ`required=true`のdegradation・note link・終端状態を載せ、起票・更新の成功応答は参照リンク付きタイトルとnamespace/scopeを一行で返し、同じnote link eventへ保存後のauthorityを同梱（legacy不在はnull） / 対応hostはモデルの最終文へ委ねず描画 / 取得失敗を空配列へ変換して「0件」と偽らない / Markdown出力失敗は`markdown_export`として残す / 続行不能だけをerrorにする |
| 5 | **ファイルの実体と持ち出し範囲はコアが守る**(2026-08-12 追加) | 新規取り込みは `managed` のみ(`Linked` は旧 record の互換読み取り専用) / path 経路は picker・drop・paste・CLI が同一のコア API に合流 / MCP へは content 経路だけを公開し path を受け取らない / `client_repo` 由来は `local_only` 固定で instructions・prompt から緩和不能 / **緩和の能力を MCP に公開しない**(能力の不在)/ content と作成時 provenance は不変(更新は新しい版)/ **新しい版は前の版の区分を引き継ぎ、渡された指定を見ない**(2026-08-13 追加)/ availability は同期せず端末ごとに導出 |
| 6 | **正本は特定の保存形式ではなく再現可能性契約(Storage Contract)で定義する**(2026-08-16追加、2026-08-18実行面改定) | 日常のAI・GUI・CLIはSQLite transactionから読み書き / 同じtransactionでdurable outboxを積みMarkdownへ出力 / MarkdownはObsidian表示・Gitバックアップ・fresh clone復元用で、通常syncは外部編集を暗黙importしない / DB binary自体はGit同期しない / repository の論理状態を決定的な JSON snapshot へ exportしSHA-256 digestで比較 / fresh cloneからDBと検索索引を再構築する受入テスト |
| 7 | **個人データを送る前に、接続先が認証済みの private repository であることを確認する**(2026-08-16 追加) | GitHub API の認証済み応答で private + push 権限を確認 / public・internal・未認証・404・通信失敗・判定不能は fail-closed / LFS を含む各 upload の直前に再確認 / 初回は「private repository を作る」と「既存 Vault を使う」を分ける / 既存 Vault は `.kb-workspace` が一致するときだけ現在の Vault に接続し、不一致を自動 merge・上書きしない |
| 8 | **KBを利用するAIは Vault の生ファイルを読まず、kb-app の取次口だけを使う**(2026-08-17 追加、2026-08-19 client surface分離、2026-08-22 tool surface分離・Codex開発モード追加) | `client` actorの先頭segmentを`ClientSurface`へ厳密変換し、モデル名や`claude` / `gpt`の部分一致で能力を推測しない / MCP tool surfaceは`read`（search / get / recent / tag_vocabulary / 語彙変更履歴・運用集計と提案レビュー専用get_proposal / 来歴閲覧history）、`write`（propose / update / set_tag_vocabulary_source / apply_tag_vocabulary_change / rollback_tag_vocabulary_change / attach / 二段階remove / 提案チケットの起票・改訂・レビュー）、`maintenance`（語彙変更/復元plan・競合解消・蒸留・initiative・旧Artifact移行・来歴backfill）へ分け、各serverは一覧にないtoolの直接callもVault操作前に`tool_surface_mismatch`で拒否 / 自動retrievalの子MCPは`read`固定 / strict modeではCodex CLI は管理 `requirements.toml` の global deny-read と専用 permission profile、Claude Code は管理 `managed-settings` の OS sandbox で、登録 Vault と kb-app 端末設定を deny-read / deny-write / unsandboxed escape 無効にする / 組み込み Read と shell の子 process の両方を拒否 / 両coding agentの管理 `UserPromptSubmit` hookは同じkb-app実行ファイルをMCP serverとして子起動し、モデル判断の前に `initialize → search(include_documents)` を一度実行して、上位5 seedからDB有向リンクを最大2ホップ展開し、最大50候補から予算内・最大10本文を同じDB snapshotで取得 / Claude Desktop・ChatGPT通常チャット面はkb-app MCPが生path能力を公開しないbroker境界とし、managed hookによる検索開始保証は主張しない / 未知client surfaceはfail-closed / OFFでもinitializeはtools capabilityとON時と同じtool surfaceを公開し、全tools/callをVault操作前に同一の構造化終端結果`kb_disabled`（`authoritative=true` / `retryable=false` / 空data）で拒否 / promptsは非公開 / hookはこの終端結果なら無音終了するが、明示ONで管理保護設定が古い場合だけ固定文の`guard_outdated`を通知し、検索・台帳I/Oは行わない。それ以外の失敗は該当なしへ変換せず劣化として届ける / coding agentで管理ポリシーが未導入・古い・競合・登録 Vault 不一致なら設定 UI の switch を操作不能にし、MCPも同じ終端結果でfail-closed / ONはMCPからデータを返せるようにするだけで、生ファイル拒否を緩めない / **明示的なCodex開発高速モードだけを一時例外**とし、strict modeが正常な状態から管理者認証で遷移、Codexだけを`:danger-full-access`へ切替、CodexのMCP・自動retrievalを同時にfail-closed、Claude Codeのstrict guardは維持 / 検証・テスト・リリース前はstrict modeへ戻し、`release-check`は戻っていなければ失敗する |
| 9 | **現行の正本・記録・候補をpathや本文推測ではなくauthority envelopeで一意に判定する**(2026-08-20追加、2026-08-23 query intent・anchor text・relation ranking追加) | 新規proposeはcore発行の不変`note_uid`と、共通6namespace (`entities` / `initiatives` / `decisions` / `procedures` / `records` / `knowledge`)、role (`canonical` / `record` / `proposal`)、status (`active` / `historical` / `superseded`)、安定scopeを必須化 / legacyノートは明示移行までenvelope不在の読み取りを許す / 同じnamespace+scopeのactive canonicalはSQLiteの部分unique indexとStorage Contractの両方で1件に固定 / `note_uid`の差し替えを拒否 / typed relation (`derived_from` / `supports` / `updates` / `contradicts` / `supersedes` / `mentions`) は存在する`note_uid`だけを端点にし、自己参照・重複・参照切れを拒否 / `supersedes`は同じnamespace+scopeのactive canonicalからsuperseded canonicalへだけ結び、後継のないsupersededを拒否 / typed relationで参照中のノートは、参照元を整理するまで削除しない / 検索は完全タイトル一致を最優先とし、明示された現行・履歴・記録・理由intentをauthorityのstatus・role・namespaceへ対応付ける / 完了確認・過去結果の質問は具体的な対象一致と本文に残る結果の根拠を併せて評価し、一般方針より対象の結果記録を優先する。未完了・失敗・未確認も結果に含め、historicalだけの一律昇格・完了条件や分類方法の誤検知・検索順位によるauthority変更をしない。完全タイトル・現行方針への明示質問・intentなしの既定順を維持し、候補抽出と経路融合後で同じ順位方針を使う / 標準Markdown linkのanchor textは全query term一致時だけリンク先を昇格する弱い派生索引とし、intentもanchor一致も無い候補間ではactive canonicalをrecord・proposal・supersededより優先 / retrievalのseed展開では根拠・系譜・変更・矛盾・後継を表すtyped relation、通常Markdown link、`mentions`の順で候補化する |
| 10 | **継続蒸留の候補planは同一snapshotへ固定し、previewだけでは一切書き込まない**(2026-08-20追加) | plannerは既存schemaのSQLiteをread-only + query-onlyで開き、単一read transactionの全DB documentから各input SHA-256、snapshot digest、決定的plan IDを生成 / schema作成・migration・Markdown復元・pull・sync・care・outbox・埋め込み追従を行わない / authorityとtyped relationで機械的に証明できる候補だけを出し、本文意味が必要なlegacy分類・proposal判断・splitはunresolvedまたは予約値に留める / planは承認キューや実行権限にせず、将来executorは実行直前にsnapshotと全input hashを再照合して不一致なら拒否 / 複数ノートの正本遷移は専用transactionでatomicに行い、単一updateの連続で代替しない |
| 11 | **semantic蒸留waveはplan全体を再照合し、全件成功または0件に固定する**(2026-08-20追加) | executorは同じwrite transaction内でplan schema・profile・ID、snapshot digest・note count、全対象input hash・operationを再計算し、不一致をwrite前に拒否 / request SHA-256のexecution IDとSQLite一意制約で二重実行を拒否 / v1はauthority付きAIノートの`normalize`(descriptionのみ)、active canonicalの`revise`、recordの`extract`(description・relationsのみ)へ能力を限定し、record本文・note UID・authorityを不変にする / 全note・索引・durable outbox・実行前後document・audit rowを1 transactionへ積み、途中失敗は全rollback / rollbackは現在の全snapshotと対象documentがexecution直後から不変の場合だけ全件を実行前planへ戻し、二重rollbackと後続変更後のrollbackを拒否 / create・delete・merge・supersede・split・legacy backfillは入力能力として公開せず、削除は対象固定型二段階操作を維持 |
| 12 | **継続蒸留の再開集合は増分化しても、受入判定は現在の全体状態へかける**(2026-08-20追加) | auditは自己digestを再照合したcheckpointと現在planを不変`note_uid`（legacyだけpath fallback）で比較し、追加・変更・削除・移動を分離 / worksetは現存差分、全non-keep・risk候補、`depends_on`の双方向閉包に固定し、baseline無しは全件 / gateはworksetだけでなく現在の全plan、unresolved・risk、Markdown outbox、Storage Contract、local Gitの未backup commitを検査 / auditはread-only DBを使い、pull・network I/O・migration・索引・care・outbox更新を行わない / checkpointとgate PASSは実行権限ではなく、executorの全snapshot再照合を省略しない |
| 13 | **継続蒸留cadenceは最後に受入成功したcheckpointから再開し、失敗ではbaselineを進めない**(2026-08-20追加) | workspace IDごとの端末ローカルstateをprocess間lockとatomic replaceで永続化 / accepted checkpointとの差分を追加直後、24時間、7日、30日の固定laneで評価 / 初回は全laneをdueにし、追加直後は直接差分、日次は依存閉包、週次はactive canonical、月次は全件をreview scopeへ加える / manifest/ref/aliasの内容識別値を本文checkpointと別に照合し、昇格・rollbackを含む台帳のみの変更も追加直後をdueにする（旧stateの識別値欠落も一度due） / 全体gate PASSかつ監査前後の台帳識別値一致時だけcheckpoint・Artifact baseline・lane完了時刻を更新し、失敗時は旧baselineを維持してaudit ID・failed checkを持ち越す / cadenceはKB・索引・outbox・remoteを変更せず、semantic writeとexecutorの再照合を自動化・省略しない |
| 14 | **完了initiativeのauthority遷移は本文蒸留と分離し、対象固定のatomic waveで閉じる**(2026-08-21追加) | `plan_initiative_closure`はAI管理のactive canonical initiativeだけを対象にnote ID・note UID・input hash・全DB snapshot・active→historical・一行理由を固定 / `apply_initiative_closure`は同じwrite transaction内でplanを再構成し、全対象一致後にauthority statusだけを変更して本文・title・description・tags・relations・namespace・role・scope・note UIDを不変にする / stale plan・改ざん・二重実行・対象外authority・部分成功を拒否 / `rollback_initiative_closure`は全snapshotと全対象documentがapply直後から不変の場合だけ全件をactiveへ戻す / 直接updateの連続でこの複数ノート遷移を代替しない |
| 15 | **自動retrievalは長文全体ではなくqueryに合うpassageをモデルへ渡す**(2026-08-23追加) | 推定4,000 token以下の文書は従来どおり全文 / 超過文書はfrontmatterを保ちMarkdown見出しsectionへ分割 / 巨大sectionまたは見出し無し本文は2,400 byte以下へ分割 / 完全query一致、異なるquery語のcoverage、本文順で決定的に順位付け / 重複passageを除き、1文書あたり最大3 passage・推定3,600 tokenへ縮約 / 全体の候補上限50、本文上限10、推定10,000 token予算は維持 |
| 16 | **検索上位枠を同一scope・類似本文だけで埋めず、異なるfacetへ配分する**(2026-08-23追加) | 完全タイトル・query intent・field score・authorityを先に評価 / scope・role・statusが全て同じ候補、またはrole・statusが同じ候補のうち正規化本文が同一か先頭2,048正規化文字の文字trigram Jaccardが85%以上の候補を同じclusterにする / 本文が同じでもrole・statusが異なるfacetは保持 / 各clusterの最上位1件だけをseed候補と最終検索結果へ残し、重複でlimitを埋め戻さない / 多様化用本文の取得失敗は`diversity_ranking` degradationとして検索結果と同時に返す |
| 17 | **会話の計測は観測した段階だけを記録し、本文とKBのOFF境界を守る**(2026-09-05追加) | `kb-core::session_ledger`はVault・同期から独立した端末ローカル台帳 / hookの出力準備`prepared`、stdout書込・flush完了`emitted`、出力失敗`stdout_failed`を分離し、MCP propose / update（提案チケットのcreateをpropose、revise/reviewをupdateに分類）は応答生成結果`success` / `error`と型で確定した拒否codeを記録 / KB OFF・ON未確認・未知clientでは台帳I/Oを行わない / 本文・query・タイトル・path・自由形式errorを保存せず、session等のIDはhash化 / session ID不在は日次集計として実session数と分離 / 追記transactionで90日より古い観測を整理し、集計はread-only / 台帳追記の障害は元の検索・書込結果を変えず劣化として示す |

| 18 | **提案チケットの内容・レビュー・本人の採否を版と履歴で管理する**(2026-09-06本人依頼) | 専用コアが`proposal_ticket`をノートdocumentへ保持し、通常ノートのauthority・保存状態と分離 / AI用write面はcreate_proposal・revise_proposal・review_proposal、readはget_proposalでレビュー対象の全文・etagと履歴を取得 / 採否はnative GUIだけに公開しMCP/CLIへ能力を出さない / 現在版のレビュー後に承認・否決、保留には次の行動、判断理由は任意（空欄でも保存可） / GUIの記録ボタンは確認モーダルを開き、提案・版・判断入力を表示し、そのetagに固定して確定時だけ保存。取消・Escapeは書き込まず、確認中の変更・二重送信を防ぐ / 変更は同じwrite transaction内でetag再照合し、過去版・レビュー・採否を保持 / 改訂は旧承認を引き継がない / 通常参照はDB派生列normal_reference_allowedで制御し、通常ノートと検証済み現在版がapprovedの提案だけを許可。未レビュー・判断待ち・保留・否決・壊れた提案は除外し、改訂・採否・importと同じtransactionで再導出 / 検索・recent・近傍・リンク展開・自動retrievalはSQLで件数制限前に除外し、anchorは両端を確認。通常書込後の判断支援候補・依存IDとタグ語彙の正本選択にも同じ可否を適用。通常getは可否と本文を同じsnapshotで検査し既知IDでも拒否 / GUIの提案管理とget_proposalは全状態を明示取得できる / 通常update・蒸留・削除による内容・履歴の上書きを拒否 / importは整合した履歴の追記とfresh復元だけを許可し、欠損・分岐を拒否 / DBとoutboxへatomicに確定しexport失敗は保存済み＋未出力として返す / 承認から作業・モデル・外部送信を自動実行しない |

| 19 | **ノート保存時の蒸留義務を残し、設定したAIでまとめて処理・失敗再試行・定期再確認する**（2026-09-06本人依頼） | schema10のnotes triggerで参照可能なAIノートの追加・内容変更と同じtransactionに版付きjobを登録 / trigger定義を起動時に照合 / 保護された提案票と旧humanは除外 / 世代・本文hash・期限付きlease・全体snapshotを反映直前に再照合 / 原recordを保持するnormalize・revise・extractと一つの正本新設・根拠リンクを検証し、意味変更・変更不要の確認記録・出力版の完了をatomicに確定 / 応答喪失は同じrun IDの結果を再取得 / 一時失敗はbackoff、バッチ不成立は単独再試行、単独でも処理対象外・意味判断保留は理由付きblocked / 常駐workerが設定AI・モデル・推論モードで関連する未処理ノートを上限付きバッチへまとめて処理し、設定間隔で確認済みを再登録 / GUIから未処理または確認済みを含む全体の即時実行を要求でき、保存済み自動蒸留ON・選択AI利用可を受付前に検査。コアが現在documentから対象を再検査し単一transactionで待機時刻を前倒し・保留を再登録、確認済みだけ新世代へ進める。原文・履歴・実行中leaseを保持し、受付と完了を区別 / KB OFF・設定変更・終了で停止し未処理を保持 / AI出力は未信頼JSON、本文はstdin、出力量・時間・実行権限を制限 / GUIとMCPへ版別状態・待ち・失敗を表示し、旧cadenceの構造受入を意味確認済みと扱わない / 正式モデル候補から対応する推論モードを選択し、明示モードの非対応を既定へ黙って置換しない / 詳細はADR-0021 |

| 20 | **ノートの来歴(誰が・いつ・どの部分を・なぜ)は本文と分離した追記専用イベントとして正本に含める**(2026-09-10追加) | `note_store`のput / queue_put / deleteを通る全書込で、コアが書込と同じtransactionへ1件のイベントを発行する(呼び出し面が省略できる引数にしない) / イベントは書き手(製品名・モデル・モデルの動作設定と、それを何から得たかのbasis)、operation、改版種別、変更のあった見出し、frontmatterの差分、本文のunified diff(上限超過は省略と明示)、前後のdocument hashを持ち、本文そのものを複製しない / 正本は`.kb-events/YYYY-MM.jsonl`への1行追記で、Markdown出力と同じcommitに含める / コアは既存行の改変・削除を行わない(訂正は新しいイベントを足す) / `.gitattributes`で`merge=union`を張り、端末間の追記を片方だけ残さない / Storage Contractのsnapshot digestに含める(イベントの無い保管庫のdigestは不変) / DBの`note_events`はdurableで、派生索引として再構築対象にしない — 消えたら`.kb-events`から復元する / 最新イベントと現在のdocumentの食い違いは件数として示し、verifyを失敗させない / 書き手の表示とfrontmatterの`generated.by`は、接続設定の製品名にモデルと動作設定を続けた`<製品名>/<モデル> <動作設定>`(例 `codex-cli/gpt-6-codex Astra medium`)で、モデル名だけの記録へ戻さない(2026-09-10本人指摘)。動作設定はMCPの`actor.mode`(自己申告)または`--client`の3番目のsegment(設定値)から得て、モデル無しでは持たない |

- 契約1の語彙一括変更は、AIが`plan_tag_vocabulary_change`で追加・説明変更・未使用語の削除・
  統合/改名の対応表と一行理由を指定し、`apply_tag_vocabulary_change`へ同じreceiptを渡して適用する。
  planはmaintenance面のread-only操作であり、本人の承認待ちではない。正本UID・指定revision・
  正本原文hash・全DBの原文snapshot・正規化した操作を固定し、適用時はwriter lockの中で再照合する。
  古いplan・改ざん・実行済みID・未出力の更新があれば書き込まない。ノートをAIへ全件往復させず、
  コアが全persisted notesの利用を列挙する。削除語が使用中なら拒否し、統合/改名では全利用を
  最終語彙へ付け替えて重複を除き1〜4タグを検証する。旧human・未管理origin・専用提案票など、
  変更できない利用ノートが一件でもあれば全体を止め、参照不可ノートのID・題名はpreviewへ出さない。
  正本以外の本文・UID・authority・relations・その他metadataを維持し、tagsとgeneratedだけを変更する。
  正本も語彙節の一つの表以外の運用本文を保ち、複数表・重複行など曖昧な表を自動破壊しない。
  全ノート・索引・蒸留job・durable outbox・理由と前後原文の履歴を一つのtransactionへ確定し、
  途中失敗は全rollback。MCPは実行ID・件数・保存結果のrequired eventを返し、個々の通常
  updateとして観測台帳へ水増ししない。出力は一括変更ごとに索引生成・log追記・Git commitをまとめ、途中中断は
  before/afterを照合して再開する。出力失敗は保存済み・出力待ちとして返し、待ち件数を
  確認できない場合はnullと警告を返す。
  履歴2表は端末ローカルのdurable stateで、schema12から必須。原文はDB内だけに保持し、
  read面の`list_tag_vocabulary_changes`/`get_tag_vocabulary_change`は既定20・最大100件の
  ページ化した要約とタグ変更metadataだけを返し、現在の非参照ノートを除外する。
  previewは最大20例、1操作は最大1000語・10万対象ノート・対象原文256 MiBまでとし、超過時は
  部分適用しない。
  第4段階の`plan_tag_vocabulary_rollback`はmaintenance面で元実行IDと理由を受け、同じworkspaceの
  履歴と現在の正本UID/revision・全snapshotへreceiptを固定する。write面の
  `rollback_tag_vocabulary_change`はwriter lock下で再照合し、対象が元実行のafter原文・UID・
  タグ索引・参照属性と一致するときだけbefore原文へ全件復元する。対象の変更・改名・削除、
  正本再指定、保護対象、復元済み記録、未出力更新、対象外に残る削除語の利用をblockerとする。
  復元前後原文の合計256 MiB・10万対象ノート・最大20例を上限とし、変更前のタグが現在契約や
  復元後語彙に違反する場合も拒否する。過去の不正状態を復元しない。
  ノート・索引・蒸留job・outboxとschema13の`tag_vocabulary_rollbacks`を同じtransactionへ確定し、
  元実行と前後原文は不変に保つ。復元の理由・client・時刻・件数は別台帳へ残し、元実行への一意制約で
  二重復元を拒否する。apply IDとrollback IDの衝突も拒否する。復元結果はrequired eventの
  `tag_vocabulary_rolled_back`で届け、通常updateの観測件数へ加算しない。
  出力は一括applyと同じ再開経路を使い、出力失敗は保存済みと返す。履歴要約は`rollback`を併記し、
  応答喪失時は元実行IDから確認する。専用GUI・復元操作の再rollback・別端末やfresh cloneへ
  自動復元用台帳を引き継ぐ機能は公開しない。
  read面の`get_tag_vocabulary_stats`は同じ読取snapshotで端末ローカルの保存済み全期間を集計し、
  apply/rollback件数、正本を含む各延べ変更ノート件数、最初と最後の時刻、最近10実行、
  ノート出力待ち総数と語彙操作分を返す。原文・操作JSON・理由の全走査、同期・修復、追加ログは行わない。
  計画回数・拒否試行・処理時間・意味判断品質は未計測と明示し、成功率や誤判断率へ読み替えない。
  詳細は[ADR-0022](adr/0022-atomic-tag-vocabulary.md)。（2026-09-08本人依頼の第3・第4段階）

- 契約19の保存と蒸留は二段階に分ける（2026-09-07本人採用）。保存時は既存の形式・タグ・
  authority・参照先整合性の検証と判断支援を維持し、専用AIをノートごとに追加起動しない。
  Inboxは同じ保存transactionで残す未蒸留jobの論理的な待機集合であり、承認待ちや別の正本ではない。
  候補検索やリンクの存在だけではその版を確認済みにしない。少量の追加にも最大待機時間を設け、
  手動の即時要求は関連ノートを集める待ちを省略し、実行中leaseを変更しない。
- バッチは全sourceの全文と版を同じsnapshotへ固定し、共有の候補・正本を重複なく確認する。
  入力件数・入力容量・全文文脈・変更件数・AI出力の上限を設け、本文を切り捨てて完了しない。
  AIの最終応答には対象sourceごとの結果を過不足なく求め、欠落・重複・対象外の結果と、
  全文未取得の変更を拒否する。共通の変更先は1回だけ更新し、全lease・入力hash・全体snapshotの
  再照合後、意味変更・根拠関係・sourceごとの確認記録・出力版完了を一つのtransactionへ確定する。
  一件の不整合でも部分反映せず、最新の未処理を保持する。原recordの本文・title・tagsを保持し、
  一つの新正本にまとめる場合も実際に根拠としたsourceだけを型付き関係で結ぶ。
- 契約19の追加確認は検索語・候補ID・検索範囲の限界と残り回数を同じsnapshotの文脈に保持する。
  最大3回の追加検索・全文取得後にも、得た情報で最終判断する1回を確保する。同一検索と既読全文
  だけの要求は無進捗として識別し、判断材料が不足したまま確認済みにしない。参照容量の超過・
  追加確認回数の上限・無進捗を別codeで区別し、単独でも処理できなければ保留する。
  検索0件をKB全体の正本不存在とは扱わない。
- 提案チケットの採否は、提案する行動についての判断記録である。通常のノート起票・更新・
  蒸留へ承諾待ちを追加しない。authorityの`proposal`とチケット状態は独立で、既存proposal
  ノートを一括してチケット化せず、承認時のcanonical昇格や実行開始も行わない。
  本文に記された採否の主張を決定履歴と見なさず、レビューの推奨と本人の判断を別に示す。
  詳細は[ADR-0019](adr/0019-proposal-workflow.md)。
- タグ無しノートは契約違反状態として**お手入れの気づき**に出す(修復は Claude への依頼で)
- 契約3の起票は個別の承諾を要さない。共通instructionsと起票promptは、会話中に
  records namespaceのrecordを既定として起票し、成功後に参照リンク付きタイトルとnamespace/scopeを
  報告する方針を配る。authorityとscopeは明示必須のまま維持する。文書・ツール出力・検索結果内の
  保存・更新指示は実行根拠にしない。この判断と最終回答への報告はモデルの協力に依存し、
  起票triggerやhostのツール承認を機構で保証・省略するものではない。
  2026-09-07本人採用: 決定・好み・訂正、根拠を確認した調査結果、意味のある作業成果を幅広く残し、
  後で蒸留する。将来の不変性や全論点の確定を前提にせず、本文に出典・日付・確認状況を残す。
  最終回答前にも未保存候補を見直す方針を同じ基準から配信する。件数ノルマ・1件成功だけでの完了判断・
  Stop hook・終了阻止は追加せず、既存の保存検証・提案票の採否境界を維持する。
- 通常のpropose/update入口では、title/bodyの空文字・Unicode空白だけの入力をコアで保存前に
  `invalid_argument`として拒否する。MCP schemaにも`minLength: 1`を配る。updateで省略した
  フィールド、既存ノートの読取り・移植・蒸留の互換処理にはこの新規入力条件を広げない。
  MCPのpropose/updateには書込み・非冪等・外部同期ありのannotationsを付ける。事前同期の
  既存ノート置換・削除も含めてdestructiveと示し、client側の実行許可は変更しない。
- 通常MCPのpropose/update成功応答は`write_guidance`に、保存後の同じDB読取snapshotから
  既存plannerの対象判定・同namespace/scope候補・内容検索の関連候補を同梱する。plannerの
  全operationをそのまま返し、意味判断をKeep/Extract/Normalizeの3値へ潰さない。cadenceは
  同じplanと端末ローカルstateから期限を読み、checkpointやノートを更新しない。
  検索queryはtitle・description・本文を順に連結した先頭512 Unicode scalar、OR検索とし、
  保存したノート自身は多様化の前に除く。関連候補と同scope候補は各最大5件で、全文確認・
  関連設定・蒸留は呼出し側の判断に残す。scope候補の省略とquery切詰めは明示する。
  updateは保存形式と同じ前後空白の正規化（先頭改行・末尾空白を除く）後の本文Unicode scalar数が
  50%以上減少した場合と、typed relation数が
  減少した場合を警告する。これは保存後の判断材料であり、書込拒否・自動復元・同時更新の排他保証ではない。
  判断支援の取得失敗は`write_guidance.degraded`・応答の`degraded`・textへ明示し、成功済み書込を
  エラーへ戻さない。補助情報の取得を理由にproposeを再試行させない。
  active canonicalの同namespace/scope拒否は既存のUNIQUE制約を維持し、拒否を確定したtransaction内の
  競合note ID・UID・title・namespace・scopeを`scope_conflict`として返す。本文は返さない。
- 契約8の既存UserPromptSubmit hookに、命令文を持たないKB状態行を追加する（2026-09-05）。
  状態行は端末設定`harvest_status_line`（既定ON）と`KB_APP_HARVEST=off`で停止でき、KB OFFが最優先。
  通常MCP initializeは`kbApp.harvest.status_line`と`disabled_reason`をVault・台帳を開かず返す。
  `--hook-context`で起動した子MCPのsearch(include_documents)だけに`cadence_digest`を同梱し、
  この補完による通常read面のtool・引数・権限は増やさない。tool引数ではhook専用情報を有効化できない。
  cadenceのcheckpoint IDとplanner導出のlineageなしrecord件数を任意の派生artifactとしてDBへcacheする。
  notesとnote_relationsのINSERT/UPDATE/DELETE triggerが同じtransactionで`notes_revision`を進め、
  import・GUI・同期・executor・旧binaryの更新も失効させる。cacheは同じread snapshotで計算し、
  snapshot終了後もrevisionと再構築generationが一致するときだけ発行する。trigger欠損・不正は
  registryの修復でcacheごと無効化し、schema 8のdurable stateやcheckpoint受入条件を変えない。
  書込後の既存planner結果と通常searchがcacheを温め、hook子MCPはcache missから全件plannerを
  起動しない。未準備・更新待ち・取得失敗は未確認と表示し、古い値を現在の事実として返さない。
  期限・受入state・前回失敗は現在時刻で毎回再評価し、観測時刻だけの変化はdigestから除く。
  状態行の件数はこのworkspace・client surfaceの直近14日・今回出力前の台帳集計で、受信成功や
  この会話だけの件数を意味しない。cadence詳細はworkspace×surface×sessionの初回とdigest変化時に
  出力する。session不明はUTC日で分ける。stdout write/flush成功かつ状態行が実際に出た後だけ
  出力履歴を記録し、履歴は端末ローカル・IDはhash・最大512件とする。失敗や予算省略では既読にしない。
  状態行も既存の全stdout予算・文書境界・劣化優先・台帳計測に含め、入らない場合は省略を明示する。
  子MCPのblocking stdout readは専用threadへ分け、起動後20秒の応答期限で子processを回収する。
  これはhost受信や起票判断を保証する仕組みではなく、追加trigger・蒸留gate変更は含まない。
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
- 起動時のschema不整合を空の保管庫として扱わない。schema10より古い宣言にschema10の蒸留台帳が
  残っているDB、schema11より古い宣言に語彙正本の台帳が残っているDB、schema12より
  古い宣言に語彙変更履歴が残っているDB、またはschema13より古い宣言に語彙復元履歴が残っているDBは、
  journal mode変更・migration前に拒否する。診断用の`inspect_runtime_storage`は
  現在の接続先だけをread-only/query-onlyで観測し、通常open・migration・索引修復・import・
  export・同期・復元を呼ばない。MCPではmaintenance面とKB ON・接続先照合を通常どおり強制し、
  任意path・SQL・本文取得の引数を持たない。件数不明・読取失敗は0件へ変換せず、未確認値と
  診断codeで返す。GUIの初期読込・取得失敗・正常0件を区別し、失敗時も診断結果をコピーできる。
  診断はデータ保全や復旧の完了を意味しない。（2026-09-07障害対応）
- 復元元の照合`plan_runtime_recovery`も同じ読取専用・接続先・公開面の境界を守る。
  schema7/9宣言・runtime marker欠落・空のnotes/outbox・残存するschema10台帳という
  限定した障害形状を調べ、DBのschema/meta/全永続行とMarkdown全件の原文をdigestへ固定する。
  DBの単一読取snapshotとMarkdownの変更検出は、DBとファイルをまたぐ原子的backupではない。
  Markdownが存在すること、台帳に対象があること、過去の版に一致することだけで最新版と認定しない。
  蒸留完了時の照合hash、履歴の変更後document、参照に使った変更前documentを分け、
  根拠不足・不一致・蒸留対象なのにjobがない候補・読取失敗を明示する。候補全体のUID・authority・
  参照関係も検証する。本文・絶対path・SQLを返さず、詳細の上限と総数を区別する。
  この計画はデータを変更せず、復元の適用口や実行許可を発行しない。（2026-09-07障害対応）
- 現存Markdownの限定復旧は、本人が起動する`--storage-recovery`専用画面だけに公開する。
  MCP・通常GUI・CLIには適用口を持たず、専用起動は通常のDB初期化・同期・蒸留・常駐を開始しない。
  起動時にAIのKB利用をmaster OFFへ変更し、照合・適用時にも停止を確認する。旧writerの終了は
  別途必要であり、この設定だけで受付済み処理が止まったとは扱わない。画面で初回に照合した
  Vaultの実体pathと永続IDを固定し、再照合なしの適用や結果不明時の同一要求の再使用を拒否する。
  コアは既知の障害形状・完全な復元元・停止中のjob・全永続行と本文の計画digestを再検査し、
  最新版を証明できない本文を含む場合は本人の明示確認を必須にする。履歴だけにあるノートを復活させない。
  対応するUnix環境でDBと親directoryの同一性を固定し、差し替えも検出する。writer排他の下で、
  WALを含むDBの整合したコピー・Markdown原文・照合manifestを製品管理領域へ
  作成し、退避内容を検証してから元DBを変更する。notesと派生索引だけを単一transactionで復元し、
  既存のexport・蒸留・action台帳は全行hashで保持を確認する。語彙正本の指定・出力待ち2表が
  残る場合も全行hashへ固定して保持し、指定先が欠損していても指定を消さない。旧版で両表が
  未導入なら空表を追加し、片欠損・破損は拒否する。語彙一括変更の2台帳も全行hashで保持し、
  復旧時は履歴の前後原文hashと件数を検査する。schema13の復元台帳も全行hashで保持し、
  元実行との参照・復元件数を照合する。原文・UID・authority・参照関係と
  schemaを検証してからcommitし、途中失敗はrollbackする。反映中の通常終了は拒否する。
  復旧完了は退避ID・復元件数・未証明件数・台帳保持の受領結果で示し、AI利用・自動処理・同期は
  再開しない。正常GUIの再起動と接続の更新は別の操作とする。（2026-09-07障害対応）
- 契約7で Vault の同一性を repository 名・URL・ローカル path から推測しない。それらは変更可能で
  端末ごとにも異なるため、Git で運ばれる `.kb-workspace` の永続 ID だけを使う。新しい端末では
  既存 repository を clone して Storage Contract を検査し、`full` の全 LFS object を取得・hash
  照合してから復元完了とする。接続後に repository が public 等へ変更された場合、以後の upload は
  止めて重大な劣化として表示する(既に公開されたデータをアプリが取り消せるとは扱わない)
- macOSの配布アプリはGit本体・Git LFS・HTTPS helperを同梱し、復元・同期で開発ツールの
  導入を要求しない。コアの外部実行入口で同梱Gitを絶対pathで選び、Git/LFSの子processも
  同梱PATH・exec-path・templateを使う。配布内の実行ファイル・必須HTTPS helperの欠損や
  非通常ファイルは起動前に拒否し、環境の別版Gitへ暗黙fallbackしない。開発用CLIと明示的な
  `KB_GIT_BIN` / `KB_GIT_LFS_BIN`の受入overrideは別経路として保持する。実行名は
  `git` / `git-lfs`（Windowsでは`.exe`付き）に限定し、PATHに表せない配置や再帰Gitの
  競合、実行権限の欠落は起動前に拒否する。
  公式source/archiveのchecksum、CPU、OS下限、署名・公証、同梱licenseの配布検査と、
  別Macで最初の保存・検索へ到達する実機受入を混同しない。
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
- 契約8のhook出力は、検索側の推定token予算とは別に`kb-core`で全体の長さを制限する。
  Claude Codeは9,000 UTF-16 code unit、Codexは実稼働hostの版と対応する仕様を検証できるまで
  9,600 UTF-8 byteの保守的な予算を使う。未知surfaceの整形も同じbyte予算とし、MCPの拒否は維持する。
  前置き・統計・劣化・本文ラベル・候補案内・末尾改行まで予算に含め、本文は順位順の先頭から
  文書単位で採用し、途中切断や短い下位文書での埋め戻しをしない。省略数と実際の出力文字数・byte数を
  統計に載せ、劣化は統計直後・本文より前に出す。長い警告や候補案内の省略も明示する。
  この検査が保証するのはhookが出力する長さであり、hostが全文をモデルへ届けたことではない。
  通常出力と劣化通知はMarkdown見出しから始め、JSONに見える`[`や`{`を先頭にしない。
  2026-09-05にCodex 0.153.3が`[`始まりの通知を不正なJSONとして拒否したためで、
  plain stdoutの形式を維持したまま先頭を区別し、見出しの文字も全体の予算へ含める。
- 契約8の接続通知は`kb-core::client_notice`の既知codeと固定文だけで構成する。
  ユーザーの明示OFFはguard判定より優先し、通知も検索・台帳I/Oも行わない。明示ONのcoding面で
  対応する管理保護設定が`Outdated`なら、MCPのinitializeと拒否結果に`guard_outdated`を付ける。
  `kb_enabled=false`および`kb_disabled`の終端条件は維持し、hookは固定通知1行だけを出す。
  Missing・Conflict・未知surfaceや設定読取失敗をOutdatedと推測せず、別surfaceの状態も混ぜない。
  有効なcoding面のinitializeでは`host_capability_unverified`を示し、通常hookの出力にも1行含める。
  管理ポリシーの一致・PATH上の版・子MCPの版・単発の受信確認からhost能力を検証済みにせず、
  通知も全stdout予算と実測へ含める。通常チャット面や明示OFFにはhost通知を出さない。
- 契約8の共通運用診断は、登録・出力・受信・操作応答を分ける（2026-09-08）。
  Codex・Claude Code・Claude Desktopの通常MCP登録は、コアでread/write/maintenance各面の
  実行ファイル・Vault名・client引数を検査する。旧登録・別Vault・無効化・設定破損・読取失敗・
  管理ポリシーとの競合を区別し、破損・解釈不能なポリシー・別scopeの上書きを自動修復しない。
  修復はバックアップを残し、無関係な設定を保持して原子的に置換する。登録一致は再接続や実動作の証拠ではない。
  initializeは契約SHA-256と実際のinstructions SHA-256、client/tool surface、kb-app版を返す。
  OFF時はOFF案内を識別し、通常initializeでVaultや端末観測台帳を開かない。
  ONのsearch/get/tag_vocabularyは同一DB snapshotのworkspace ID・語彙正本の状態・UID・指定revision・
  正本document SHA-256を添える。指定revisionを本文の版として使わず、読取の診断不能を一致扱いにしない。
  hook台帳は子MCP検索で確認した識別情報を出力準備へ付け、stdout成功・失敗を既存receiptで確定する。
  最新の出力に版がない場合、古い成功で補わない。子MCP検索の版一致はhostへのinitialize案内の配信、
  hostの全文受信、モデルの規則遵守を証明しない。GUIの操作件数は過去30日の応答観測であり、
  同じ規則版での全操作受入とは扱わない。観測台帳が読めない場合はゼロ件ではなく未確認とする。
- 契約8の接続先照合は、GUIでの完全保護設定・各対応clientのMCP登録時に固定した
  client surfaceごとのVault名・永続IDを期待値とする。hookの子MCPは固定したVault名を選び、
  通常MCPは登録済みの選択を維持する。コアが同期・DB操作前とpull後に保存済みIDを照合し、
  不一致は本文・pathを含まない`vault_mismatch`、設定破損やID確認不能は`workspace_unverified`で
  停止する。hookは期待値の未設定でも本文を配信しない。通常MCPの未設定だけは旧接続との互換を
  保ち、未検証を明示する。一致を装って既定Vaultへ切り替えたり期待値を自動更新したりしない。
  OFF・通常initialize・tools/listでは接続先ID設定もVaultも読まない。GUIの明示的な設定操作は別経路とする。
  開始計測用のinitializeだけは、明示ONのClaude Code・read面・`hook_context`・必須client bindingに
  `kb_app_session_observation=true`を組み合わせた場合、登録先の`.kb-workspace`メタデータだけを
  コアで読み、固定IDと照合する（2026-09-06 本人採用）。本文・索引を開かず、検索・同期は行わない。
  一致時だけ`kbApp.session_observation_binding={verified:true,workspace_id}`を返し、未設定・不一致・
  確認不能では開始記録を読まず、書かない。この補完では通常readのtoolや通常initializeの境界を変えない。
- 契約9の`proposal`は**人間の承認待ち状態ではない**。AIが蒸留中に正本候補と既存canonicalを
  区別するためのauthority roleである。2026-09-06本人依頼の提案チケット（契約18）は、
  専用の版・レビュー・採否履歴で進行状態を導出する。通常ノートや蒸留の承諾待ちへ広げない。
- 契約9の判断・行動記録（2026-09-08本人採用、Issue #146）は、任意の`judgment`構造として
  正本documentに保持する。決定の出所（本人決定・本人訂正・AI推測）と、行動の状況・結果・
  観測の出所を別にし、出典referenceと抜粋、適用条件・行動・例外を型と長さ上限で検証する。
  参照する決定UIDは既存typed relationにも要求し、参照先の存在と削除保護を既存機構で守る。
  保存形式を数値の重要度、本人認証、実行許可へ変換しない。既存ノートへの付与は任意で、
  自然文から過去の本人決定や出典を自動補完しない。通常updateの省略は保持、null指定は削除とする。
  getと本文同梱searchは同じDB snapshotから、出典・優先理由・適用未確認・矛盾・後継を返す。
  scopeは明示された今回の範囲と完全一致またはslash境界の包含だけを照合し、自然文の条件充足は
  判定しない。無指定は未確認とし、失効・無関係・推測を現在の本人決定と同一視しない。
  同じ出典イベントの重複と行動件数は規範の優先度を上げず、本人訂正も自動的な後継認定にしない。
  hookは条件・例外・参照を一体で予算内に提示し、省略件数を示す。無効化・非公開提案票・本文なし
  profileの境界は維持し、観測台帳へ判断本文を追加しない。モデルがどの行動を選ぶかの評価は
  保存・配信の決定的な検証と区別する。設計と評価方法は[判断根拠の配信](judgment-evidence.md)を参照。
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
- 契約17の`emitted`はhostの受信・表示・モデル利用を証明しない。`prepared`のまま残った観測を
  出力成功に数えず、MCPの`error`からノート未保存を推定しない。台帳を使う
  `kb sessions`はVaultを開かず、DB不在でも作成しない。保持整理は追記時にだけ行い、
  定時削除や自動起票の発火を伴わない。詳細は[ADR-0018](adr/0018-session-observation-ledger.md)。
- 契約17の固定期間集計は`observation_summary`としてmaintenance面だけへ公開し、接続先照合済みの
  現在workspaceへ限定する。read/write面の直接call・KB OFFは台帳を読む前に拒否する。
  CLIの`observation-summary`と同じread-only集計を使い、生台帳・会話hash・任意pathを公開しない。
  通常/診断/不明、実効状態行ON/OFF・環境上書き、出力包含・省略、成功updateの型付き削減警告を
  機械観測として保持し、旧eventの不明値を通常利用や警告なしへ補完しない。
  Claudeの書込session率は、開始確認済みで単一区間/設定・既知permissionに属する通常UPS観測
  3回以上の同一session集合を分母とし、その集合のpropose/update成功sessionだけを分子にする。
  新規eventの開始情報は、同じ登録workspaceとhost由来実session IDに対応する有効なSessionStart
  証拠を条件にし、台帳初出・最初のUPS/write・actorから推測しない。起動補助の時刻は補助情報に留める。
  `session_started_at_ms`は`host_start_event`由来の開始イベント観測時刻であり、host内部の厳密な
  会話誕生時刻ではない。期間境界もこの観測時刻に基づき、旧eventの起動補助時刻とは出所別に示す。
  CodexのID不在writeは件数のみとし、診断・設定混在・境界跨ぎ・不明の除外と計測の欠落を別掲する。
  数値はR3やenforceを自動発火させず、本文読了・起票漏れ・全会話への捕捉率を保証しない。
- 契約17のClaude開始計測は、管理`SessionStart` hookの`--hook-session-start`で行う
  （2026-09-06 本人採用）。ONと登録先の照合後に、匿名session ID・観測時刻・世代を端末ローカルに保持する。
  有効な`startup`はworkspaceごとに最大1つとし、別IDの`startup`または`resume` / `clear` / `compact` /
  `fork` / 未知sourceを観測したら、それ以前の証拠を失効させる。同じ有効startupの重複は元の時刻を保ち、
  失効IDは保持中の再通知で復活させない。匿名session IDと開始証拠は失効記録を含め90日保持し、
  workspace単位の世代・時刻の境界値は再有効化を防ぐ制御状態として残す。生IDは保持せず、
  永久のsession ID再利用検出は保証しない。
  MCPは書込前後で同じ証拠・世代を照合し、UPSもpayloadの実IDで照合する。DB不在・破損・照合失敗は
  開始未確認とし、launcher情報へのfallbackや過去eventの補完をしない。計測失敗で元の検索・書込を止めない。
  SessionStartはUPS件数へ加えず、本文・prompt・生IDを保存しない。開始記録の読取はread-onlyとする。
  runtimeの所有関係を推測しないため、並行会話は過剰に除外され得る。後続hookの欠落、DB障害・ロック競合で
  失効を保存できなかった場合、OFF中・登録先切替中の遷移は検出を保証できない。`write_linkage_verified`は
  保存できたhook/writeの証拠の一致を示し、古いMCP IDの完全な現在性や全会話の捕捉を保証しない。
- 契約17の書込拒否codeは、既存の検証に由来し、書込み前の拒否と確定できる型だけを
  `WriteRejection`としてMCPと台帳へ渡す。診断文から分類せず、分類できないerrorは未分類のまま残す。
  Homeの観測表示は選択中workspaceと未帰属分を分け、直近14日の件数と最大90日内の最終起票成功応答を
  read-onlyで読む。全体OFFまたは両AI familyがOFFなら台帳I/Oを行わず、部分OFFでは設定と過去記録を
  区別する。未観測・OFF・取得不能を「正常な0件」へ潰さず、受信率・起票率・起票漏れを推定しない。
- 契約20の来歴は承認キューでも本文の一部でもない。frontmatterの`generated`は「最後の書き手」のまま変えず、履歴をノート本文へ書き戻さない。書き手の申告(改版種別・要約・理由・根拠)は空でも書込を止めない — 強制すると経路が迂回されるため、記録の欠落として残す。モデル名は`--client`の位置に依存する文字列を再解析して埋めず、確認できない間は不明のままにする。詳細は[ADR-0023](adr/0023-note-provenance-events.md)。
- 契約の変更はこの文書の改定+コアの強制点の変更として行う(instructions だけの変更は不可)
- **語彙の内容はノートの`## 語彙`節、参照先はworkspace_idとnote_uidの明示指定で固定する**。
  `tag_vocabulary`は未指定・指定済み・欠損・通常参照不可を区別し、現在版と候補を返す。
  題名検索は候補発見だけに使い、候補が1件でも読取では指定しない。AIが`get`で確認し、
  `set_tag_vocabulary_source(workspace_id, note_uid, expected_revision, reason)`で指定する。
  初回は明示null、変更は現在版の一致を必須とし、版は毎回新しくして古い指定の再利用を拒否する。
  UIDがない旧ノートはauthorityと対の移行を先に行う。個別の本人合意は必要ない。
  未指定かつ候補0件だけ現用語からbootstrapする。候補があれば指定までタグ指定を拒否し、
  固定後の欠損・参照不可から別候補や現用タグへ切り替えない。空表は登録語0件として強制する。
  読取失敗を空語彙に変換しない。通常getとタグを省略する本文更新は、語彙の未指定・指定先欠損だけを理由には止めない。workspace不一致や指定JSON破損は共通のDB準備で拒否する。
  指定とoutboxはSQLite transactionで確定し、追跡対象`.kb-tag-vocabulary.json`へ出力する。
  出力失敗は保存済み・未出力として表示し、後続の通常同期・書込みで再試行する。未出力のまま次の指定を重ねず、
  指定中と未出力の旧指定ノートの削除・参照不可化を同じwrite transactionで拒否する。
  fresh clone・明示importはノートと指定を同じtransactionへ復元する。指定データの不正・
  workspace不一致、既存指定を追跡ファイルの削除だけで解除する操作は拒否する。
  論理snapshotは指定ありをv2としてUID・版をdigestへ含め、未指定はv1の形式とdigestを維持する。
  運用本文は通常updateで管理する。新語の必要性や統合・削除の意味はAIが判断し、語彙表の変更は
  plan/applyで利用ノートと一括確定する。未知の語を通常updateのtagsへ渡すと拒否する。
  旧human・保護された提案票の境界は維持する。使用中の旧語を本文update・指定切替だけで削除せず、
  一括変更・復元・履歴・運用集計の条件は契約1の補足に従う。
  詳細は[タグ語彙の更新手順](tag-vocabulary.md)。

### 契約に**入れない**もの(2026-08-11 本人決定)

**通常ノートに「下書き(draft)」の保存状態を持たない。** 暫定・要確認などの扱いは
タグで表し、通常ノートの保存と蒸留に個別の承認待ちを追加しない。
2026-09-06に本人が実装を指定した提案チケットは、提案する行動をレビューして採否を記録する
専用用途として契約18に定義する。提案一覧の状態filterと採否ボタンはこの用途に限定する。

## 設計原則(2026-08-10 本人決定)

**確定すべき挙動は AI の理解力に期待せず、機構で必然にする。** 文章(instructions)に
残してよいのは、裁量が本質の挙動(何を検索するか・何を残す価値と見るか等)だけ。
強制の手段は強い順に: 能力の不在(ツール非公開)> スキーマ・コア検証 > フックの
事前/事後ブロック(Claude Code)> ツール応答・エラーでのその場教育 > 常駐文章。
新しい規律を足すときは、まずこの階段の上から検討する。

## Artifact昇格後の構造監査（2026-09-08、Issue #88）

StorageReportの`legacy_files`は旧実体の物理総数として維持し、未昇格Artifact数と、
現役・保持専用・未分類の物理path数を`legacy_inventory`で区別する。共有pathは一度だけ数え、
現在のLegacyGit locatorによる利用を優先する。保持判定は最新の構造化promotion証拠と
現在のManaged manifest・ref/alias・旧path/hash/sizeへ照合し、旧散文や同hashだけで推定しない。
構造化証拠の破損、保持を記録した旧実体の欠損・改変は監査失敗とする。未分類は移行完了へ数えない。
旧実体・LFSの削除や本文の自動書換えは行わない。台帳識別値の軽量確認でpayloadを再hashせず、
実体照合はStorage Contract監査時に行う。詳細は[Artifact監査](artifact-audit.md)。
