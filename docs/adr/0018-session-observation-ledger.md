# ADR-0018: 配信と起票の観測を、本文を持たない端末ローカル台帳へ記録する

- 日付: 2026-09-05
- 状態: 採用(R1、R2観測補完とClaude開始計測は2026-09-06本人採用)
- 関連: [契約17](../contract.md) / [FR-C9・FR-A2](../requirements.md) / [ADR-0006](0006-ai-raw-access-guard.md) / [ADR-0007](0007-client-surface-and-conversation-events.md)

## 背景

hookの出力上限を守るだけでは、出力を準備したのか、stdoutへ書けたのか、起票の呼出しが
あったのかを後から区別できない。会話本文やノートを複製せず、アプリ自身が観測した段階を
記録する。台帳から起票漏れやhostの受信を推定する機能は、この段階に含めない。

## 記録する段階

`kb-core::session_ledger`がschemaと集計を所有し、hookとMCPは観測値だけを渡す。
保存先は`app_data_dir()/session-ledger/ledger.sqlite3`。初期版のschema v1へ、
R1の拒否分類・Home観測表示でschema v2を追加する。
複数の短命hookと常設MCPが同時に追記するため、JSONファイルの全置換ではなく既存依存の
SQLite transactionを使う。ロック待ちは100msに制限し、競合を会話の長い待機へ変えない。

| 観測 | 記録する意味 | 保証しないこと |
| --- | --- | --- |
| `output_prepared` + `prepared` | 整形済み出力と統計を、stdoutへの書込み前に記録した | stdoutへの出力完了 |
| `emitted` | 同じ観測の出力に対し`write_all`と`flush`が成功した | hostの受信・表示・モデル利用 |
| `stdout_failed` | 同じ観測の書込みまたはflushが失敗した | hostが一部も受け取っていないこと |
| `filtered` / `error` | 検索対象から外した固定理由、または失敗した固定段階 | 全発話を観測したこと |
| `propose` / `update` + `success` / `error` | MCPの応答生成段階で成功・エラーを観測した | MCP応答のstdio送信・host受信、error時のノート未保存 |

hookの完了更新には、挿入した`prepared`観測に結び付いたreceiptが必要となる。
重複観測には完了更新の権利を渡さず、`emitted`と`stdout_failed`間の上書きを拒否する。
完了更新が失敗した場合は`prepared`のまま残し、出力済み統計へ加算しない。
MCPの観測はONかつ公開tool surfaceで許可された`propose` / `update`だけを対象とする。

## 既存拒否の分類とschema v2

既存の検証失敗のうち、書込み前に拒否したと確定できる境界だけを`WriteRejection`へ分類し、
MCPの`structuredContent.write_rejection`と台帳へ同じcodeを渡す。
検証自体は保存済みノートの読込みや出力でも走るため、
原因の文言や検証エラー型だけで書込拒否へ昇格させない。R1ではtitle・本文の受理条件を維持した。
R2の通常起票入口では空文字・空白だけのtitle/bodyを保存前に拒否し、既存の`invalid_argument`へ
分類する。enumと台帳schemaは増やさず、既存v2 readerの互換を保つ。

| code | 既存の拒否分類 |
| --- | --- |
| `tag_count` / `tag_shape` / `tag_vocabulary` | タグの個数・形式・語彙 |
| `authority_scope` / `authority_shape` | authorityのscope・形式 |
| `relation_integrity` | relationの整合性 |
| `active_canonical_conflict` | active canonicalの重複 |
| `legacy_read_only` | 旧humanノートの書込所有境界 |
| `missing_argument` / `invalid_argument` | 必須引数の不足・引数の不正 |
| `mcp_capability` | MCPに公開されていない入力能力 |

台帳のwrite観測は`code: Option<WriteRejection>`とする。`error`かつ`code: null`は未分類であり、
成功とも拒否済みとも扱わない。診断文やノート内容をcodeの代わりに保存しない。

v1台帳は新版の最初のappend transactionでv2へ移行し、既存payloadを保持する。
read-only集計はv1/v2の両方を読め、読取りのために移行しない。旧binaryはv2を未知schemaとして
拒否するため、混在期間には台帳の記録が欠け得る。台帳障害で元のノート操作を失敗へ変えることは
しない。更新後は全MCP接続を再接続し、稼働中の旧binaryを残さない。

## 情報と保持の境界

