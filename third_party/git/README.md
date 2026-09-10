# 同梱Gitの出典

Git本体は `scripts/git-assets.json` で固定した公式ソースからビルドする。
第三者の実行ファイルや開発機のGitをコピーしない。

- 公式配布: https://www.kernel.org/pub/software/scm/git/
- ビルド定義: https://github.com/git/git/blob/v2.55.0/Makefile
- GPL v2: https://github.com/git/git/blob/v2.55.0/COPYING
- 準備手順と検証境界: [bundled-git.md](../../docs/bundled-git.md)

準備スクリプトは、検証したtarball、使用したスクリプトとmanifest、全ソース内の
COPYING/LICENSE/NOTICEファイルを生成物に添付する。ソース変更は行わず、
`source/patches.json` の空配列にもその事実を記録する。これらは配布時に
`Resources/git-source` と `Resources/licenses/git` へ含める。

ソースtarballはGPL v2の対応ソースとして同じアプリへ同梱する。
SDKのOS標準ライブラリは再配布せず、ビルド後の依存先を検査する。
