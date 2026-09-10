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
import { availableParallelism } from "node:os";
import { basename, dirname, join, relative, resolve, sep } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const scriptPath = fileURLToPath(import.meta.url);
const repositoryRoot = resolve(dirname(scriptPath), "..");
const manifestPath = join(dirname(scriptPath), "git-assets.json");
export const helperNames = [
  "git-remote-http",
  "git-remote-https",
  "git-upload-pack",
  "git-receive-pack",
  "git-http-fetch",
];

export function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function regularBytes(path) {
  if (!lstatSync(path).isFile())
    throw new Error(`通常ファイルではありません: ${path}`);
  return readFileSync(path);
}

export function verifyArchive(bytes, manifest) {
  const actual = sha256(bytes);
  if (actual !== manifest.source.sha256) {
    throw new Error(
      `公式Git archiveのSHA-256不一致: expected=${manifest.source.sha256} actual=${actual}`,
    );
  }
}

export function parseOptions(args, environment, manifest) {
  const values = {};
  for (let index = 0; index < args.length; index += 1) {
    const key = args[index];
    if (["--fetch-only", "--verify-only"].includes(key)) {
      if (values[key]) throw new Error(`引数が重複しています: ${key}`);
      values[key] = true;
    } else if (
      ["--target", "--minimum-system-version", "--source-archive"].includes(key)
    ) {
      if (values[key] || !args[index + 1] || args[index + 1].startsWith("--"))
        throw new Error(`引数が不正です: ${key}`);
      values[key] = args[++index];
    } else {
      throw new Error(`未対応の引数です: ${key}`);
    }
  }
  if (values["--fetch-only"] && values["--verify-only"])
    throw new Error("fetch-onlyとverify-onlyは同時に指定できません");
  const host =
    process.arch === "arm64" ? "aarch64-apple-darwin" : "x86_64-apple-darwin";
  const target =
    values["--target"] ||
    environment.KB_TAURI_TARGET ||
    environment.CARGO_BUILD_TARGET ||
    host;
  if (!Object.hasOwn(manifest.targets, target))
    throw new Error(`Git同梱の未対応targetです: ${target}`);
  const minimum =
    values["--minimum-system-version"] ||
    environment.MACOSX_DEPLOYMENT_TARGET ||
    manifest.minimumSystemVersion;
  if (
    !/^\d+\.\d+(?:\.\d+)?$/.test(minimum) ||
    Number(minimum.split(".")[0]) <
      Number(manifest.minimumSystemVersion.split(".")[0])
  ) {
    throw new Error(`GitのmacOSビルド下限が不正です: ${minimum}`);
  }
  return {
    target,
    minimum,
    sourceArchive:
      values["--source-archive"] || environment.KB_GIT_SOURCE_ARCHIVE,
    fetchOnly: !!values["--fetch-only"],
    verifyOnly: !!values["--verify-only"],
  };
}

export function fileInventory(root, exclude = []) {
  const result = {};
  function visit(directory) {
    for (const name of readdirSync(directory).sort()) {
      const path = join(directory, name);
      const key = relative(root, path).split(sep).join("/");
      const stat = lstatSync(path);
      if (stat.isSymbolicLink())
        throw new Error(`Git生成物にsymlinkがあります: ${key}`);
      if (stat.isDirectory()) visit(path);
      else if (!stat.isFile())
        throw new Error(`Git生成物に未対応の項目があります: ${key}`);
      else if (!exclude.includes(key)) result[key] = sha256(regularBytes(path));
    }
  }
  visit(root);
  return result;
}

export function validateArchiveMembers(listing, version) {
  const root = `git-${version}`;
  const entries = listing.trim().split("\n");
  if (!entries.length) throw new Error("Git archiveが空です");
  for (const entry of entries) {
    const parts = entry.replace(/\/$/, "").split("/");
    if (
      parts[0] !== root ||
      parts.some(
        (part) => !part || part === "." || part === ".." || part.includes("\\"),
      )
    ) {
      throw new Error(`Git archiveの相対pathが不正です: ${entry}`);
    }
  }
}