- hookはMCPのinitializeで明示ONを確認してから台帳へ触れる。OFF、ON未確認、
  権威ある`kb_disabled`、未知clientでは台帳I/Oを行わない。UPS観測へ他eventを混ぜない。
  ClaudeのSessionStartは、後述の登録先照合を追加した開始証拠として別に扱う。
  `capabilities.experimental.kbApp.kb_enabled`はそのinitialize時点の状態であり、
  次のtools/callでは設定を再評価する。検索応答の`structuredContent.workspace_id`は
  `include_documents`時だけ添付し、ID取得失敗は未帰属と劣化に分ける。
- hookのworkspaceはMCPから得た不透明ID、MCP書込みの観測は既存のworkspace IDで帰属させる。
  名前やpathから推定せず、帰属できない観測は`unassigned`へ残す。
  フィルター済み入力は検索しないため、ON時もworkspace未帰属となる。
- session / prompt / turn / callの生IDは保存せず、surface・workspaceの区分を含めてhash化する。
  本文・query・タイトル・path・自由形式errorを受け取る台帳フィールドを持たない。
  permission modeは既知の列挙値だけを保持する。
- session IDが無い場合はUTC日単位のfallbackへ分け、実session数へ加算しない。
  MCPでsessionを採用するのはClaude Codeの`CLAUDE_CODE_SESSION_ID`がある場合だけで、
  直近hookから書込みを推測で結び付けない。
- 出力統計、initialize / search / renderの所要時間、予算とその根拠を記録する。
  host版は`unverified`、予算は`conservative_fallback`とし、記録する検証日は運用予算を
  定めた日を表す。稼働hostの受信確認日として扱わない。
- 接続先の期待IDは[ADR-0006](0006-ai-raw-access-guard.md)のclient bindingで固定する。
  照合で停止した自動検索を「該当なし」や出力成功へ変換しない。確認できないworkspaceへ帰属させず、
  固定段階のエラーとして未帰属に残す。ID不一致を通常の起票内容検証による拒否codeへ混ぜない。
- 台帳はVault・索引・Git同期から独立する。追記transactionで90日より古い行を整理し、
  集計や完了更新は保持整理を行わない。定時削除は設けない。

## 障害と集計

台帳への追記に失敗しても検索本文やノート操作の結果を保ち、`session_ledger`劣化として伝える。
hookでは警告を含む全体を再整形し、既存の出力予算を維持する。stdoutへの出力後に完了更新が
失敗してもstdoutへ追記せず、stderrへ固定の診断を出す。壊れたDB・未知schemaは空台帳へ
置換せず拒否する。

```bash
kb sessions --days 14
kb sessions --days 14 --workspace-id <opaque ID>
```

`--days`は1〜90、既定14。Vaultを開かず、台帳をread-only / query-onlyのsnapshotで集計する。
台帳が無ければ`exists: false`を返し、DBやdirectoryを作らない。workspace指定時も
`unassigned`は別枠に残す。`--vault`は使わない。

JSONはworkspace・surface別にhookの各状態、出力済みの文字数・byte数・本文数・省略数、
propose/updateの成功・エラー数、指定期間内の最終propose成功応答時刻を返す。
`hook_groups`と`write_groups`の`actual_sessions` / `daily_fallback_days`は別集計であり、
そのまま起票率の分母・分子として結合しない。

## Homeでの観測表示

`home_observation_health`をノート一覧の`home_state`から分離し、台帳をread-onlyで取得する。
Home表示中は15秒ごととアプリ復帰時に再取得する(2026-09-06の自動更新共通化)。
観測の再取得自体からVault保守やノート一覧の再読込みを起動しない。
選択中workspaceの直近14日の件数、最大90日内の最終propose成功応答を示し、未帰属分を別枠に残す。
別workspaceの観測を現在のworkspaceへ合算しない。

状態は`available` / `no_observations` / `disabled` / `unavailable`に分ける。
14日内の観測が無くても90日内にpropose成功応答があれば、件数0と最終成功からの日数を表示する。
`no_observations`は、表示対象の14日内の観測も90日内のpropose成功応答も無い場合を表す。
全体OFFまたは両AI familyがOFFなら台帳I/Oを行わない。一部だけOFFなら読める過去記録は残し、
現在の設定がOFFであることと区別して表示する。設定やworkspaceの識別・台帳の読取りに失敗した場合は
取得不能として扱い、空配列や0件へ変換しない。

