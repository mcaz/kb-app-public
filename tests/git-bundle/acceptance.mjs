import assert from "node:assert/strict";
import {
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { helperNames, sha256 } from "../../scripts/prepare-git.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");

function argumentsFor(args) {
  const values = {};
  for (let index = 0; index < args.length; index += 2) {
    const key = args[index];
    if (
      !["--git", "--git-lfs", "--runtime", "--templates"].includes(key) ||
      values[key] ||
      !args[index + 1]
    )
      throw new Error(`引数が不正です: ${key}`);
    values[key] = resolve(args[index + 1]);
  }
  return values;
}

function executable(path) {
  const stat = lstatSync(path);
  if (!stat.isFile() || !(stat.mode & 0o111))
    throw new Error(`実行可能な通常ファイルではありません: ${path}`);
}

export function validateTemplates(directory) {
  for (const name of ["description", "info/exclude"]) {
    if (!lstatSync(join(directory, name)).isFile())
      throw new Error(`同梱Git templateが通常ファイルではありません: ${name}`);
  }
}

export function isolatedEnvironment(
  directory,
  runtime,
  templates = join(runtime, "templates"),
) {
  // /usr/binをPATHへ足すと、CLTのgit stubが受入を代行してしまう。
  // 個人のGit設定・認証環境を継承せず、テストに必要なOS道具だけを明示する。
  return {
    PATH: directory,
    TMPDIR: dirname(directory),
    LC_ALL: "C",
    GIT_EXEC_PATH: join(runtime, "git-core"),
    GIT_TEMPLATE_DIR: templates,
    GIT_CONFIG_NOSYSTEM: "1",
    GIT_CONFIG_SYSTEM: "/dev/null",
    GIT_CONFIG_GLOBAL: "/dev/null",
    GIT_TERMINAL_PROMPT: "0",
    GIT_SSH_COMMAND: "/usr/bin/false",
    GIT_AUTHOR_NAME: "synthetic acceptance",
    GIT_AUTHOR_EMAIL: "synthetic@example.invalid",
    GIT_COMMITTER_NAME: "synthetic acceptance",
    GIT_COMMITTER_EMAIL: "synthetic@example.invalid",
    GIT_LFS_SKIP_SMUDGE: "1",
  };
}

