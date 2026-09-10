# ADR-0024: 公式updaterの署名検証と、アプリ更新の受入条件を分ける

- 日付: 2026-09-09
- 状態: 採用。実配信と新旧writerの共存は未受入

## 背景

一般導入後の更新には、取得したpackageの真正性だけでなく、対象アプリ・CPU・版・永続形式の一致が必要になる。GUIを更新しても起動中のMCPは旧binaryを使い続けるため、同じ表示版だけで共存可能とは判断できない。

## 決定

取得と更新署名の検証には公式`tauri-plugin-updater` 2.11.0を使う。独自のHTTP取得・Minisign署名実装を製品へ重複させない。公式実装はdownloadとinstallを分離でき、downloadが署名を検証する。install自体が再検証したとは扱わない。

検証済みの同じbytesを`kb-core::app_update_package::ValidatedPackage`が所有する。展開前に単一の`kb-app.app`、通常file/directory、容量上限、パス衝突、計画とInfo.plistを検査する。PAX・GNU長名・link・特殊fileを使うtarは受け付けない。展開先は排他的に所有する空stagingに限定し、実配置やcodesignの受入は別工程にする。この内容検査型自体は署名の真正性を認証しない。

`crates/kb-core/update-compatibility.json`をcompiled metadataと配布計画の共通入力とし、`runtime_store`・`database_schema`・`persistent_compatibility_epoch`を省略できないようにする。epochはDB以外の台帳も含む永続形式の契約IDであり、新旧writerの共存試験が成功したことを表す値ではない。

更新元は`compatible_sources`のplan SHA-256と実行ファイルSHA-256の組で指定する。表示版や自己申告の`passed: true`から許可リストを作らない。現段階の配布生成器は検証可能な共存receiptを取り込む口を持たないため、空配列だけを生成する。実行時も空配列はどの更新元も許可しない。

公開配信設定の正本は`app/src-tauri/updater-config.json`とする。未設定はendpoint・public keyが両方nullの状態で、片方だけの設定は拒否する。設定済みreleaseでは資格情報を含まないHTTPS URI、公式公開keyの形式、`TAURI_SIGNING_PRIVATE_KEY`を要求し、Tauriの`createUpdaterArtifacts: true`で`.app.tar.gz`と`.sig`を作る。秘密鍵やパスワードを計画・成果物へ書き込まない。candidateは未設定のまま生成できる。

配布時は同じclean sourceから`--locked`で独立CLIをbuildしてSHA-256とversionを固定する。`kb verify-update-package`は明示fileだけを上限付きで読み、公式pluginと同じ`minisign-verify`による署名検証とcoreの内容検査を同じbytesへ適用する。KB・レジストリ・個人設定は開かない。Nodeの配布検査はreceiptのarchive/plan hash・対象版・CPU・source commitを検査し、全file・directory・権限も候補appと一致させる。verifierを明示しない検査では署名・内容のblockerを残す。署名と内容が一致しても共存receiptのblockerは別に残し、更新配信できるreleaseとして成功させない。

展開前に受け取った圧縮bytes・解凍合計・各entry・file数へ上限を設ける。これは公式pluginのdownload内部で単一pollが確保するメモリの厳密な上限を保証しない。取得中の監視と、取得後のarchive検査の限界を区別する。

## 比較した代案

公式pluginの`install`だけを使う案は実装量が少なく、OSの認証導線も利用できる。一方、今回必要な旧MCPとの共存確認、永続journalによる復元、同一appの受入記録までを満たさないため、そのまま採用しない。公式の取得・暗号検証に、内容検査と更新supervisorを組み合わせる。

独立したHTTP取得器はstream単位の受信上限を細かく制御できる技術的な利点がある。一方、endpoint解釈・platform選択・取得時の署名処理の保守が重複する。今回は公式pluginを採用し、受け取ったbytesの容量境界とdownload中の厳密なメモリ上限を同一視しない。

## 検証と残る条件

合成テストでアーカイブの不正構造、版・識別子・永続形式の不一致、半設定、危険なURI、署名file欠落、秘密情報の非出力を検査する。実際にTauriが生成したarchiveが受入subsetに収まること、旧sourceと新sourceのMCP共存、Developer ID署名・公証、HTTPS配信、更新失敗時の復元は別途実測する。

DB診断は全登録KBと明示KB_VAULTを対象にし、旧schemaも自動migrationしない。これは観測時点の診断で、後続の更新許可やrollback成功のreceiptではない。具体的な境界は[更新の設計](../app-updates.md)を参照する。

## 一次資料

- [公式Tauri Updater](https://v2.tauri.app/plugin/updater/)
- [Update 2.11.0 API](https://docs.rs/tauri-plugin-updater/2.11.0/tauri_plugin_updater/struct.Update.html)
- `tauri-plugin-updater` 2.11.0配布sourceの`src/updater.rs`: `download`と`verify_signature`、`install`の境界を確認した。
