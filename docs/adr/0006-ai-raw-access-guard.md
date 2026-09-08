# ADR-0006: AI の生ファイルアクセスを管理 OS sandbox で閉じる

- 日付: 2026-08-17
- 状態: 採用
- 関連: [contract.md](../contract.md) 契約8 / [requirements.md](../requirements.md) FR-A9

## 背景

MCP の `tools/list` と `tools/call` を OFF にしても、Codex と Claude Code は汎用 shell と
組み込み file tool を持つ。同じ OS user で動くため、instructions に「直読みしない」と書くだけでは
比較条件を機構で確定できない。設定ファイルだけを閉じても Vault の Markdown は読め、Vault だけを
閉じても設定を ON に書き換えられる。

## 決定

strict modeでは、生ファイルの拒否は ON/OFF と分離して常時有効にする。ON/OFF が変えるのは kb-app MCPが
データを返すかどうかだけで、接続とtool schemaは残し、Vault の OS sandbox deny は緩めない。
自動retrievalのlifecycle hookもKB内容へ直接触れず、毎回kb-app MCPを子起動して現在のON/OFFを
tools/callの構造化終端結果で確認する。

- Codex 0.138.0 以降: `/etc/codex/requirements.toml` に管理 custom permission profile を定義し、
  `allowed_permission_profiles` を kb-app の read-only / workspace profile だけに限定する。両 profile は
  `:read-only` / `:workspace` を継承し、保護 path を `deny` にする。`:danger-full-access` は選択肢に
  含めない。管理 `permissions.filesystem.deny_read` も同じ path へ重ね、full-access 指定を
  requirements の段階で拒否する。legacy sandbox mode も read-only / workspace-write だけに限定する。
- Claude Code: system-level `managed-settings.d` に `sandbox.enabled`、
  `failIfUnavailable`、`allowUnsandboxedCommands: false`、`allowManagedReadPathsOnly` を置く。
  `denyRead` / `denyWrite` は shell と子 process に、管理 `permissions.deny` は組み込み Read / Edit に
  適用する。`bypassPermissions` も管理設定で無効にする。
- Codex / Claude Code: 管理 `UserPromptSubmit` hookから同じkb-app実行ファイルの
  `--hook-auto-retrieve`を呼ぶ。hookは同じ実行ファイルをMCP serverとして子起動し、
  `initialize → search(any, include_documents)`だけをJSON-RPCで実行する。検索seed、最大2ホップの
  リンク候補、予算内本文を同じSQLite snapshotから返すことで候補ごとの再同期・Markdown再読を行わない。
  これによりモデルの自発性と
  クライアント別instructionsの差を検索開始条件から外す。OFFでもinitializeは同じtoolsを公開し、
  searchが`kb_disabled`（権威あり・再試行不可・空data）を返したときは無音で終了する（下記の古いguard通知だけ例外）。それ以外の
  検索失敗は該当なしへ変換せず劣化コンテキストとして返す。
- Codexのローカル開発に限り、明示的な「開発高速モード」を設ける。strict guardが完全一致かつ
  root管理で有効な状態からのみ、管理者認証でCodex requirementsを`:danger-full-access`へ切り替え、
  `allowed_approval_policies`を`never`だけに固定して承認待ちを無くす。
  同じ状態検査でCodexを`development`と判定し、Codexのkb-app MCPと自動retrievalはfail-closedにする。
  Claude Codeのmanaged sandboxは変更しない。設定UIとCLIは警告を表示し、strict modeへ戻す操作を置く。
  `kb settings dev-mode release-check`はstrict modeでなければ失敗し、検証・テスト・配布の前提にする。
- 拒否対象は登録 Vault、各 canonical path、既定の `~/kb`、kb-app の端末設定 directory とする。
- ポリシー内容を毎回再生成して完全一致で検査する。未導入・登録 Vault 追加による古さ・競合・
  非対応のどれでも MCP は fail-closed。policy file から root までの所有者と mode も検査する。
  UI も保護完了まで switch を操作させない。