function run(program, args, options = {}) {
  const result = spawnSync(program, args, {
    encoding: "utf8",
    maxBuffer: 32 * 1024 * 1024,
    ...options,
  });
  if (result.error || result.status !== 0)
    throw new Error(
      `${basename(program)}が失敗しました: ${result.error?.message || result.stderr || result.stdout}`,
    );
  return result.stdout.trim();
}

export function verifySystemDependencies(output) {
  const libraries = output
    .split("\n")
    .filter((line) => /^\s+\S/.test(line))
    .map((line) => line.trim().split(" (")[0]);
  if (
    !libraries.length ||
    libraries.some(
      (path) =>
        !path.startsWith("/usr/lib/") &&
        !path.startsWith("/System/Library/Frameworks/"),
    )
  ) {
    throw new Error(
      `GitにOS標準以外のdylib依存があります: ${libraries.join(", ")}`,
    );
  }
  return libraries;
}

function copyRegular(source, destination, executable = false) {
  regularBytes(source);
  if (existsSync(destination) && !lstatSync(destination).isFile())
    throw new Error(`出力先が通常ファイルではありません: ${destination}`);
  mkdirSync(dirname(destination), { recursive: true });
  copyFileSync(source, destination);
  chmodSync(destination, executable ? 0o755 : 0o644);
}

export function syncDevelopmentAlias(sidecar, alias) {
  if (
    !existsSync(alias) ||
    !(lstatSync(alias).mode & 0o111) ||
    sha256(regularBytes(alias)) !== sha256(regularBytes(sidecar))
  )
    copyRegular(sidecar, alias, true);
}

function copyLicenses(source, destination) {
  function visit(directory) {
    for (const name of readdirSync(directory).sort()) {
      const path = join(directory, name);
      const stat = lstatSync(path);
      if (stat.isDirectory()) visit(path);
      else if (
        stat.isFile() &&
        /^(COPYING|LICENSE|NOTICE)([.-].*)?$/i.test(name)
      )
        copyRegular(path, join(destination, relative(source, path)));
    }
  }
  visit(source);
  if (!existsSync(join(destination, "COPYING")))
    throw new Error("公式GitのCOPYINGがありません");
}

export function verifyPrepared(runtime, binary, expected) {
  const provenance = JSON.parse(regularBytes(join(runtime, "provenance.json")));
  for (const key of [
    "schema",
    "version",
    "target",
    "minimum_system_version",
    "recipe_sha256",
  ]) {
    if (provenance[key] !== expected[key])
      throw new Error(`Git provenanceの${key}が一致しません`);
  }
  if (JSON.stringify(provenance.source) !== JSON.stringify(expected.source))
    throw new Error("Git sourceの出典が一致しません");
  verifyArchive(regularBytes(join(runtime, "source", expected.source.asset)), {
    source: expected.source,
  });
  for (const [name, hash] of Object.entries(expected.inputs)) {
    if (sha256(regularBytes(join(runtime, "source", name))) !== hash)
      throw new Error(`Git build recipeが一致しません: ${name}`);
  }
  for (const path of [
    "licenses/COPYING",
    "templates/description",
    "templates/info/exclude",
    "source/patches.json",
  ])
    regularBytes(join(runtime, path));
  if (
    regularBytes(join(runtime, "source", "patches.json")).toString() !== "[]\n"
  )
    throw new Error("未対応のGit source変更があります");
  if (provenance.binary_sha256 !== sha256(regularBytes(binary)))
    throw new Error("Git本体のhashが一致しません");
  if (!(lstatSync(binary).mode & 0o111))
    throw new Error("Git本体が実行可能ではありません");
  for (const name of helperNames) {
    const path = join(runtime, "git-core", name);
    if (!lstatSync(path).isFile() || !(lstatSync(path).mode & 0o111))
      throw new Error(
        `Git helperが実行可能な通常ファイルではありません: ${name}`,
      );
  }
  const actual = fileInventory(runtime, ["provenance.json"]);
  if (JSON.stringify(actual) !== JSON.stringify(provenance.inventory))
    throw new Error("Git同梱inventoryが一致しません");
  return provenance;
}

