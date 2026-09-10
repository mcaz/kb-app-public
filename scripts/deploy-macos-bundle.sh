#!/usr/bin/env bash
# build済みのkb-app bundleを、署名とrollbackを保ったまま配置する。
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
使い方: deploy-macos-bundle.sh SOURCE_APP DESTINATION_APP [--skip-launch]

SOURCE_APPをadhoc署名して検証し、DESTINATION_APPへ配置する。
--skip-launchはGUIを持たないmacOS CIでのみ使用する。
EOF
}

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "必要なmacOS commandが見つかりません: $1" >&2
    return 1
  fi
}

launch_services_error_kind() {
  local output="${1:-}"
  if [[ "$output" == *"-10822"* || "$output" == *"kLSServerCommunicationErr"* ]]; then
    printf '%s\n' "server-unavailable"
  elif [[ "$output" == *"-10827"* || "$output" == *"kLSNoExecutableErr"* ]]; then
    printf '%s\n' "executable-unavailable"
  elif [[ "$output" == *"-600"* || "$output" == *"procNotFound"* ]]; then
    # 同じ実行ファイルで動く別モードのprocess(MCP server / hook)がLaunchServicesに
    # 「起動中のkb-app」として登録されていると、openはそこへ転送しようとして-600になる
    # (2026-09-10 の反映で実測。Claude配下の `kb-app --mcp` がcgsConnection無しで登録されていた)。
    printf '%s\n' "instance-conflict"
  else
    printf '%s\n' "other"
  fi
}

report_launch_services_failure() {
  local action="$1"
  local output="${2:-}"
  case "$(launch_services_error_kind "$output")" in
    server-unavailable)
      echo "$action: LaunchServices serverに到達できません。bundleの破損とは分けて再試行してください。" >&2
      ;;
    executable-unavailable)
      echo "$action: LaunchServicesがbundleの実行ファイルを解決できません。" >&2
      ;;
    instance-conflict)
      echo "$action: LaunchServicesが別モードのprocess(MCP server / hook)を起動中のkb-appとして扱っています。" >&2
      echo "  \`lsappinfo list | grep -A4 kb-app\` で登録を確認し、AIクライアントのMCPを再接続してから再実行するか、" >&2
      echo "  \`open -n\` で新しいインスタンスとして起動してください。" >&2
      ;;
    *)
      echo "$action: LaunchServices受入に失敗しました。" >&2
      ;;
  esac
  [[ -z "$output" ]] || printf '%s\n' "$output" >&2
}

hash_file() {
  shasum -a 256 "$1" | awk '{print $1}'
}

verify_bundle() {
  local app="$1"
  local label="$2"
  local output
  if ! output="$(codesign --verify --deep --strict --verbose=2 "$app" 2>&1)"; then
    echo "${label}の署名検証に失敗しました: $app" >&2
    [[ -z "$output" ]] || printf '%s\n' "$output" >&2
    return 1
  fi
}