- macOS は設定 Modal から AppleScript の標準管理者認証を出し、root 管理領域へ固定ファイルを置く。
  既存の Codex requirements が kb-app 所有でなければ上書きせず conflict とする。Claude Code は
  公式の drop-in directory に kb-app 専用ファイルを置き、他の管理設定と分離する。
- 旧Claude Codeユーザー設定のPython前出し／Stop hookは、managed hook導入後の二重検索を避けるため
  他のhookを温存して削除する。互換ラッパーもCLI検索をせず、新しいMCP自動retrievalへ転送する。

## 理由

Codex の permission profile は macOS Seatbelt で、spawn した command にも同じ filesystem deny を
継承する。管理 requirements は user config や CLI override で解除できない。Claude Code の sandbox
も macOS Seatbelt / Linux bubblewrap を使い、管理 settings は user / project / CLI より優先される。
したがって `cat`、Python、別名 CLI、子 process の選択に依存せず、能力の不在として強制できる。

OFF時にもtool schemaを残すのは、toolの不在を接続障害と区別し、唯一の正規取次口から権威ある
利用不可状態を返すためである。構造化終端結果はエージェントへ再試行不可を伝える制御プレーンで、
個人データ、件数、ID、存在判定を一切含まない。実際の迂回不能性は引き続きOS sandboxが担う。

開発高速モードではCodexのOS sandbox境界を意図的に緩めるため、KB brokerを同時に停止して通常の
「KBを利用するcoding agent」と区別する。一般のKB ON/OFFやエラーからは遷移せず、strict状態からの
管理者操作だけに限定する。これにより日常開発の承認待ちは減らせるが、検証・配布条件まで緩めない。

- Codex Permissions: https://learn.chatgpt.com/docs/permissions
- Codex Managed configuration: https://learn.chatgpt.com/docs/enterprise/managed-configuration
- Claude Code Sandboxing: https://code.claude.com/docs/en/sandboxing
- Claude Code Settings: https://code.claude.com/docs/en/settings

## 却下した案

- **instructions のみ**: モデルの理解と追従に依存し、今回の比較目的を満たさない。
- **OFF時にtoolsを非公開**: 接続障害と区別できず、汎用toolから代替経路を探す誘因を残す。
  schemaだけを公開し、全callを同一の終端結果へ収束させる方が正規取次口を明確にできる。
- **KBのON/OFFのたびに policy を差し替える**: 更新途中の窓、管理者認証の反復、ON 時の直読みを残す。
  開発高速モードは日常のON/OFFから独立した明示状態であり、CodexのKB broker停止とrelease gateを伴うため
  この案とは異なる。
- **Vault の Unix mode / ACL を変更する**: 管理アプリと AI client が同じ OS user なので区別できない。
- **アプリ独自の常駐 broker user へ全 storage を移す**: client sandbox より強いが、既存の Markdown / Git
  storage contract と配布・復旧手順を全面変更する。公式 client の管理 sandbox で同じ境界を作れる
  現段階では採らない。

## 限界

### 自動retrievalの出力予算（2026-09-05）

検索の推定token予算だけでは、hookを受け取るhostの出力上限を守れない。
`kb-core`の整形処理で最終stdout全体に単位付きの予算を適用する。
Claude Codeは9,000 UTF-16 code unit、Codexと不明なsurfaceは9,600 UTF-8 byteとする。
値はkb-app側の保守的な運用予算であり、hostの仕様上限そのものとは区別する。
実稼働Codex hostの版と対応する仕様を検証できるまで、PATH上の`codex --version`や
モデル名だけを根拠に予算を拡大しない。plain text形式と既存の管理ポリシーは維持する。