async function obtainArchive(path, options, manifest) {
  if (options.sourceArchive) {
    const bytes = regularBytes(resolve(options.sourceArchive));
    verifyArchive(bytes, manifest);
    mkdirSync(dirname(path), { recursive: true });
    if (
      !existsSync(path) ||
      sha256(regularBytes(path)) !== manifest.source.sha256
    )
      writeFileSync(path, bytes);
  } else if (!existsSync(path)) {
    const response = await fetch(manifest.source.url, {
      signal: AbortSignal.timeout(60000),
    });
    if (!response.ok)
      throw new Error(
        `公式Git sourceを取得できません: HTTP ${response.status}`,
      );
    const bytes = Buffer.from(await response.arrayBuffer());
    verifyArchive(bytes, manifest);
    mkdirSync(dirname(path), { recursive: true });
    const temporary = `${path}.${process.pid}.tmp`;
    try {
      writeFileSync(temporary, bytes, { flag: "wx" });
      renameSync(temporary, path);
    } finally {
      rmSync(temporary, { force: true });
    }
  }
  verifyArchive(regularBytes(path), manifest);
}

export async function prepare(
  args = process.argv.slice(2),
  environment = process.env,
) {
  const manifestBytes = regularBytes(manifestPath);
  const manifest = JSON.parse(manifestBytes);
  const options = parseOptions(args, environment, manifest);
  if (process.platform !== "darwin" && !options.fetchOnly)
    throw new Error(
      "同梱Gitのビルド・照合はmacOS build hostで実行してください",
    );
  const cache = join(repositoryRoot, "target", "bundled-git");
  const archive = join(cache, "source", manifest.source.asset);
  if (options.fetchOnly) {
    await obtainArchive(archive, options, manifest);
    process.stdout.write(`${archive}\n`);
    return;
  }
  const sdk = run("/usr/bin/xcrun", ["--sdk", "macosx", "--show-sdk-path"]);
  const sdkVersion = run("/usr/bin/xcrun", [
    "--sdk",
    "macosx",
    "--show-sdk-version",
  ]);
  const compiler = run("/usr/bin/clang", ["--version"]);
  const architecture = manifest.targets[options.target].architecture;
  const compileFlags = `-O2 -g0 -arch ${architecture} -isysroot ${sdk} -mmacosx-version-min=${options.minimum}`;
  const flags = [
    "NO_RUST=YesPlease",
    "NO_GETTEXT=YesPlease",
    "NO_PERL=YesPlease",
    "NO_PYTHON=YesPlease",
    "NO_TCLTK=YesPlease",
    "NO_EXPAT=YesPlease",
    "NO_HOMEBREW=YesPlease",
    "NO_FINK=YesPlease",
    "NO_DARWIN_PORTS=YesPlease",
    "NO_INSTALL_HARDLINKS=YesPlease",
    "RUNTIME_PREFIX=YesPlease",
    "CURL_CONFIG=/usr/bin/curl-config",
    "CC=/usr/bin/clang",
    "SHELL_PATH=/bin/sh",
    "prefix=/kb-app-git",
    `HOST_CPU=${architecture}`,
    `CFLAGS=${compileFlags}`,
    `LDFLAGS=-arch ${architecture} -isysroot ${sdk} -mmacosx-version-min=${options.minimum}`,
  ];
  const build = { architecture, sdk_version: sdkVersion, compiler, flags };
  const recipe = {
    manifest_sha256: sha256(manifestBytes),
    script_sha256: sha256(regularBytes(scriptPath)),
    build,
  };
  const expected = {
    schema: "1.0",
    version: manifest.version,
    target: options.target,
    minimum_system_version: options.minimum,
    recipe_sha256: sha256(JSON.stringify(recipe)),
    source: manifest.source,
    inputs: {
      "prepare-git.mjs": recipe.script_sha256,
      "git-assets.json": recipe.manifest_sha256,
    },
  };
  const binaries = join(repositoryRoot, "app", "src-tauri", "binaries");
  const sidecar = join(binaries, `git-${options.target}`);
  const runtime = join(repositoryRoot, "app", "src-tauri", "git-runtime");
  if (existsSync(join(runtime, "provenance.json")) && existsSync(sidecar)) {
    try {
      verifyPrepared(runtime, sidecar, expected);
      if (!options.verifyOnly)
        syncDevelopmentAlias(sidecar, join(binaries, "git"));
      process.stdout.write(`${sidecar}\n`);
      return;
    } catch (error) {
      if (options.verifyOnly) throw error;
    }
  }
  if (options.verifyOnly)
    throw new Error("照合可能な同梱Git生成物がありません");
  await obtainArchive(archive, options, manifest);
  mkdirSync(cache, { recursive: true });
  const temporary = mkdtempSync(join(cache, "build-"));
  try {
    validateArchiveMembers(
      run("/usr/bin/tar", ["-tf", archive]),
      manifest.version,
    );
    run("/usr/bin/tar", ["-xf", archive, "-C", temporary]);
    const source = join(temporary, `git-${manifest.version}`);
    const buildEnvironment = {
      PATH: "/usr/bin:/bin:/usr/sbin:/sbin",
      LC_ALL: "C",
      TMPDIR: temporary,
      SDKROOT: sdk,
      MACOSX_DEPLOYMENT_TARGET: options.minimum,
    };
    process.stdout.write(
      `公式Git ${manifest.version}を${options.target}向けにビルドします\n`,
    );
    run(
      "/usr/bin/make",
      [
        "-j",
        String(Math.min(availableParallelism(), 8)),
        ...flags,
        "git",
        "git-remote-http",
        "git-http-fetch",
      ],
      { cwd: source, env: buildEnvironment },
    );
    const staged = join(temporary, "git-runtime");
    mkdirSync(join(staged, "git-core"), { recursive: true });
    for (const name of helperNames) {
      const builtName =
        name === "git-remote-https"
          ? "git-remote-http"
          : ["git-upload-pack", "git-receive-pack"].includes(name)
            ? "git"
            : name;
      copyRegular(
        join(source, builtName),
        join(staged, "git-core", name),
        true,
      );
    }
    // サンプルhookは起動しない。init/cloneが開発機のtemplateへ戻らない最小の同梱template。
    copyRegular(
      join(source, "templates", "description"),
      join(staged, "templates", "description"),
    );
    copyRegular(
      join(source, "templates", "info", "exclude"),
      join(staged, "templates", "info", "exclude"),
    );
    copyLicenses(source, join(staged, "licenses"));
    copyRegular(archive, join(staged, "source", manifest.source.asset));
    copyRegular(scriptPath, join(staged, "source", "prepare-git.mjs"));
    copyRegular(manifestPath, join(staged, "source", "git-assets.json"));
    writeFileSync(join(staged, "source", "patches.json"), "[]\n");
    const libraries = {};
    for (const path of [
      join(source, "git"),
      ...helperNames.map((name) => join(staged, "git-core", name)),
    ]) {
      const archs = run("/usr/bin/lipo", ["-archs", path]);
      if (archs !== architecture)
        throw new Error(`GitのCPUが一致しません: ${archs}`);
      libraries[basename(path)] = verifySystemDependencies(
        run("/usr/bin/otool", ["-L", path]),
      );
    }
    const provenance = {
      ...expected,
      source: manifest.source,
      build: { ...build, libraries },
      binary_sha256: sha256(regularBytes(join(source, "git"))),
      inventory: fileInventory(staged),
    };
    writeFileSync(
      join(staged, "provenance.json"),
      `${JSON.stringify(provenance, null, 2)}\n`,
    );
    verifyPrepared(staged, join(source, "git"), expected);
    // 失敗したbuildで前回の生成物を消さない。全inventory検証後だけ置き換える。
    mkdirSync(binaries, { recursive: true });
    copyRegular(join(source, "git"), sidecar, true);
    copyRegular(sidecar, join(binaries, "git"), true);
    const previous = `${runtime}.previous-${process.pid}`;
    if (existsSync(runtime)) renameSync(runtime, previous);
    try {
      renameSync(staged, runtime);
    } catch (error) {
      if (existsSync(previous)) renameSync(previous, runtime);
      throw error;
    }
    rmSync(previous, { recursive: true, force: true });
    process.stdout.write(`${sidecar}\n`);
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}

if (process.argv[1] && resolve(process.argv[1]) === scriptPath) {
  prepare().catch((error) => {
    process.stderr.write(`Git準備を停止しました: ${error.message}\n`);
    process.exitCode = 1;
  });
}
