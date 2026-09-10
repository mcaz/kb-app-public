# macOS候補配布物の生成

この経路は、指定commitのappとDMG、同梱物・署名・hashの検証manifestを作る。
既存アプリの配置、GitHub Releaseの公開、更新配信は実行しない。
一般利用者が開発環境のないMacで初回記録・検索へ到達した受入とは別の証拠とする。
一般向け導入Issueの部分実装であり、updaterの配信先・署名鍵と実機受入は未完了である。

## 入力と版

- `RELEASE_COMMIT`: checkoutのHEADと一致する完全な40桁commit SHA。未commit変更や未追跡ファイルがあれば停止する。
  ignoredのsidecar・compile出力はGit差分とは分け、専用の生成証拠とhashを照合する。
- `RELEASE_VERSION`: `X.Y.Z`。Cargo workspace、npm package、Tauri設定と完全一致させる。
  Tauri overrideだけで版を変えるとMCPのRust版表示が古いままになるため認めない。
- `RELEASE_TARGET`: `aarch64-apple-darwin` または `x86_64-apple-darwin`。各CPUを別のMac runnerでbuildする。
- `RELEASE_MINIMUM_OS`: 明示的なbuild下限。今回の候補では`13.0`を使う。
  そのOSで実機受入済みという意味ではない。
- `RELEASE_MODE`: 資格を使わない`candidate`、Developer IDと公証を必須とする`release`。
- `KB_GITHUB_CLIENT_ID`: GitHub OAuth Appの公開Client ID。ローカル実行でも呼び出し側が設定する。
  空または空白だけならbuild前に停止する。client secretを渡す欄ではない。

準備・build・検査は`bash scripts/prepare-macos-release.sh`へまとめている。
compileには専用の`target/macos-release`を再利用し、前回のbundle出力だけを除去する。
通常の`target/release`や配置済みアプリには触らない。生成済みの`release-output`は上書きしない。

