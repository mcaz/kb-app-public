#!/usr/bin/env bash
# kb-app をビルドして /Applications に置く。
#
# 開発中の起動コマンドを毎回思い出さなくて済むように、普通のアプリとして
# Launchpad / Spotlight から開ける状態にするためのもの。
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "このスクリプトは macOS 用です(現在: $(uname -s))" >&2
  exit 1
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
app_name="kb-app.app"
built="$root/target/release/bundle/macos/$app_name"
dest_dir="/Applications"

echo "==> ビルド(release。初回は10〜20分かかる)"
npm --prefix "$root/app" run tauri build

if [[ ! -d "$built" ]]; then
  echo "ビルド結果が見つからない: $built" >&2
  exit 1
fi

if [[ ! -w "$dest_dir" ]]; then
  dest_dir="$HOME/Applications"
  mkdir -p "$dest_dir"
  echo "==> /Applications に書けないので $dest_dir を使う"
fi

# 起動中だと差し替えに失敗するので先に終了させる
if pgrep -x "kb-app" >/dev/null 2>&1; then
  echo "==> 起動中の kb-app を終了する"
  osascript -e 'quit app "kb-app"' 2>/dev/null || pkill -x "kb-app" || true
  # 終了を待つ(最大5秒)
  for _ in $(seq 1 25); do
    pgrep -x "kb-app" >/dev/null 2>&1 || break
    sleep 0.2
  done
fi

echo "==> $dest_dir へ配置"
rm -rf "${dest_dir:?}/$app_name"
cp -R "$built" "$dest_dir/$app_name"

echo
echo "完了: $dest_dir/$app_name"
echo "Spotlight で「kb-app」と打てば開けます。"
