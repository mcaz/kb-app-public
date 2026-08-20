#!/usr/bin/env bash
# kb-app をビルドして /Applications に安全に差し替える。
#
# 開発中の起動コマンドを毎回思い出さなくて済むように、普通のアプリとして
# Launchpad / Spotlight から開ける状態にするためのもの。build後の署名・
# rollback・起動受入は deploy-macos-bundle.sh に一本化している。
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "このスクリプトは macOS 用です(現在: $(uname -s))" >&2
  exit 1
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
app_name="kb-app.app"
built="$root/target/release/bundle/macos/$app_name"
system_dest_dir="/Applications"
user_dest_dir="$HOME/Applications"
# OAuth App の Client ID は公開情報。fork / 受入では環境変数で上書きできる。
github_client_id="${KB_GITHUB_CLIENT_ID:-Ov23li1xyWYmAMscYlj8}"
# Lindera の辞書 archive は大きいため、更新のたびに再取得しない。
lindera_cache="${LINDERA_BUILD_DICTIONARY_CACHE_DIR:-$root/target/lindera-cache}"

# 既存の配置先を優先する。/Applications 版がある場合、親 directory ではなく
# 所有している app bundle の Contents を差し替えるので管理者権限を必要としない。
# ~/Applications に別版を作ると MCP 登録先だけが古いままになるため、移動もしない。
if [[ -d "$system_dest_dir/$app_name" ]]; then
  dest_dir="$system_dest_dir"
elif [[ -d "$user_dest_dir/$app_name" ]]; then
  dest_dir="$user_dest_dir"
elif [[ -w "$system_dest_dir" ]]; then
  dest_dir="$system_dest_dir"
else
  dest_dir="$user_dest_dir"
  mkdir -p "$dest_dir"
  echo "==> /Applications に書けないので $dest_dir を使う"
fi

installed="$dest_dir/$app_name"

echo "==> ビルド(release。初回は10〜20分かかる)"
mkdir -p "$lindera_cache"
KB_GITHUB_CLIENT_ID="$github_client_id" \
  LINDERA_BUILD_DICTIONARY_CACHE_DIR="$lindera_cache" \
  npm --prefix "$root/app" run tauri -- build --bundles app

if [[ ! -d "$built" ]]; then
  echo "ビルド結果が見つからない: $built" >&2
  exit 1
fi

bash "$root/scripts/deploy-macos-bundle.sh" "$built" "$installed"

echo
echo "更新完了: $installed"
echo "Spotlight で「kb-app」と打てば開けます。"
echo "MCP の更新は、接続中の AI client で再接続すると反映されます。"