Homeで表示するのは出力の各状態・propose/updateの結果・型で分類した拒否と未分類errorの件数である。
host実受信率・起票漏れの判定は作らない。固定した対象集合の書込率は、下記の専用集計に分ける。

### 日別推移の接続元による絞込み（2026-09-06 本人決定）

`home_observation_trend`の直近14暦日の利用実績を、全体・Claude・GPTで切り替える。
台帳の検証を通ったeventの`ClientSurface::family()`を使い、Claude Code/Claude Desktopを
Claude、Codex/ChatGPTをGPTに分類する。全体は現行どおり評価用を含む全ての有効なsurfaceを
数えるため、ClaudeとGPTの合計に一致するとは限らない。モデル名やノートの作成者から推測せず、
未知surfaceの拒否も緩和しない。台帳のschema変更や過去記録の書換えは不要とする。

期間合計・グラフ・日別一覧は同じ対象から求め、観測の有無も絞込み後に判定する。
一部OFFでも保存済みの履歴は参照できる。全体OFF・両family OFFではworkspaceや台帳を読まず、
読取専用のsnapshot・workspace除外・receiptの検証は従来どおり維持する。
キャッシュは対象と全体OFFの状態ごとに分け、切替待ちや取得失敗を他の対象の実績で覆い隠さない。
設定取得済みの全体OFF・両family OFFでは、UIからの履歴取得も省く。

## 固定期間の観測補完（2026-09-06 本人決定）

状態行ON/OFFを比べる観測案が採用された。旧summaryのhook/write別集合を割る方法では、
診断・失敗だけのwrite・異なる会話を混ぜてしまうため、元の件数表示を維持して専用集計を追加する。
この変更は計測だけを補い、R3のhook、承認ルール、検索・出力予算を変更しない。

### 記録する機械的な事実

新規eventには通常利用/診断/不明の区分、状態行の実効状態（GUI ON/OFF・環境上書き・不明）、
最終rendererでの状態行/cadence包含・予算省略を付ける。包含フラグは出力の準備内容であり、
従来どおりstdoutのwrite/flushが完了した`emitted`と合わせて読む。
writeには既存の成功/エラーに加え、成功updateの型付き削減警告を記録する。
警告情報の欠落を「警告なし」へ変換しない。plannerの採用率や履歴は今回追加しない。

旧eventの欠けている情報は不明のまま読む。新規processの用途は既定normalで、診断launcherから
`KB_APP_OBSERVATION_PURPOSE=diagnostic`を継承する。未知の値は不明とする。
この区分は起動者が指定する計測ラベルであり、診断を自動的に見抜く機能ではない。
actorの自由な末尾から用途を推測しない。

