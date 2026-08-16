import { mkdtempSync, readFileSync, readdirSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

function findFiles(root, predicate) {
  const found = [];
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const path = join(root, entry.name);
    if (entry.isDirectory()) found.push(...findFiles(path, predicate));
    else if (entry.isFile() && predicate(path)) found.push(path);
  }
  return found;
}

const repositoryRoot = resolve(import.meta.dirname, "..");
const bundleDirectory = join(repositoryRoot, "target", "release", "bundle", "deb");
const packages = findFiles(bundleDirectory, (path) => path.endsWith(".deb"));
if (packages.length !== 1) {
  throw new Error(`検査対象のdebは1件である必要があります: ${packages.join(", ")}`);
}

const extracted = mkdtempSync(join(tmpdir(), "kb-app-deb-"));
try {
  const unpack = spawnSync("dpkg-deb", ["--extract", packages[0], extracted], {
    encoding: "utf8",
  });
  if (unpack.status !== 0) throw new Error(unpack.stderr || unpack.stdout);

  const binaries = findFiles(
    extracted,
    (path) => basename(path) === "git-lfs" && (statSync(path).mode & 0o111) !== 0,
  );
  if (binaries.length !== 1) {
    throw new Error(`配布物の実行可能git-lfsは1件必要です: ${binaries.join(", ")}`);
  }
  const version = spawnSync(binaries[0], ["version"], { encoding: "utf8" });
  if (
    version.status !== 0 ||
    !`${version.stdout}${version.stderr}`.includes("git-lfs/3.7.1")
  ) {
    throw new Error(`配布物のgit-lfs versionが不正です: ${version.stderr || version.stdout}`);
  }

  const licenses = findFiles(
    extracted,
    (path) => basename(path) === "git-lfs-LICENSE.md",
  );
  if (
    licenses.length !== 1 ||
    !readFileSync(licenses[0], "utf8").includes("Git LFS contributors")
  ) {
    throw new Error("配布物にGit LFSのライセンスがありません");
  }
  process.stdout.write(`verified ${packages[0]} (${version.stdout.trim()})\n`);
} finally {
  rmSync(extracted, { recursive: true, force: true });
}
