import { createHash } from "node:crypto";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
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

if (!validBinary(sidecar)) {
  const temporary = mkdtempSync(join(tmpdir(), "kb-app-git-lfs-"));
  try {
    const url = `${manifest.releaseBase}/${asset.asset}`;
    const response = await fetch(url);
    if (!response.ok) {
      throw new Error(`Git LFSを取得できません(${response.status}): ${url}`);
    }
    const archiveBytes = Buffer.from(await response.arrayBuffer());
    const digest = createHash("sha256").update(archiveBytes).digest("hex");
    if (digest !== asset.sha256) {
      throw new Error(
        `Git LFS archiveのSHA-256が一致しません: expected=${asset.sha256} actual=${digest}`,
      );
    }

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
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}

// 受入・開発実行ではGitのfilter-processがbasenameで探索する。Tauriはtarget suffix
// 付きのsidecarを配布時にbasenameへ戻すため、build treeにも同じ別名を用意する。
copyFileSync(sidecar, developmentAlias);
if (process.platform !== "win32") chmodSync(developmentAlias, 0o755);
process.stdout.write(`${sidecar}\n`);
