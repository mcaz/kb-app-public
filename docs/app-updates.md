# アプリ更新と互換性検査

2026-09-09。一般利用者向けインストールの続きとして、設定画面からの手動更新、取得、署名・内容検査、適用前診断、起動受入と旧版の保持を実装する。実機で受入済みの765a845にはこの変更はまだ含まれない。現行候補の配信設定は未設定で、公開更新の成功を表示しない。

## 操作と実装の境界

設定の「一般」から手動で更新を確認する。更新があれば版を表示し、取得中は受信量、取得後は検査中の表示へ進む。署名と内容、対応する更新元、KBの互換性を確認した場合だけ「更新して再起動」を使える。取得の失敗は再試行できる。更新が利用できない状態と、確認して最新版だった状態を分ける。

取得とMinisign署名の検証は公式 `tauri-plugin-updater` 2.11.0を使う。HTTPSはfeedだけでなく取得URLとredirectにも強制し、取得側にも120秒の期限を渡す。pluginの取得終了callbackは署名検証前なので、ここでは適用可能にしない。`download()`が返した同じbytesをコアの `ValidatedPackage` が所有する。JavaScriptへpluginの直接install権限を与えない。

128MiBを超える取得は中断を要求し、返されたbytesにも上限を適用する。ただしplugin自身がメモリへ蓄積するため、callbackからfutureがキャンセルされるまでの一時使用量を厳密に128MiB以下とする保証ではない。内容検査は単一gzip member、展開後512MiB、4096 entries、単一file256MiBを上限にする。tar末尾まで読み、単一の `kb-app.app`、通常file/directory、一意なpathと全親directory、mode、plan、Info.plist、版・CPU target・識別子を照合する。link、特殊file、PAX/GNU拡張、パス衝突、末尾の別archiveは拒否する。展開先は排他で保持する空stagingに限定する。

## 永続データの互換性

`crates/kb-core/update-compatibility.json`を実行物と配布planの共通入力とする。`runtime_store=db-v1`、`database_schema=14`、`persistent_compatibility_epoch=1`が省略されず、実装側の値と一致することを確認する。欠落や未知の値を現在値で補わない。DB実装のschema定数との一致はテストでも確認する。

診断は登録済みすべてのKBと明示された `KB_VAULT` を対象にする。registryが存在しない場合と、読めない・壊れている場合を分ける。DBの通常openやmigrationを呼ばず、既存WALのcommitを含めて調べる。旧schema、将来schema、欠損、復旧が必要なDBは適用対象にしない。

診断にDB本体や既存WALを書き換えるSQL・checkpointを混ぜない。SQLite共有メモリの読取管理は永続データの変更とは区別する。事前観測後に別writerがWALを削除すると、read-only openが空sidecarを作る場合がある。観測前後の変化は拒否するが、全writerを止めるロックや、競合中も一切sidecarを作らない保証ではない。

DB以外にもsettings、registry、client binding、cadence、各SQLite台帳、Markdown、Artifactの永続形式がある。同じepochの宣言だけで共存を許可しない。署名されたplanの `compatible_sources` に、試験を受けた更新元のplan SHA-256と実行ファイルSHA-256の組が必要になる。表示版が同じ別commitや別buildを代用しない。現在の生成器はこの受入証拠を未接続として空配列を作るので、更新元はすべて拒否される。

最初の対応範囲ではschema/epoch変更を伴う更新を拒否する。全writer停止、全永続形式の整合した退避、旧新writerのfixtureによる共存・復元試験が別途揃うまで有効化しない。既存 `runtime_recovery::apply` は特定破損の限定復旧であり、更新の汎用rollbackには呼び出さない。

## 差し替えと起動の受入

macOSで実行中の `kb-app.app` だけを対象にする。展開先でDeveloper ID署名、hardened runtime、同じTeam ID、CPUを確認する。配置先の親に書込み権限が必要で、管理者権限を使った削除へ自動で切り替えない。

適用直前の診断後、同じfilesystemの `.kb-app-update` にplan・実行物・全fileのidentityを固定したjournalを保存する。GUIは旧実行物のsupervisorを起動し、準備完了を受けてから終了する。supervisorはGUI終了と互換性を再確認し、旧bundleを `previous.app` へ退避して新bundleを配置する。renameの前にintentを保存し、既知の中断形状だけを識別して復元する。稼働中の旧MCPを終了する操作は持たない。

新実行物のmainは、GUI・MCP・hook・復旧の各入口より前に更新状態を確認する。未受入の新MCP等は待たせ、supervisorが指定した新GUIは署名・identity・compiled metadata・全KBの診断を通常DB openとworker開始前に確認する。初期画面の取得成功後にUIがreceiptを渡し、その後5秒間GUIが動作していた場合に受入する。既存KBではsetup・home・categoryの取得成功、初回導入ではsetupの取得成功を条件にする。これを全機能や外部AIの受信成功とは扱わない。

新GUIの終了、起動失敗、90秒以内にreceiptが届かない場合は、そのGUIだけを終了して旧bundleへ戻す。旧版で再起動した際は復元を通知する。受入後も旧bundleは保持し、次の明示更新で現在の実物が一致する場合に限り退避物を整理する。更新後は利用中の各AIクライアントでMCPを再接続する。

supervisor自体の停止後は、生きている未受入GUIを推測で終了しない。GUIを終了した後、現在の実行物、または退避した `previous.app/Contents/MacOS/kb-app` の `--recover-app-update` で既知の未受入状態を復元できる。任意の配置pathは指定できない。未journalの部分展開、整理途中の停止、未知のfile、改変されたbundleは自動削除せず停止する。特に旧bundleのrename後にsupervisor自体が停止すると通常のアプリpathが一時的に存在しない場合があり、退避実行物からの復元が必要になる。電源断を含む無条件の自動復旧とは扱わない。

## 配布前に残る条件

更新用公開鍵とHTTPS配信先の正本は `app/src-tauri/updater-config.json`。未設定では両方nullとし、秘密鍵はsourceや一般成果物へ含めない。設定後は新たなbuildが必要になる。macOS更新は `.app.tar.gz` と `.sig` を使い、DMG/ZIPのhash manifestでは代用しない。

配布工程は同じclean checkoutから `--locked` で検査CLIをbuildしてSHAを固定し、
`kb verify-update-package` で署名と同じarchive bytesの内容を検査する。
CLIはregistryやKBを開かず、明示したarchive・署名・公開keyだけを読む。
受け取ったreceiptの版・target・commit・plan・全file/directoryの内容とmodeを候補appへ照合する。
署名と内容が一致しても共存受入のblockerは残す。

共存試験の受入証拠、Developer ID署名と公証、実配信先での取得、実際の旧版から次版への適用・起動・復元は未受入。現在のローカル候補を0.0.1の別commitとして新しい版とみなさず、実更新の受入にはCargo・npm・Tauriの版を揃えた次版を用意する。公開・GitHub Release作成・Issue完了は別の実行として扱う。

採用理由と一次資料は[ADR-0024](adr/0024-app-updates.md)にまとめる。
