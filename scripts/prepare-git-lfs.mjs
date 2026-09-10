import { createHash } from "node:crypto";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(scriptDirectory, "..");
const manifest = JSON.parse(
  readFileSync(join(scriptDirectory, "git-lfs-assets.json"), "utf8"),
);

function argument(name) {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : undefined;
}

function hostTarget() {
  const tauriTarget = (() => {
    const architecture = process.env.TAURI_ENV_ARCH;
    switch (process.env.TAURI_ENV_PLATFORM) {
      case "darwin":
        return architecture ? `${architecture}-apple-darwin` : undefined;
      case "linux":
        return architecture ? `${architecture}-unknown-linux-gnu` : undefined;
      case "windows":
        return architecture ? `${architecture}-pc-windows-msvc` : undefined;
      default:
        return undefined;
    }
  })();
  const explicit =
    argument("--target") ||
    process.env.KB_TAURI_TARGET ||
    process.env.CARGO_BUILD_TARGET ||
    tauriTarget;
  if (explicit) return explicit;
  const result = spawnSync("rustc", ["-vV"], { encoding: "utf8" });
  if (result.status !== 0) {
    throw new Error(`rustcからbuild targetを取得できません: ${result.stderr}`);
  }
  const host = result.stdout.match(/^host: (.+)$/m)?.[1];
  if (!host) throw new Error("rustc -vVにhostがありません");
  return host;
}

function run(program, args) {
  const result = spawnSync(program, args, { encoding: "utf8" });
  if (result.status !== 0) {
    throw new Error(
      `${program} ${args.join(" ")} に失敗しました:\n${result.stderr || result.stdout}`,
    );
  }
}

function findFile(root, filename) {
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const path = join(root, entry.name);
    if (entry.isDirectory()) {
      const found = findFile(path, filename);
      if (found) return found;
    } else if (entry.isFile() && entry.name === filename) {
      return path;
    }
  }
  return undefined;
}

function validBinary(path) {
  if (!existsSync(path)) return false;
  const result = spawnSync(path, ["version"], { encoding: "utf8" });
  return (
    result.status === 0 &&
    `${result.stdout}${result.stderr}`.includes(`git-lfs/${manifest.version}`)
  );
}

const digestOf = (bytes) => createHash("sha256").update(bytes).digest("hex");
const retryableStatus = new Set([408, 429, 500, 502, 503, 504]);

async function requestArchive(url, headers, label) {
  // 2026-09-09: 公式配布URLのHTTP 500で準備が止まった。恒久エラーは繰り返さない。
  const attempts = 3;
  let reason;
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    let retryable = true;
    try {
      // headerだけでなくbodyの受信にも同じ期限を適用する。
      const response = await fetch(url, {
        headers,
        redirect: "follow",
        signal: AbortSignal.timeout(30_000),
      });
      if (response.ok)
        return { bytes: Buffer.from(await response.arrayBuffer()) };
      reason = `HTTP ${response.status}`;
      retryable = retryableStatus.has(response.status);
      await response.body?.cancel().catch(() => {});
    } catch (error) {
      const code = error.cause?.code ?? error.code;
      reason = ["AbortError", "TimeoutError"].includes(error.name)
        ? "30秒の取得期限を超えました"
        : /^[A-Z0-9_]+$/.test(code ?? "")
          ? `通信失敗 (${code})`
          : "通信またはbody受信に失敗しました";
    }
    process.stderr.write(
      `Git LFS ${label} ${attempt}/${attempts}: ${reason}\n`,
    );
    if (!retryable) return { reason, retryable: false };
    if (attempt < attempts)
      await new Promise((done) => setTimeout(done, 500 * attempt));
  }
  return { reason, retryable: true };
}

async function downloadArchive(url, asset) {
  let result = await requestArchive(url, {}, "公式配布URL");
  if (
    !result.bytes &&
    result.retryable &&
    Number.isSafeInteger(asset.releaseAssetId) &&
    asset.releaseAssetId > 0
  ) {
    const apiUrl = `https://api.github.com/repos/git-lfs/git-lfs/releases/assets/${asset.releaseAssetId}`;
    process.stderr.write(
      "Git LFSの公式配布URLで取得できないため、公式GitHub APIへ切り替えます。\n",
    );
    result = await requestArchive(
      apiUrl,
      {
        Accept: "application/octet-stream",
        "X-GitHub-Api-Version": "2022-11-28",
        "User-Agent": "kb-app-sidecar-prepare",
      },
      "公式GitHub API",
    );
  }
  if (!result.bytes)
    throw new Error(
      `Git LFSを取得できません: ${result.reason}。cache・sidecarは更新していません。`,
    );
  const digest = digestOf(result.bytes);
  // 別bytesを一時障害と読み替えず、他経路へ切り替えて不一致を隠すこともしない。
  if (digest !== asset.sha256) {
    throw new Error(
      `Git LFS archiveのSHA-256が一致しません: expected=${asset.sha256} actual=${digest}`,
    );
  }
  return result.bytes;
}