export function acceptance(args = process.argv.slice(2)) {
  if (process.platform !== "darwin")
    throw new Error("Git同梱の実動作受入はmacOSで実行してください");
  const options = argumentsFor(args);
  const git = options["--git"] || join(root, "app/src-tauri/binaries/git");
  const lfs =
    options["--git-lfs"] || join(root, "app/src-tauri/binaries/git-lfs");
  const runtime =
    options["--runtime"] || join(root, "app/src-tauri/git-runtime");
  const templates = options["--templates"] || join(runtime, "templates");
  validateTemplates(templates);
  for (const path of [
    git,
    lfs,
    ...helperNames.map((name) => join(runtime, "git-core", name)),
  ])
    executable(path);
  const temporary = mkdtempSync(join(tmpdir(), "kb-git-受入 space-"));
  try {
    const bin = join(temporary, "allowed-tools");
    mkdirSync(bin);
    symlinkSync(git, join(bin, "git"));
    symlinkSync(lfs, join(bin, "git-lfs"));
    for (const name of [
      "sh",
      "bash",
      "env",
      "uname",
      "sed",
      "grep",
      "cut",
      "tr",
      "mkdir",
      "rm",
      "cat",
      "wc",
      "sort",
      "printf",
      "xargs",
      "sleep",
      "head",
      "tail",
      "dirname",
      "basename",
      "readlink",
      "touch",
      "cp",
      "mv",
      "test",
    ]) {
      const path = [join("/usr/bin", name), join("/bin", name)].find(
        existsSync,
      );
      if (path) symlinkSync(path, join(bin, name));
    }
    const environment = isolatedEnvironment(bin, runtime, templates);
    const invoke = (cwd, args) => {
      const result = spawnSync(git, args, {
        cwd,
        env: environment,
        encoding: "utf8",
        timeout: 30000,
        maxBuffer: 8 * 1024 * 1024,
      });
      if (result.error || result.status !== 0)
        throw new Error(
          `同梱Git ${args.join(" ")} が失敗: ${result.error?.message || result.stderr || result.stdout}`,
        );
      return result.stdout.trim();
    };
    assert.equal(invoke(temporary, ["--version"]), "git version 2.55.0");
    assert.match(invoke(temporary, ["lfs", "version"]), /^git-lfs\/3\.7\.1/);
    assert.equal(invoke(temporary, ["--exec-path"]), join(runtime, "git-core"));
    const remote = join(temporary, "remote.git");
    const first = join(temporary, "日本語 vault A");
    const second = join(temporary, "日本語 vault B");
    invoke(temporary, ["init", "--bare", "--initial-branch=main", remote]);
    invoke(temporary, ["init", "--initial-branch=main", first]);
    invoke(first, ["lfs", "install", "--local"]);
    invoke(first, ["config", "lfs.storage", join(temporary, "lfs-a")]);
    invoke(first, ["lfs", "track", "*.bin"]);
    const payload = Buffer.alloc(900_123, 0x5a);
    writeFileSync(join(first, "payload.bin"), payload);
    writeFileSync(join(first, "note.txt"), "initial synthetic note\n");
    invoke(first, ["add", "."]);
    assert.match(
      invoke(first, ["show", ":payload.bin"]),
      /^version https:\/\/git-lfs.github.com\/spec\/v1\n/,
    );
    invoke(first, ["commit", "-m", "synthetic initial"]);
    invoke(first, ["remote", "add", "origin", remote]);
    invoke(first, ["push", "-u", "origin", "HEAD"]);
    invoke(temporary, ["clone", remote, second]);
    assert.match(
      readFileSync(join(second, "payload.bin"), "utf8"),
      /^version https:\/\/git-lfs.github.com\/spec\/v1\n/,
    );
    invoke(second, ["lfs", "install", "--local"]);
    invoke(second, ["config", "lfs.storage", join(temporary, "lfs-b")]);
    invoke(second, ["lfs", "pull"]);
    assert.equal(
      sha256(readFileSync(join(second, "payload.bin"))),
      sha256(payload),
    );
    writeFileSync(join(second, "remote-note.txt"), "remote synthetic commit\n");
    invoke(second, ["add", "remote-note.txt"]);
    invoke(second, ["commit", "-m", "synthetic remote change"]);
    invoke(second, ["push"]);
    writeFileSync(join(first, "local-note.txt"), "local synthetic commit\n");
    invoke(first, ["add", "local-note.txt"]);
    invoke(first, ["commit", "-m", "synthetic local change"]);
    writeFileSync(join(first, "note.txt"), "uncommitted synthetic edit\n");
    invoke(first, ["pull", "--rebase", "--autostash"]);
    assert.equal(
      readFileSync(join(first, "note.txt"), "utf8"),
      "uncommitted synthetic edit\n",
    );
    assert.equal(
      readFileSync(join(first, "remote-note.txt"), "utf8"),
      "remote synthetic commit\n",
    );
    assert.equal(
      readFileSync(join(first, "local-note.txt"), "utf8"),
      "local synthetic commit\n",
    );
    return {
      schema: "1.0",
      git_sha256: sha256(readFileSync(git)),
      lfs_sha256: sha256(readFileSync(lfs)),
      checks: [
        "isolated-path",
        "git-lfs-pointer",
        "local-push",
        "fresh-clone-lfs-pull",
        "rebase-autostash",
        "unicode-space-path",
      ],
      status: "passed",
      limitations: ["HTTPS認証・TLSとclean Mac実機は別途確認が必要"],
    };
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  try {
    process.stdout.write(`${JSON.stringify(acceptance(), null, 2)}\n`);
  } catch (error) {
    process.stderr.write(`Git同梱受入を停止しました: ${error.message}\n`);
    process.exitCode = 1;
  }
}