先頭の注意書き、取得・出力統計、劣化行、本文ラベル、候補案内、末尾改行も予算へ含める。
本文は検索順位順の先頭から文書単位で渡し、最初に収まらない文書以降を省略する。
本文の途中切断や短い下位文書での埋め戻しを避け、長さで検索順位を変えない。
警告は本文より先に置く。長い警告や候補案内を全部表示できない場合も、その省略を明示する。
`emitted_chars`・`emitted_bytes`・本文数は出力側の実測であり、モデルの受信・参照を示さない。

公式文書は[Codex](https://learn.chatgpt.com/docs/hooks#large-hook-output)で既定約2,500 tokens、
[Claude Code](https://code.claude.com/docs/en/hooks#json-output)で10,000 charactersを超える出力の
退避を説明している。具体的なbyte/UTF-16換算やCodexの版境界はその記述だけでは確定しないため、
出力予算内であることをhostでの全文受信の保証に読み替えず、実稼働環境の受入で別途確認する。

2026-09-05の再確認では、[Codex hookの共通入力](https://learn.chatgpt.com/docs/hooks#common-input-fields)に
hostの版を示すフィールドはなく、上記の出力仕様からも提案段階の「0.145以上なら9,000 UTF-16」
という境界は確認できなかった。子MCPの`clientInfo.version`はkb-app自身の版であり、hostの版には使わない。
予算の拡張は、実行中hostから得た証拠と、その版に対応する仕様・実環境での受入が揃ってから再検討する。

### 接続先の永続ID照合（2026-09-05）

hookには通常MCPの`--vault`が自動では引き継がれない。registryの既定が変わるだけで、
同じクライアントの自動検索と明示検索が別のKBを指し得る。現在の既定同士を比較しても検出できないため、
`kb-core::client_binding`が接続時の期待値をsurfaceごとに保存する。

- GUIの「完全保護を設定」で、選択中Vaultの登録名と保存済みworkspace IDをCodex / Claude Codeへ固定する。
  Claude Desktopは「接続」時に固定する。名前とIDは同じVaultから取得する。
- 保存先は端末ローカルの`app_data_dir()/client-bindings/`。surfaceごとにschema・surface・登録名・IDを
  検証し、同じディレクトリの一時ファイルから置換する。破損を未設定へ変換しない。
  trusted GUIでの再設定は破損した当該surfaceだけを置き換え、他の接続を保持する。
- hookは引き続き設定やVaultを直接読まない。子MCPを`--require-client-binding`で起動し、
  KBがONのtools/callでのみ固定済みの登録名を選ぶ。通常MCPの既存`--vault`は上書きしない。
- MCPは要求中の期待IDを固定し、remote pull・DB操作前とpull後に、自己修復せず保存済みIDを照合する。
  不一致は`vault_mismatch`、設定破損・実IDの確認不能は`workspace_unverified`で停止する。
  固定codeの通知だけを返し、拒否したKBの本文・名前・path・IDを応答に含めない。
- 同期内部もpull前のIDを固定し、通常pullとpush再試行のpullから戻った直後、MarkdownをDBへ
  importする前に同一性を確認する。IDが変わった状態を同期成功へ進めない。
  MCPはtool結果を返す前にも要求時のIDを照合する。書込み後の自動同期で止まる場合もあるため、
  接続先のエラー応答を「ノートは未保存」の証拠にしない。
- 新版で期待値が未設定の場合、自動検索は`workspace_unverified`の通知だけを返す。
  通常MCPだけは旧登録との互換を保ち、各tool応答へ未検証の通知を付ける。
  現在の既定や直近の別セッションから期待IDを自動で補完しない。
- GUIは固定した接続先が選択中Vaultと合わなければ再設定を案内する。
  このUI表示とOS sandbox自体の有効性は分離し、未設定を誤ってKB OFFと報告しない。
  OFF・initialize・tools/listではbinding/VaultのI/Oを行わない。

既存利用者はアプリ更新後、利用するKBを選んで「完全保護を設定」を再実行し、AIクライアントを
再起動・MCP再接続する。Claude Desktopは接続操作も再実行する。
別のKBへ切り替えるときも接続先を明示的に再設定する。クライアント内の複数KB同時接続はこの段階の対象外で、
1つのsurfaceには1つの期待IDを持つ。受入では既定の変更でもhookが固定したKBを選ぶことと、
通常MCPの別KB選択が拒否されることを、本文が漏れない状態で確認する。

### 実環境の制約

現在の自動導入 UI は macOS 向け。Windows の native Claude Code は filesystem sandbox 非対応のため、
完全保護を有効にしない。本人が管理者権限で policy 自体を削除した場合は次回検査で即座に
fail-closedへ戻るが、OS administrator 本人を攻撃者とは扱わない。すでに会話へ渡った内容は
filesystem deny では取り消せないため、比較では client 再起動と新規会話が必要である。開発高速モードは
Codexが端末上の他データへ触れる能力も広げるため、信頼できるローカル開発だけに用い、復旧後もCodexを
再起動して新規セッションで検証する。


## R2の状態行とcadenceキャッシュ（2026-09-05）

既存UserPromptSubmitの配送へ事実を添える。追加のread toolや起票triggerは設けない。
DBの任意派生artifactとしてrevision・cacheと更新triggerをregistryへ登録し、schema 8の
正本を保持したまま既存DBにも追加・自己修復する。cacheの内容はノートから再計算できる
checkpoint IDとplannerのlineageなしrecord件数だけで、受入checkpointや監査には使わない。

hookのcache missでは全件走査を始めず、未確認を返す。通常検索と書込後に温めるため、GUIだけで
更新した直後は次の通常MCP操作まで未確認になり得る。古い値の継続表示よりこの明示を選ぶ。
cache発行はsnapshotのrevisionと再構築generationで照合し、別process更新・自己修復と競合した
古い計算は破棄する。cadence stateは都度読み、時間境界と前回失敗はcacheから独立して再評価する。

状態行は既定ON、設定または環境変数でOFFにできる。initializeで実効状態を返し、KB OFF・ON不明では
状態行の台帳を開かない。毎回の件数はworkspace×surfaceの直近14日と明示する。cadence詳細の
出力履歴はsession（不明時はUTC日）ごとに分け、write/flush成功時だけ更新する。
履歴の破損は未確認表示へ落とし、復旧後に同じ詳細が再表示されることを許す。

子MCPの応答読取りはthreadと20秒期限で待ち、host側30秒timeoutより前に子を回収して劣化を出す。
最終stdoutを計測するため、状態行も既存rendererに渡し、予算外の追記はしない。
実hostへの配送・受信と観測ゲートの確定は別の受入事項に残る。

### 接続状態の固定通知（2026-09-05）

管理ファイルの照合で既に判定していた`Outdated`を接続判断に保持し、ユーザーの明示OFFと区別する。
明示OFFはguard評価より先に確定させ、従来の無音・Vault/binding/台帳I/Oなしを維持する。
ONを希望していても対応するcoding面のguardが古い場合、initializeとtools/callの拒否に
`guard_outdated`を付ける。KBは無効のままで、既存の権威ある終端結果は変えない。
hookは例外として固定の1行を返すが、検索と台帳記録を開始しない。
この通知は期待する管理設定との不一致を表し、古い版だけでなく所有権・設定差分も含む。
Missing・Conflict・検査失敗や接続先照合の失敗を一括してこのcodeへ変換しない。

管理保護の一致は、実行中hostがhook全文を受信できる証明にはならない。
有効なCodex・Claude Code面はinitializeで`host_capability_unverified`を返し、hookの通常出力にも
固定文を1回含める。保守予算とCapAssumptionは維持し、hostの版を子MCPの版から補完しない。
通知は状態行設定から独立し、最終stdoutの予算・統計・台帳実測へ含める。
通知文は既知codeから再生成し、MCP応答の自由形式message・設定path・診断原文を出力しない。