ローカルでもwrapperはTauriへ`--ci`と`CI=true`、`TAURI_BUNDLER_DMG_IGNORE_CI=false`を渡す。
DMG生成のFinder AppleScriptを省略し、GUIへの自動操作権限に依存しない。
このためDMGの背景・アイコンの座標調整は行わない。`--ci`単独ではこの省略は効かない。
[Tauri 2.11.4のDMG生成処理](https://github.com/tauri-apps/tauri/blob/tauri-cli-v2.11.4/crates/tauri-bundler/src/bundle/macos/dmg/mod.rs#L162)

計画だけを作る入口は次のとおり。
その前に指定targetのGit LFSを準備し、公式archiveと署名前binaryのprovenanceを生成する。
Git本体も同じtargetで準備する。Gitの生成証拠は全bytesのhashで計画と同梱物を照合し、
生成側のsource・build条件・binary検査を、配布側で別のschemaへ書き直さない。

```sh
node scripts/prepare-macos-release.mjs \
  --mode candidate --version 0.0.1 --source-commit FULL_COMMIT_SHA \
  --target aarch64-apple-darwin --minimum-system-version 13.0 --output /tmp/candidate-plan
```

`plan.json`と`tauri-release.json`を生成する。Tauriの公式`--config`による設定の追加を使い、
元設定を編集しない。[Tauri CLI](https://v2.tauri.app/reference/cli/)
同じplanをappの`Contents/Resources/release/plan.json`へ同梱し、検査時にsource・版・targetの一致を照合する。

## 署名・公証

`candidate`は明示的なadhoc署名を検査し、公開向け署名・公証は未確認と記録する。
Apple資格を引き継いだ候補生成は拒否する。adhoc署名を一般配布の受入には使わない。

`release`は以下の資格を環境から受け取る。値をplan・manifestへ書き込まず、子processの失敗出力を
そのままログへ転記しない。Team IDは署名の照合に用いる公開識別子としてplanに残す。

- `APPLE_SIGNING_IDENTITY`: Developer ID Applicationのidentity
- `APPLE_TEAM_ID`: 署名を照合するTeam ID
- `APPLE_API_ISSUER`、`APPLE_API_KEY`、`APPLE_API_KEY_PATH`: 公証APIの資格

Tauriはappの署名・公証を行う。DMGにもDeveloper ID署名を付け、公証のAcceptedを確認してticketを
stapleする。検査ではappとDMGの署名・Team ID、appのhardened runtime、両方のstaple ticketと
Gatekeeper評価を確認する。署名後にadhocへ置き換える既存の開発用deployスクリプトには渡さない。
[Tauri macOS署名](https://v2.tauri.app/distribute/sign/macos/)、
[Appleの公証workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow)

## 検査と成果物

`verify-macos-release.mjs --plan PLAN --app APP --dmg DMG --output DIRECTORY`は次を検査する。

- bundle識別子、版、macOS下限、実行ファイル、Git LFS版とCPU。
- Git LFSの公式archive SHA、署名前sidecarの実bytes、署名後の配布物hashを別々に記録する。
  署名でbytesが変わるため、署名前hashを署名後binaryへ直接比較しない。
- Git本体も、生成記録の版・target・build下限・公式source SHA・署名前binaryを照合する。
  対応ソースarchive、実際の`prepare-git.mjs`と`git-assets.json`、`COPYING`を必須とし、
  source・license・template全ファイルのhashを、生成時の計画とapp内で照合する。
- 全Mach-OのCPUと動的ライブラリ。OS標準以外はapp内へ解決できなければ停止する。
  実際のmacOS下限もload commandで確認する。各Mach-O自身のRPATHを使い、独立起動するGit/helperへ
  kb-app本体のRPATHを引き継いだと推測しない。複雑なloader chainが必要な構成は未解決として停止する。
- 同梱Git LFSライセンスと`THIRD_PARTY_NOTICES.md`の実bytes。
- app木全体のファイルhash・実行bit・内部symlink。app外へ出るsymlinkや非通常ファイルは拒否する。
- DMGをreadonlyでmountし、中のappが候補appと同じであること。
- 検査前後のapp・DMGの不変性。出来上がったapp.zipとDMGのSHA-256。

`manifest.json`にはcommit、target、版、計画hash、配布物hash、app木の一覧、検査結果を残す。
Git本体が欠ける候補には`bundled_git_missing`を付け、`release`では検査を失敗させる。
Gitの存在・version確認を、GitHubとの通信や開発環境のないMacでの動作成功へ読み替えない。

`distribution_verified`は署名・同梱検査の合格を表す。`public_release_accepted`は常にfalseで、
初回記録・検索、更新とDB復旧、稼働AIの再接続は`unverified`へ残す。
updater用の署名鍵・配布endpointは未設定で、OS署名と更新物の署名を混同しない。
アプリだけを旧版へ戻して、更新済みDBも復旧したとは扱わない。

## 手動workflowの境界

`.github/workflows/release-macos.yml`は`mcaz/kb-app`のmain上からのみ手動開始できる。
指定SHAは実行時のmain HEADとの完全一致を要求し、任意refやPRのコードへ資格を渡さない。
権限は`contents: read`のみ。成果物upload以外の公開操作を持たない。
runnerはarm64の`macos-15`とIntelの`macos-15-intel`を明示する。
[GitHub runner一覧](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)

release用GitHub secretsは上記に加え、`APPLE_CERTIFICATE`（p12のbase64）、
`APPLE_CERTIFICATE_PASSWORD`、`KEYCHAIN_PASSWORD`、`APPLE_API_PRIVATE_KEY`。
一時keychainと資格ファイルはrunnerの一時directoryに置き、終了時に削除する。
資格の作成・登録・実際の公証は、この実装作業では行っていない。

## 検証

```sh
node --test tests/macOS-release/release.test.mjs
bash -n scripts/prepare-macos-release.sh
```

合成fixtureは候補/署名検査の拒否境界とmanifestを検査する。Appleサービス、実署名鍵、実DMGの
公証成功を表すテストではない。候補生成後、別途開発環境のないMacで起動・作成/復元・接続・
最初の記録・検索を測定し、通信失敗・再試行・更新中断・再起動も受け入れる。
