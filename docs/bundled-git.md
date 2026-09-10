# macOSへ同梱するGit

2026-09-08。Git CLIはLFSのclean/smudge、clone、同期時のrebaseとautostashを担当する。
これをlibgit2へ一括置換せず、公式Gitソースからアプリ専用のGitを構築する。
利用者のCLT/HomebrewやGitへ依存しないことが目的で、ビルド機にはApple SDKと
Cコンパイラ・makeが必要。Windowsの配布・実機対応は別項目として扱う。

## 出典と固定値

`scripts/git-assets.json` が版・公式URL・tarball SHA256・build targetの正本。
Git 2.55.0の公式tar.xzのSHA256は
`457fdb04dc8728e007d4688695e6912e6f680727920f2a40bf11eacc17505357`。
取得・cache再利用・出力の照合で、この固定値を検査する。別版の実行ファイルや
開発機のGitを代用品にしない。

- [公式版](https://git-scm.com/)
- [公式checksum](https://www.kernel.org/pub/software/scm/git/sha256sums.asc)
- [上流ビルド定義](https://github.com/git/git/blob/v2.55.0/Makefile)
- [Darwinの定義](https://github.com/git/git/blob/v2.55.0/config.mak.uname)
- [署名の検証方法](https://www.kernel.org/signature.html)

checksum照合は配布された同一bytesの検証であり、開発者署名検証を済ませたとの
証拠にはしない。`.tar.sign` は展開後のtarに対する署名。公開前に確認した開発者鍵で
別途検証し、その結果をrelease証拠に残す。prepareは署名成功を推測・捏造しない。

## 準備の入口

```bash
node scripts/prepare-git.mjs --target aarch64-apple-darwin --minimum-system-version 13.0
node scripts/prepare-git.mjs --verify-only --target aarch64-apple-darwin
```

`--target`は`KB_TAURI_TARGET`、`CARGO_BUILD_TARGET`、host CPUの順でも解決できる。
`--minimum-system-version`は`MACOSX_DEPLOYMENT_TARGET`、manifestの13.0の順。
13.0は今回のビルド下限であり、13.0上の実機動作を測定済みという意味ではない。
Intel/Apple Siliconの両targetを定義するが、それぞれのbuildと実機証拠が必要。
対象CPUとApple SDKの版・compiler・flagsはprovenanceへ記録する。

取得だけを行う場合:

```bash
node scripts/prepare-git.mjs --fetch-only --target aarch64-apple-darwin
```

cacheは`target/bundled-git/source/git-2.55.0.tar.xz`。
検証済みarchiveをこの場所へ置けば再取得しない。別pathから渡す場合は
`--source-archive <path>`または`KB_GIT_SOURCE_ARCHIVE`を使い、同じSHA256を検査する。
取得失敗、欠損、checksum不一致は停止し、build成功のprovenanceを出さない。

`NO_RUST/NO_GETTEXT/NO_PERL/NO_PYTHON/NO_TCLTK/NO_EXPAT`で不要なruntimeを外す。
`NO_HOMEBREW/NO_FINK/NO_DARWIN_PORTS`を明示し、SDKのcurl・zlib・iconvと
Darwinの暗号実装を使う。Darwin 24以降のsystem iconv用上流workaroundは残す。
ビルド後、全Git Mach-OのCPUと`otool -L`を調べ、OS標準以外のdylib参照を拒否する。
SDK更新で新しいOS APIを参照する可能性があるため、minimum flagだけで旧OS対応を
保証しない。release verifierのMach-O下限検査と対象OSでの受入も必要。

## 配置と対応ソース

| build生成物                                 | アプリ内                                 |
| ------------------------------------------- | ---------------------------------------- |
| `app/src-tauri/binaries/git-<target>`       | `Contents/MacOS/git`                     |
| `app/src-tauri/binaries/git`                | 開発時の別名                             |
| `app/src-tauri/git-runtime/git-core/`       | `Contents/Resources/git-core/`           |
| `app/src-tauri/git-runtime/templates/`      | `Contents/Resources/git-templates/`      |
| `app/src-tauri/git-runtime/licenses/`       | `Contents/Resources/licenses/git/`       |
| `app/src-tauri/git-runtime/source/`         | `Contents/Resources/git-source/`         |
| `app/src-tauri/git-runtime/provenance.json` | `Contents/Resources/git-provenance.json` |

helperは`git-remote-http`、同内容の`git-remote-https`、`git-upload-pack`、
`git-receive-pack`、`git-http-fetch`。全て通常ファイルとしてコピーする。
上流の`make install NO_INSTALL_HARDLINKS`だけではsymlinkが残るため使用しない。
runtimeは`GIT_EXEC_PATH`と`GIT_TEMPLATE_DIR`を同梱先へ固定し、再帰Git呼び出しと
Git LFSにも同梱GitのPATHを渡す。欠損した本番bundleをホストGitで補完しない。

sourceには公式tar.xz、実際の`prepare-git.mjs`と`git-assets.json`、空の
`patches.json`を添付する。ソース変更はしない。licensesにはGitのCOPYINGを含む
ソース中のCOPYING/LICENSE/NOTICEファイルを元の相対pathで収集する。
GPL v2の対応ソースを同じ配布単位で渡せるよう、Tauri resourcesへ全て同梱する。
外部リンクだけを対応ソースの配布済み証拠にしない。

provenanceの`inventory`はgit-runtimeからの相対path→SHA256。`binary_sha256`は
署名前Git本体。`recipe_sha256`はmanifest・script・toolchain・flagsを結合する。
全inventoryに加え、tarの公式hashと現在script/manifestの実bytesを直接照合してから
cacheを再利用する。時刻を書かず、同じ入力のbuild hook再実行でbytesを変更しない。
署名後Gitのhashは変わり得るので、releaseでは署名前入力と署名後bundleのhashを区別する。

## 検証

```bash
node --test tests/git-bundle/prepare-git.test.mjs
node tests/git-bundle/acceptance.mjs
```

既定はbuild treeの資材を検査する。署名後bundleを検査する場合は`--git`、
`--git-lfs`を`Contents/MacOS/`内の各実行ファイルへ、`--runtime`を
`Contents/Resources`へ、`--templates`を`Contents/Resources/git-templates`へ指定する。
templateのdescriptionとinfo/excludeは実行前に必須検査し、Gitが欠損を警告だけで
扱っても受入成功にしない。

合成テストはchecksum/recipe/inventoryの改変、helper欠損・実行属性、symlink、
OS外dylib、対象CPU/minOS入力を検査する。受入scriptは一時ディレクトリだけで
実際の同梱Git/LFSを使い、日本語・空白pathでpointer化、local bareへのpush、
fresh clone後のLFS実体復元、rebase＋autostashを通す。PATHに`/usr/bin`やCLTを
入れず、必要なOS道具だけを一時的な許可リストへ置く。個人のGit設定や認証環境は継承しない。

2026-09-09、本人が取得した公式source cacheのSHA256を照合し、arm64向けを
Apple clang 17・SDK 26.2・minimum 13.0で実ビルドした。`--verify-only`が成功し、
本体・helperのCPUとOS標準dylib限定も検査済み。Git本体のSHA256は
`693a19ced65c454bb0107766a736f60535b3965ddcfffd1f0b113b8718a13065`、
provenanceのSHA256は
`1c1d644c28fcc530f4717ea266eef2f467454ed13822e983dc4223aeda1a7e30`。

PATHを同梱Gitと`/bin/sh`への一時リンクだけに限定したGit単独の実動作も成功した。
日本語・空白pathでinit→commit→local bareへのpush→fresh cloneを通し、内容一致を
確認した。prepare再実行でGit本体と全inventory/provenanceのbytesが変わらないことも
確認済み。これはこの開発機でのGit単独検証であり、LFSを含む上記受入scriptの完了を
示さない。LFSの公式資材取得後に完全な受入を行い、Intel向けbuild、HTTPS/TLS/認証、
macOS 13およびCLT/Homebrewがない実機での起動・同期を別に検証する。
