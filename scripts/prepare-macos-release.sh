#!/usr/bin/env bash
# 手動workflowとローカルで同じ候補生成を使う。既存アプリ・Vault・配布先へは書き込まない。
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
[[ "$(uname -s)" == Darwin ]] || { echo 'macOS上で実行してください' >&2; exit 1; }
: "${RELEASE_MODE:?}" "${RELEASE_VERSION:?}" "${RELEASE_COMMIT:?}" "${RELEASE_TARGET:?}" "${RELEASE_MINIMUM_OS:?}"
# OAuthの公開Client IDを埋め込まずに、接続不能な配布物を作らない。
[[ "${KB_GITHUB_CLIENT_ID:-}" =~ [^[:space:]] ]] || {
  echo 'KB_GITHUB_CLIENT_IDにGitHub OAuth Appの公開Client IDを設定してください。' >&2
  exit 1
}
case "$RELEASE_MODE" in candidate|release) ;; *) echo 'modeが不正です' >&2; exit 1 ;; esac
case "$RELEASE_TARGET" in aarch64-apple-darwin|x86_64-apple-darwin) ;; *) echo 'targetが不正です' >&2; exit 1 ;; esac
export KB_TAURI_TARGET="$RELEASE_TARGET" MACOSX_DEPLOYMENT_TARGET="$RELEASE_MINIMUM_OS"
node --input-type=module <<'JS'
import { validateEnvironment } from './scripts/prepare-macos-release.mjs';
validateEnvironment(process.env.RELEASE_MODE, process.env);
JS
# sourceとは別の一時領域へoverrideを書き、Gitの差分確認と秘密情報の保持範囲を混ぜない。
release_plan="$(mktemp -d "${TMPDIR:-/tmp}/kb-release-plan.XXXXXX")"
trap 'rm -rf "$release_plan"' EXIT
# 公式取得物と署名前の実bytesを計画へ固定してから、Tauriへ同じsidecarを渡す。
npm --prefix app run prepare:git-lfs
npm --prefix app run prepare:git
node scripts/prepare-macos-release.mjs \
  --mode "$RELEASE_MODE" --version "$RELEASE_VERSION" \
  --source-commit "$RELEASE_COMMIT" --target "$RELEASE_TARGET" \
  --minimum-system-version "$RELEASE_MINIMUM_OS" --output "$release_plan"

# 稼働MCPが参照し得る通常target/releaseを触らず、専用compile cacheを再利用する。
release_target_dir="$root/target/macos-release"
export KB_RELEASE_PLAN="$release_plan" KB_RELEASE_TARGET_DIR="$release_target_dir"
node --input-type=module <<'JS'
import { rmSync } from 'node:fs';
import { resolve } from 'node:path';
import { TARGETS } from './scripts/prepare-macos-release.mjs';
if (!Object.hasOwn(TARGETS, process.env.RELEASE_TARGET)) throw new Error('targetが不正です');
// 新しいbuildが失敗した時に、前回のDMGを成功成果物へ転記しない。
rmSync(resolve(process.env.KB_RELEASE_TARGET_DIR, process.env.RELEASE_TARGET, 'release/bundle'), { recursive: true, force: true });
JS
# --ciだけではDMGのFinder自動化は省略されないため、bundlerの環境も固定する。
CARGO_TARGET_DIR="$release_target_dir" KB_TAURI_TARGET="$RELEASE_TARGET" \
  CI=true TAURI_BUNDLER_DMG_IGNORE_CI=false \
  npm --prefix app run tauri -- build --ci --target "$RELEASE_TARGET" --bundles app,dmg \
  --config "$release_plan/tauri-release.json"

node --input-type=module <<'JS'
import { readFileSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { run, sha256 } from './scripts/prepare-macos-release.mjs';
import { verifyArtifacts } from './scripts/verify-macos-release.mjs';
const plan = process.env.KB_RELEASE_PLAN;
const planDocument = JSON.parse(readFileSync(resolve(plan, 'plan.json')));
const updaterVerification = {};
if (planDocument.updater === 'configured') {
  // 同じclean sourceから独立verifierをbuildし、実行直前のbytesを固定する。
  // 更新元versionは構造検査用の入力であり、新旧MCP共存receiptの代用ではない。
  if (!process.env.RELEASE_UPDATE_FROM_VERSION) throw new Error('RELEASE_UPDATE_FROM_VERSIONが必要です');
  if (run('git', ['rev-parse', 'HEAD']) !== planDocument.source_commit || run('git', ['status', '--porcelain', '--untracked-files=all']))
    throw new Error('verifierのbuild元が固定sourceと一致しません');
  const host = run('rustc', ['-vV']).match(/^host: (aarch64-apple-darwin|x86_64-apple-darwin)$/m)?.[1];
  if (!host) throw new Error('verifierを実行するmacOS hostが不明です');
  const verifierTarget = resolve(process.env.KB_RELEASE_TARGET_DIR, 'updater-verifier');
  run('cargo', ['build', '--release', '--locked', '-p', 'kb-cli', '--bin', 'kb', '--target', host,
    '--target-dir', verifierTarget], { timeout: 1_800_000 });
  const verifier = resolve(verifierTarget, host, 'release/kb');
  updaterVerification['updater-verifier'] = verifier;
  updaterVerification['updater-verifier-sha256'] = sha256(readFileSync(verifier));
  updaterVerification['updater-current-version'] = process.env.RELEASE_UPDATE_FROM_VERSION;
}
const bundle = resolve(process.env.KB_RELEASE_TARGET_DIR, process.env.RELEASE_TARGET, 'release/bundle');
const dmgs = readdirSync(resolve(bundle, 'dmg')).filter(name => name.endsWith('.dmg'));
if (dmgs.length !== 1) throw new Error('候補DMGが一意ではありません');
const dmg = resolve(bundle, 'dmg', dmgs[0]);
if (process.env.RELEASE_MODE === 'release') {
  // appはTauriの公証を使う。DMGも自身の署名とticketを検証できる形へ確定する。
  run('/usr/bin/codesign', ['--force', '--timestamp', '--sign', process.env.APPLE_SIGNING_IDENTITY, dmg]);
  const response = JSON.parse(run('/usr/bin/xcrun', ['notarytool', 'submit', dmg,
    '--key', process.env.APPLE_API_KEY_PATH, '--key-id', process.env.APPLE_API_KEY,
    '--issuer', process.env.APPLE_API_ISSUER, '--wait', '--output-format', 'json'], { timeout: 1_800_000 }));
  if (response.status !== 'Accepted') throw new Error('DMGの公証が受理されませんでした');
  run('/usr/bin/xcrun', ['stapler', 'staple', dmg]);
}
const report = verifyArtifacts({ plan: resolve(plan, 'plan.json'),
  app: resolve(bundle, 'macos/kb-app.app'), dmg, output: resolve('release-output'), ...updaterVerification });
if (report.mode === 'release' && !report.distribution_verified) {
  throw new Error(`配布blockerが残っています: ${report.blockers.join(', ')}`);
}
process.stdout.write('候補生成を完了しました。公開・更新配信・実機受入は実施していません。\n');
JS
