#!/usr/bin/env bash
# 2026-08-20に、実行ファイル一致だけではresource seal破損を検出できなかった回帰を守る。
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
deploy="$root/scripts/deploy-macos-bundle.sh"
fixture_plist="$root/tests/macos-deploy/Info.plist"
temporary="$(mktemp -d "${TMPDIR%/}/kb-app-deploy-test.XXXXXX")"
trap 'rm -rf "$temporary"' EXIT

make_app() {
  local app="$1"
  local executable="$2"
  mkdir -p "$app/Contents/MacOS"
  cp "$fixture_plist" "$app/Contents/Info.plist"
  cp "$executable" "$app/Contents/MacOS/kb-app"
  chmod 755 "$app/Contents/MacOS/kb-app"
}

candidate="$temporary/source/kb-app.app"
installed="$temporary/destination/kb-app.app"
mkdir -p "$(dirname "$candidate")" "$(dirname "$installed")"

# 未署名bundleから開始し、配置経路自身が署名することを検査する。
make_app "$candidate" /usr/bin/false
if codesign --verify --deep --strict "$candidate" >/dev/null 2>&1; then
  echo "fixtureが意図せず署名済みです" >&2
  exit 1
fi

bash "$deploy" "$candidate" "$installed" --skip-launch
codesign --verify --deep --strict "$installed"
if "$installed/Contents/MacOS/kb-app"; then
  echo "更新候補の実行ファイルが配置されていません" >&2
  exit 1
fi
if find "$installed" -maxdepth 1 -name '.kb-app-update.*' -print -quit | grep -q .; then
  echo "app bundle内に一時領域が残っています" >&2
  exit 1
fi

# 配置後の厳格検証だけを一度失敗させ、旧Contentsが復元されることを検査する。
old_hash="$(shasum -a 256 "$installed/Contents/MacOS/kb-app" | awk '{print $1}')"
rollback_candidate="$temporary/rollback-source/kb-app.app"
mkdir -p "$(dirname "$rollback_candidate")"
make_app "$rollback_candidate" /usr/bin/true

fake_bin="$root/tests/macos-deploy/fake-bin"

# 候補署名の失敗はGUI停止やContents交換より前に止まり、旧版へ触れない。
sign_failure_candidate="$temporary/sign-failure-source/kb-app.app"
mkdir -p "$(dirname "$sign_failure_candidate")"
make_app "$sign_failure_candidate" /usr/bin/true
if PATH="$fake_bin:$PATH" \
  KB_FAIL_SIGN=1 \
  KB_FAIL_MARKER="$temporary/sign-failed" \
  bash "$deploy" "$sign_failure_candidate" "$installed" --skip-launch; then
  echo "候補署名の強制失敗を成功扱いしました" >&2
  exit 1
fi
after_sign_failure_hash="$(shasum -a 256 "$installed/Contents/MacOS/kb-app" | awk '{print $1}')"
if [[ "$after_sign_failure_hash" != "$old_hash" ]]; then
  echo "候補署名の失敗前にインストール済みアプリを変更しました" >&2
  exit 1
fi

if PATH="$fake_bin:$PATH" \
  KB_FAIL_VERIFY_PATH="$installed" \
  KB_FAIL_MARKER="$temporary/post-verify-failed" \
  bash "$deploy" "$rollback_candidate" "$installed" --skip-launch; then
  echo "配置後検証の強制失敗を成功扱いしました" >&2
  exit 1
fi

restored_hash="$(shasum -a 256 "$installed/Contents/MacOS/kb-app" | awk '{print $1}')"
if [[ "$restored_hash" != "$old_hash" ]]; then
  echo "配置後検証の失敗時に旧版が復元されませんでした" >&2
  exit 1
fi
codesign --verify --deep --strict "$installed"

# LaunchServices server停止とbundle不備を同じ原因として報告しない。
source "$deploy"
[[ "$(launch_services_error_kind 'kLSServerCommunicationErr (-10822)')" == "server-unavailable" ]]
[[ "$(launch_services_error_kind 'kLSNoExecutableErr (-10827)')" == "executable-unavailable" ]]

echo "macOS配備の署名・bundle外stage・rollback検査: PASS"