const target = hostTarget();
const asset = manifest.targets[target];
if (!asset) {
  throw new Error(
    `git-lfs sidecarを用意していないtargetです: ${target}\n` +
      `対応target: ${Object.keys(manifest.targets).join(", ")}\n` +
      "cross buildではKB_TAURI_TARGETもbuild targetと同じ値にしてください。",
  );
}

const extension = target.includes("windows") ? ".exe" : "";
const binaryName = `git-lfs${extension}`;
const outputDirectory = join(repositoryRoot, "app", "src-tauri", "binaries");
const sidecar = join(outputDirectory, `git-lfs-${target}${extension}`);
const developmentAlias = join(outputDirectory, binaryName);
mkdirSync(outputDirectory, { recursive: true });

const url = `${manifest.releaseBase}/${asset.asset}`;
const cache =
  process.env.KB_SIDECAR_CACHE_DIR ||
  join(repositoryRoot, "target", "sidecar-cache");
mkdirSync(cache, { recursive: true });
const cachedArchive = join(cache, asset.asset);
if (existsSync(cachedArchive) && !lstatSync(cachedArchive).isFile()) {
  throw new Error("Git LFS cacheが通常ファイルではありません");
}
let archiveBytes = existsSync(cachedArchive)
  ? readFileSync(cachedArchive)
  : null;
// 版文字列を自己申告する既存binaryを信用せず、固定archiveから毎回同じ実体を用意する。
if (!archiveBytes || digestOf(archiveBytes) !== asset.sha256) {
  archiveBytes = await downloadArchive(url, asset);
  // 検証済みbytesだけを同じfilesystem内で確定し、中断した取得をcacheとして残さない。
  const downloadDirectory = mkdtempSync(join(cache, ".git-lfs-download-"));
  try {
    const download = join(downloadDirectory, "archive");
    writeFileSync(download, archiveBytes, { flag: "wx" });
    renameSync(download, cachedArchive);
  } finally {
    rmSync(downloadDirectory, { recursive: true, force: true });
  }
}
const temporary = mkdtempSync(join(tmpdir(), "kb-app-git-lfs-"));
try {
  const archive = join(temporary, basename(asset.asset));
  const extracted = join(temporary, "extracted");
  writeFileSync(archive, archiveBytes);
  mkdirSync(extracted);
  if (asset.archive === "tar.gz") {
    run("tar", ["-xzf", archive, "-C", extracted]);
  } else if (process.platform === "win32") {
    run("tar.exe", ["-xf", archive, "-C", extracted]);
  } else {
    run("unzip", ["-q", archive, "-d", extracted]);
  }

  const extractedBinary = findFile(extracted, binaryName);
  if (!extractedBinary) {
    throw new Error(`${asset.asset}に${binaryName}がありません`);
  }
  copyFileSync(extractedBinary, sidecar);
  if (process.platform !== "win32") chmodSync(sidecar, 0o755);
  if (!validBinary(sidecar)) {
    throw new Error(`展開したGit LFSを実行できません: ${sidecar}`);
  }
  writeFileSync(
    `${sidecar}.provenance.json`,
    `${JSON.stringify(
      {
        schema: 1,
        version: manifest.version,
        target,
        archive_url: url,
        archive_sha256: asset.sha256,
        binary_sha256: digestOf(readFileSync(sidecar)),
      },
      null,
      2,
    )}\n`,
  );
} finally {
  rmSync(temporary, { recursive: true, force: true });
}

// 受入・開発実行ではGitのfilter-processがbasenameで探索する。Tauriはtarget suffix
// 付きのsidecarを配布時にbasenameへ戻すため、build treeにも同じ別名を用意する。
copyFileSync(sidecar, developmentAlias);
if (process.platform !== "win32") chmodSync(developmentAlias, 0o755);
process.stdout.write(`${sidecar}\n`);