create_stage_root() {
  local anchor="$1"
  local source_parent="$2"
  local destination_parent="$3"
  local anchor_device parent candidate candidate_device
  local -a parents=()

  anchor_device="$(stat -f '%d' "$anchor")"
  [[ -w "$destination_parent" ]] && parents+=("$destination_parent")
  [[ -n "${TMPDIR:-}" ]] && parents+=("${TMPDIR%/}")
  parents+=("$source_parent")

  for parent in "${parents[@]}"; do
    [[ -d "$parent" && -w "$parent" ]] || continue
    candidate="$(mktemp -d "$parent/.kb-app-update.XXXXXX" 2>/dev/null || true)"
    [[ -n "$candidate" ]] || continue
    candidate_device="$(stat -f '%d' "$candidate")"
    if [[ "$candidate_device" == "$anchor_device" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
    rmdir "$candidate"
  done

  echo "配置先と同一filesystemにbundle外の一時領域を作れませんでした。" >&2
  return 1
}

gui_pids=()
refresh_gui_pids() {
  local executable="$1"
  local pid command
  gui_pids=()
  while IFS= read -r pid; do
    [[ -n "$pid" ]] || continue
    command="$(ps -p "$pid" -o command= 2>/dev/null || true)"
    if [[ "$command" == "$executable" ]]; then
      gui_pids+=("$pid")
    fi
  done < <(pgrep -x "kb-app" 2>/dev/null || true)
}

stop_gui() {
  local executable="$1"
  local quiet="${2:-false}"

  refresh_gui_pids "$executable"
  (( ${#gui_pids[@]} > 0 )) || return 0
  [[ "$quiet" == "true" ]] || echo "==> 起動中の GUI を終了する (MCP server は継続)"
  osascript -e 'quit app "kb-app"' 2>/dev/null || true

  for _ in $(seq 1 20); do
    refresh_gui_pids "$executable"
    (( ${#gui_pids[@]} == 0 )) && return 0
    sleep 0.2
  done

  refresh_gui_pids "$executable"
  if (( ${#gui_pids[@]} > 0 )); then
    kill "${gui_pids[@]}" 2>/dev/null || true
    for _ in $(seq 1 5); do
      refresh_gui_pids "$executable"
      (( ${#gui_pids[@]} == 0 )) && return 0
      sleep 0.2
    done
  fi

  echo "GUIを終了できませんでした。kb-appの画面を閉じて再実行してください。" >&2
  return 1
}

launch_and_accept() {
  local app="$1"
  local executable="$2"
  local lsregister output
  lsregister="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"

  if ! output="$("$lsregister" -f "$app" 2>&1)"; then
    report_launch_services_failure "LaunchServices登録失敗" "$output"
    return 1
  fi
  if [[ "$(launch_services_error_kind "$output")" != "other" ]]; then
    report_launch_services_failure "LaunchServices登録失敗" "$output"
    return 1
  fi

  # -n: 登録済みインスタンスへ転送せず、必ず新しいprocessを起動する。GUIはstop_guiで
  # 止めてあるので二重起動にはならず、同じ実行ファイルのMCP serverがLaunchServicesに
  # 登録されていても-600(procNotFound)で受入が落ちない。
  if ! output="$(open -n -gj "$app" 2>&1)"; then
    report_launch_services_failure "GUI起動失敗" "$output"
    return 1
  fi
  if [[ "$(launch_services_error_kind "$output")" != "other" ]]; then
    report_launch_services_failure "GUI起動失敗" "$output"
    return 1
  fi

  for _ in $(seq 1 50); do
    refresh_gui_pids "$executable"
    (( ${#gui_pids[@]} > 0 )) && break
    sleep 0.2
  done
  refresh_gui_pids "$executable"
  if (( ${#gui_pids[@]} == 0 )); then
    echo "LaunchServicesは起動を受理しましたが、GUI processを確認できませんでした。" >&2
    return 1
  fi

  # 起動直後だけ存在して終了する回帰も受入成功にしない。
  sleep 1
  refresh_gui_pids "$executable"
  if (( ${#gui_pids[@]} == 0 )); then
    echo "GUI processが起動直後に終了しました。" >&2
    return 1
  fi
}

stage_root=""
replace_target=""
previous=""
failed=""
installed=""
installed_executable=""
old_moved=0
new_moved=0
accepted=0
preserve_stage_root=0
gui_was_running=0
launch_enabled=1

rollback() {
  local rollback_failed=0 output
  (( old_moved == 1 || new_moved == 1 )) || return 0

  echo "==> 配置を受入できなかったため旧版へ戻す" >&2
  stop_gui "$installed_executable" true || true

  if (( new_moved == 1 )) && [[ -e "$replace_target" ]]; then
    if ! mv "$replace_target" "$failed"; then
      echo "更新版を退避できませんでした: $replace_target" >&2
      rollback_failed=1
    else
      new_moved=0
    fi
  fi

  if (( rollback_failed == 0 && old_moved == 1 )); then
    if ! mv "$previous" "$replace_target"; then
      echo "旧版を復元できませんでした: $previous" >&2
      rollback_failed=1
    else
      old_moved=0
    fi
  fi

  if (( rollback_failed == 0 && gui_was_running == 1 )); then
    local lsregister
    lsregister="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
    output="$("$lsregister" -f "$installed" 2>&1 || true)"
    if [[ -n "$output" ]]; then
      printf '%s\n' "$output" >&2
    fi
    # rollback後の再起動も新しいインスタンスとして起動する(理由は launch_and_accept と同じ)。
    open -n -g "$installed" >/dev/null 2>&1 || true
  fi

  if (( rollback_failed == 1 )); then
    preserve_stage_root=1
    echo "rollback用データを保持しました: $stage_root" >&2
    return 1
  fi
  return 0
}

cleanup() {
  local status=$?
  trap - EXIT INT HUP TERM
  set +e

  if (( accepted == 0 )); then
    rollback || status=1
  fi
  if [[ -n "$stage_root" && -d "$stage_root" && $preserve_stage_root -eq 0 ]]; then
    rm -rf "$stage_root"
  fi
  exit "$status"
}

main() {
  if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "このスクリプトはmacOS用です(現在: $(uname -s))" >&2
    return 1
  fi
  if (( $# < 2 || $# > 3 )); then
    usage
    return 2
  fi

  local source_input="$1"
  local destination_input="$2"
  local option="${3:-}"
  if [[ -n "$option" && "$option" != "--skip-launch" ]]; then
    usage
    return 2
  fi
  [[ "$option" == "--skip-launch" ]] && launch_enabled=0

  local command
  for command in codesign ditto pgrep ps shasum stat; do
    require_command "$command" || return 1
  done
  if (( launch_enabled == 1 )); then
    require_command open || return 1
    require_command osascript || return 1
    if [[ ! -x "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister" ]]; then
      echo "LaunchServices登録commandが見つかりません。" >&2
      return 1
    fi
  fi

  local source_parent destination_parent source_app anchor candidate_app
  local candidate_executable expected_hash actual_hash staged
  source_parent="$(cd "$(dirname "$source_input")" && pwd)"
  source_app="$source_parent/$(basename "$source_input")"
  destination_parent="$(cd "$(dirname "$destination_input")" && pwd)"
  installed="$destination_parent/$(basename "$destination_input")"
  installed_executable="$installed/Contents/MacOS/kb-app"

  if [[ ! -d "$source_app" || ! -x "$source_app/Contents/MacOS/kb-app" ]]; then
    echo "配置候補のapp bundleが不正です: $source_app" >&2
    return 1
  fi
  if [[ -e "$installed" && ! -d "$installed" ]]; then
    echo "配置先がapp bundleではありません: $installed" >&2
    return 1
  fi
  if [[ "$source_app" == "$installed" ]]; then
    echo "配置候補と配置先は別pathにしてください。" >&2
    return 1
  fi
  if [[ -d "$installed" && ( ! -w "$installed" || ! -x "$installed" ) ]]; then
    echo "既存アプリへ書き込めません: $installed" >&2
    return 1
  fi
  if [[ ! -d "$installed" && ( ! -w "$destination_parent" || ! -x "$destination_parent" ) ]]; then
    echo "配置先directoryへ書き込めません: $destination_parent" >&2
    return 1
  fi

  anchor="$destination_parent"
  [[ -d "$installed" ]] && anchor="$installed"
  if ! stage_root="$(create_stage_root "$anchor" "$source_parent" "$destination_parent")"; then
    return 1
  fi
  candidate_app="$stage_root/$(basename "$installed")"

  trap cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' HUP TERM

  echo "==> bundle外へ更新候補を準備"
  ditto "$source_app" "$candidate_app"
  candidate_executable="$candidate_app/Contents/MacOS/kb-app"
  if [[ ! -x "$candidate_executable" ]]; then
    echo "更新候補の実行ファイルが見つかりません: $candidate_executable" >&2
    return 1
  fi

  echo "==> 更新候補をadhoc署名して厳格検証"
  if ! codesign --force --deep --sign - "$candidate_app"; then
    echo "更新候補のadhoc署名に失敗しました。インストール済みアプリは変更していません。" >&2
    return 1
  fi
  verify_bundle "$candidate_app" "更新候補" || return 1
  expected_hash="$(hash_file "$candidate_executable")"

  refresh_gui_pids "$installed_executable"
  if (( ${#gui_pids[@]} > 0 )); then
    gui_was_running=1
    stop_gui "$installed_executable" || return 1
  fi

  if [[ -d "$installed" ]]; then
    if [[ ! -d "$installed/Contents" ]]; then
      echo "既存アプリにContentsがありません: $installed" >&2
      return 1
    fi
    staged="$candidate_app/Contents"
    replace_target="$installed/Contents"
    previous="$stage_root/Contents.previous"
    failed="$stage_root/Contents.failed"
  else
    staged="$candidate_app"
    replace_target="$installed"
    previous="$stage_root/kb-app.previous.app"
    failed="$stage_root/kb-app.failed.app"
  fi

  echo "==> $installed へ配置"
  if [[ -e "$replace_target" ]]; then
    if ! mv "$replace_target" "$previous"; then
      echo "旧版をrollback領域へ退避できませんでした。" >&2
      return 1
    fi
    old_moved=1
  fi
  if ! mv "$staged" "$replace_target"; then
    echo "更新版を配置できませんでした。" >&2
    return 1
  fi
  new_moved=1

  verify_bundle "$installed" "配置後bundle" || return 1
  actual_hash="$(hash_file "$installed_executable")"
  if [[ "$actual_hash" != "$expected_hash" ]]; then
    echo "配置後の実行ファイルhashが更新候補と一致しません。" >&2
    echo "expected: $expected_hash" >&2
    echo "actual:   $actual_hash" >&2
    return 1
  fi

  if (( launch_enabled == 1 )); then
    echo "==> LaunchServices登録とGUI起動を受入"
    launch_and_accept "$installed" "$installed_executable" || return 1
    if (( gui_was_running == 0 )); then
      stop_gui "$installed_executable" true || return 1
    fi
  fi

  accepted=1
  echo "配置受入完了: $installed"
  echo "SHA-256: $actual_hash"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