初期の観測補完では、起動補助が生成した`KB_APP_OBSERVATION_SESSION_ID`と
`KB_APP_OBSERVATION_SESSION_STARTED_AT_MS`を、host由来の実session IDに照合する。
片方欠落・不一致・不正時刻は開始不明とし、保存するのは開始時刻と匿名IDだけ。
生ID・会話本文・プロンプト・設定pathは追加保存しない。
`scripts/start-observation-claude.py`は新規UUIDを公式の
[`--session-id`](https://code.claude.com/docs/en/cli-reference)へ渡し、resume等の任意オプションを
転送しない。`CLAUDE_CODE_SESSION_ID`を起動補助で捏造・補完せず、hostが渡さなければ
writeの会話帰属は不明に留める。単なる台帳初出では新規会話を証明できない。
この方式ではCLI起動補助を通らないClaudeアプリのルーティンの開始を確認できないため、
後述のSessionStart計測で新規eventの開始条件を置き換える。旧eventの時刻と出所は書き換えない。

### 固定区間と対象集合

maintenance面の`observation_summary`は、接続先照合済みの現在workspaceだけを対象にする。
read/write面からの呼出しとKB OFFは従来の終端拒否を維持する。生台帳・session hashのexportや
任意path/他workspaceへの切替は公開しない。CLIの`observation-summary`も同じcoreの集計を使う。
集計はread-only snapshotとし、不在時にDB作成、旧schemaのmigration、保持整理を行わない。

区間は`[since_ms, until_ms)`で、クライアント・実効状態・hookで観測したpermissionを分ける。
Claudeの分母Dは、開始確認済み・単一区間/状態・既知の許容permissionに属し、
非通知/filter通過の通常UserPromptSubmit観測が3回以上ある実session集合。
分子Nは、その同じDに属するpropose/update成功観測のあるsession集合。
writeのpermissionはhostから得られないため、hookの既知設定で分類した結果であり、
各writeのpermissionを検証済みとは主張しない。Codexの日次代替集合をD/Nへ混ぜない。

同じ状態のClaudeでhook/writeの開始情報と実IDの一致を少なくとも1件観測でき、帰属不能な
通常/不明writeがない場合だけ`write_linkage_verified`を立てる。成功・失敗のどちらのwriteも
帰属確認の証拠になる。writeが1件も観測されていない場合を含め、帰属確認が不足した率は
`null`と理由を返し、0%と表示しない。D/Nはこの場合も暫定件数として残す。
帰属確認とその件数は状態単位でpermission別の行に併記されるため、行を足し合わせない。
`normal_counts`は開始未確認も含む通常利用の操作件数で、D/Nの会話集合とは別の値である。

元の本文出力6件以上は補助情報とし、本文数で主分母を選別しない。状態行が本文予算を使うため、
出力本文数で選ぶとON/OFFの対象が変わり得る。turn/prompt IDがあれば従来の重複除去を使うが、
IDのないhook観測は本人の異なる発話数を保証しないので、その件数を併記する。

診断が混ざったsession、設定/permissionが混在するsession、開始不明、境界外にも観測されたsessionを
除外し、理由別の件数を返す。手動診断の`manual_exclusions`は指定窓全体を除き、通常利用まで
除外され得ることを明示する。保持期間外、旧計測情報、未確定出力などの品質情報も返す。
必要情報がないsessionは除外し、計測系全体の欠落規模を把握できない場合は比較を観測不足とする。
hook不発の会話は見えないため、実利用全体への捕捉率・起票不要率は求められない。
`period_closed`は終了時刻を過ぎたことだけを示し、会話終了の証明ではない。
後日同じ会話が再開されれば境界跨ぎとして除外され、再集計結果が変わり得る。
比較結果には集計時刻を残し、各条件の開始時は新しい会話を使う。

### 運用上の判断との分離

アプリ反映・通常接続でのID帰属受入後に開始を合意し、ON7日/OFF7日を観測する。
各条件5session・計10以上は集計を読む最低件数であり、因果効果の証明ではない。
不足時はONへ復帰して観測不足とし、自動延長や閾値の引下げは行わない。
書込率50%未満・通常利用があった7日起票0は、具体的な書込見送り例や削減警告の内容を確認して
R3を再協議する目安とする。lineage解消率は、MCPで固定した開始時UID集合を追跡できる場合に限る。
現在総数の差やcadenceの最終失敗表示から履歴率を推定しない。集計APIは数値によるpolicy切替を行わない。

## Claudeの開始イベント計測（2026-09-06 本人採用）

Claudeアプリが作るルーティンの会話は、CLI起動補助を通らない。通常利用の書込みを診断として
除外せず、hostのSessionStartで観測できる開始を補う。追加するhookは開始計測だけを担当し、
Stop、起票の自動発火、操作の許可・拒否、検索・出力予算の変更は含めない。

### 開始証拠と接続先照合

管理SessionStart hookは`--hook-session-start`で同じkb-app実行ファイルを起動し、
payloadの実session ID・sourceと、その開始イベントを受け取った時刻を使う。
[`SessionStart`](https://code.claude.com/docs/en/hooks#sessionstart)の`startup`を新規開始候補とし、
`resume` / `clear` / `compact` / `fork`を新規開始へ読み替えない。
イベントの観測時刻はhost内部の会話誕生時刻を保証しない。時刻を持たないpayloadに対し、
最初のUPSやwriteを開始として補うこともしない。

開始hookはread面・`--hook-context`・必須client bindingの子MCPへ、initializeの
`kb_app_session_observation=true`を明示して問い合わせる。明示ONのClaude Codeでこの組合せが
揃った場合だけ、コアが登録先の`.kb-workspace`メタデータを読み、保存済みの期待IDと照合する。
一致時の`kbApp.session_observation_binding={verified:true,workspace_id}`だけを採用する。
設定欠落・不一致・確認不能・OFFでは開始記録へのI/Oを行わない。子MCPは検索・本文取得・索引読込・
同期を行わず、hook自身がVaultを読む経路も作らない。通常initializeの「Vaultを開かない」境界は
維持し、この計測補完では通常read面にtoolを追加しない。
（後続の提案管理によるget_proposal追加は[ADR-0019](0019-proposal-workflow.md)を参照。）

証拠はVault外の端末ローカル領域へ保存し、sessionの生IDはworkspaceで区分してhash化する。
本文・プロンプト・transcript・設定path・argvは保持しない。lookupはread-onlyで、不在・破損・
未知schema・失効・保持期限超過は開始未確認とする。読取のためにDBを作成・修復しない。
起動補助のID一致時刻は補助情報として残せるが、新規eventを開始確認済みにするfallbackには使わない。

### 再開と長寿命MCPに対する保守的な失効

Claudeの[環境変数仕様](https://code.claude.com/docs/en/env-vars)では、stdio MCPの
`CLAUDE_CODE_SESSION_ID`は起動時の値を保持し、`/clear`などでhook側の現在IDと異なり得る。
単にID別の開始時刻を保持すると、古いMCPの書込みを以前の会話へ誤って結合する。
runtimeの所有関係をプロセス名・共通親・直近の発話から推測せず、workspace単位で次を適用する。

- 有効なstartup証拠は最大1つ。異なるIDのstartupまたはstartup以外のsourceを観測したら、
  既存証拠を失効させる。新しいstartupだけが新しい有効証拠になり得る。
- 同じ有効startupの再通知は元の観測時刻を保つ。失効済みIDの再通知では復活させない。
  匿名session IDと開始証拠は失効記録を含め90日保持する。workspace単位の世代・時刻の境界値は、
  遅着eventで再有効化しないための制御状態として保持期間後も残す。生IDは保持せず、
  永久のsession ID再利用検出や古い会話の復元は保証しない。
- UPSはpayloadの実IDで証拠を照合する。MCPは各書込の前後で同じworkspace・実ID・証拠の世代を
  照合し、途中の失効や世代変更を今回の書込の開始未確認として残す。
- MCPが先に起動しても、その後に届いたstartup証拠を次の書込時に参照できる。
  証拠が無かった過去のUPS/writeを遡って確認済みへ変えない。

この方法では並行会話の一方の開始・再開によって、他方も観測対象から外れ得る。
過剰除外を隠して通常利用の分母へ戻さず、件数と未確認の理由を残す。
また、**後続のlifecycle hookが欠けたこと自体は検出できない**。hookが発火しても、DB障害や
ロック競合で失効を保存できなければ、以前の証拠が残り得る。KB OFF中や登録先切替中の遷移も
開始記録へ反映できるとは限らない。開始hookが最初から無い場合の未確認と、この後続イベントの
欠落は別の限界である。`write_linkage_verified`は保存できたhook/writeの証拠が一致することを示す。
観測して保存できた遷移の失効を実装したものであり、古いMCP IDの完全な現在性検証、runtimeによる
全event発火、全会話の捕捉を保証しない。計測障害は元の検索・書込を止めずに扱う。

新規eventの`session_started_at_ms`は出所`host_start_event`を伴うstartup観測時刻とし、
起動補助の時刻や出所を持たない旧eventの時刻とは品質集計で区別する。
固定区間の開始判定もこの観測時刻を使うため、host内部では区間前に作られ、hook到着が区間内に
なった可能性は残る。期間境界付近の比較で厳密なhost誕生時刻として使わない。
SessionStartは通常UPS回数へ加えず、分母の「通常UPS観測3回以上」を変更しない。

### 反映と実環境の受入

アプリの配備だけでは管理hookと稼働中のMCPは更新されない。配備後に完全保護設定を再生成し、
Claudeを再起動・再接続して新しい会話を使う。古い管理設定はOutdatedとして検出する。
CLI起動補助をルーティン本文から呼ぶと別の会話を作るため、ルーティンの開始計測の代替にしない。

実環境の受入は別途必要であり、この採用・実装だけで完了したとは扱わない。
通常CLIの新規会話、Claudeアプリの新規会話、ルーティンの手動実行と予定時刻の実行について、
実IDの開始証拠、通常UPS、同じ会話での書込の照合を確認する。
自動検証ではMCP先行、重複startup、失効後の再通知、書込途中の世代変更、再開・clear・compact・fork、
OFF・登録先不一致、記録不在・破損、期間境界を確認する。
受入の記録とON/OFF観測の開始合意は分け、T0や設定切替をこの変更で自動設定しない。

## 後続段階

文脈detector、gate、自動起票triggerはこの台帳に追加しない。
観測が無いことを起票不要・起票漏れ・KB未使用のいずれかへ自動判定しない。
