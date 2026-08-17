#!/usr/bin/env bash
# kb-app をビルドして /Applications に安全に差し替える。
#
# 開発中の起動コマンドを毎回思い出さなくて済むように、普通のアプリとして
# Launchpad / Spotlight から開ける状態にするためのもの。GUI だけを終了し、
# AI client が子起動している `kb-app --mcp` は強制終了しない。
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
if [[ -d "$installed" ]]; then
  writable_root="$installed"
else
  writable_root="$dest_dir"
fi

if ! permission_probe="$(mktemp -d "$writable_root/.kb-app-write-test.XXXXXX" 2>/dev/null)"; then
  echo "既存アプリへ書き込めません: $writable_root" >&2
  echo "このコマンドへの書き込み権限を許可して再実行してください。" >&2
  exit 1
fi
rmdir "$permission_probe"

echo "==> ビルド(release。初回は10〜20分かかる)"
mkdir -p "$lindera_cache"
KB_GITHUB_CLIENT_ID="$github_client_id" \
  LINDERA_BUILD_DICTIONARY_CACHE_DIR="$lindera_cache" \
  npm --prefix "$root/app" run tauri -- build --bundles app

if [[ ! -d "$built" ]]; then
  echo "ビルド結果が見つからない: $built" >&2
  exit 1
fi

built_executable="$built/Contents/MacOS/kb-app"
installed_executable="$installed/Contents/MacOS/kb-app"

if [[ ! -x "$built_executable" ]]; then
  echo "ビルド結果の実行ファイルが見つからない: $built_executable" >&2
  exit 1
fi

# GUI と MCP server は同じ process 名なので、引数なしでインストール済みの
# 実行ファイルを動かしている process だけを GUI として扱う。
gui_pids=()
refresh_gui_pids() {
  local pid command
  gui_pids=()
  while IFS= read -r pid; do
    [[ -n "$pid" ]] || continue
    command="$(ps -p "$pid" -o command= 2>/dev/null || true)"
    if [[ "$command" == "$installed_executable" ]]; then
      gui_pids+=("$pid")
    fi
  done < <(pgrep -x "kb-app" 2>/dev/null || true)
}

refresh_gui_pids
if (( ${#gui_pids[@]} > 0 )); then
  echo "==> 起動中の GUI を終了する (MCP server は継続)"
  osascript -e 'quit app "kb-app"' 2>/dev/null || true

  # 通常終了を待つ。MCP process は判定対象にしない。
  for _ in $(seq 1 20); do
    refresh_gui_pids
    (( ${#gui_pids[@]} == 0 )) && break
    sleep 0.2
  done

  refresh_gui_pids
  if (( ${#gui_pids[@]} > 0 )); then
    # AppleEvent に応答しない GUI だけへ TERM を送り、さらに1秒待つ。
    kill "${gui_pids[@]}" 2>/dev/null || true
    for _ in $(seq 1 5); do
      refresh_gui_pids
      (( ${#gui_pids[@]} == 0 )) && break
      sleep 0.2
    done
  fi

  refresh_gui_pids
  if (( ${#gui_pids[@]} > 0 )); then
    echo "GUI を終了できませんでした。kb-app の画面を閉じて再実行してください。" >&2
    exit 1
  fi
fi

# 同じ filesystem 内で新旧 bundle を入れ替える。既存版では app bundle 自体を
# 移動せず、中の Contents だけを原子的に差し替える。これなら /Applications の
# 親 directory へ書けない一般 user でも、自分が所有する app を更新できる。
if [[ -d "$installed" ]]; then
  stage_root="$(mktemp -d "$installed/.kb-app-update.XXXXXX")"
  source_to_stage="$built/Contents"
  staged="$stage_root/Contents"
  staged_executable="$staged/MacOS/kb-app"
  replace_target="$installed/Contents"
  previous="$stage_root/Contents.previous"
  failed="$stage_root/Contents.failed"
else
  stage_root="$(mktemp -d "$dest_dir/.kb-app-update.XXXXXX")"
  source_to_stage="$built"
  staged="$stage_root/$app_name"
  staged_executable="$staged/Contents/MacOS/kb-app"
  replace_target="$installed"
  previous="$stage_root/kb-app.previous.app"
  failed="$stage_root/kb-app.failed.app"
fi

cleanup() {
  if [[ -d "$previous" && ! -e "$replace_target" ]]; then
    mv "$previous" "$replace_target" 2>/dev/null || true
  fi
  rm -rf "$stage_root"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' HUP TERM

echo "==> 更新版を準備"
ditto "$source_to_stage" "$staged"
if [[ ! -x "$staged_executable" ]]; then
  echo "更新版の検証に失敗しました。インストール済みアプリは変更していません。" >&2
  exit 1
fi

echo "==> $installed へ配置"
if [[ -e "$replace_target" ]]; then
  mv "$replace_target" "$previous"
fi

if ! mv "$staged" "$replace_target"; then
  echo "配置に失敗しました。旧版へ戻します。" >&2
  exit 1
fi

if ! cmp -s "$built_executable" "$installed_executable"; then
  echo "配置後の検証に失敗しました。旧版へ戻します。" >&2
  mv "$replace_target" "$failed"
  if [[ -d "$previous" ]]; then
    mv "$previous" "$replace_target"
  fi
  exit 1
fi

echo
echo "更新完了: $installed"
echo "Spotlight で「kb-app」と打てば開けます。"
echo "MCP の更新は、接続中の AI client で再接続すると反映されます。"
