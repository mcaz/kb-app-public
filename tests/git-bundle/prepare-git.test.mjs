import assert from "node:assert/strict";
import {
  chmodSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";
import {
  fileInventory,
  helperNames,
  parseOptions,
  sha256,
  syncDevelopmentAlias,
  validateArchiveMembers,
  verifyArchive,
  verifyPrepared,
  verifySystemDependencies,
} from "../../scripts/prepare-git.mjs";
import { isolatedEnvironment, validateTemplates } from "./acceptance.mjs";

const manifest = JSON.parse(
  readFileSync(new URL("../../scripts/git-assets.json", import.meta.url)),
);

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), "kb-git-provenance-test-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const runtime = join(root, "runtime");
  const binary = join(root, "git");
  const put = (name, bytes, mode = 0o644) => {
    const path = join(runtime, name);
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, bytes, { mode });
  };
  writeFileSync(binary, "synthetic git", { mode: 0o755 });
  for (const name of helperNames)
    put(`git-core/${name}`, `synthetic ${name}`, 0o755);
  put("templates/description", "synthetic template");
  put("templates/info/exclude", "");
  put("licenses/COPYING", "synthetic license");
  put("source/git-2.55.0.tar.xz", "synthetic archive");
  put("source/prepare-git.mjs", "synthetic recipe");
  put("source/git-assets.json", "synthetic manifest");
  put("source/patches.json", "[]\n");
  const expected = {
    schema: "1.0",
    version: "2.55.0",
    target: "aarch64-apple-darwin",
    minimum_system_version: "13.0",
    recipe_sha256: sha256("synthetic recipe"),
    source: {
      url: "https://example.invalid/git",
      asset: "git-2.55.0.tar.xz",
      sha256: sha256("synthetic archive"),
    },
    inputs: {
      "prepare-git.mjs": sha256("synthetic recipe"),
      "git-assets.json": sha256("synthetic manifest"),
    },
  };
  const receipt = () => {
    const provenance = {
      ...expected,
      binary_sha256: sha256(readFileSync(binary)),
      inventory: fileInventory(runtime, ["provenance.json"]),
    };
    writeFileSync(
      join(runtime, "provenance.json"),
      `${JSON.stringify(provenance, null, 2)}\n`,
    );
    return provenance;
  };
  receipt();
  return { runtime, binary, expected, put, receipt };
}

test("releaseのtargetとmacOS下限はenvで渡せ、明示CLIが優先する", () => {
  assert.deepEqual(
    parseOptions(
      [],
      {
        KB_TAURI_TARGET: "x86_64-apple-darwin",
        MACOSX_DEPLOYMENT_TARGET: "14.0",
      },
      manifest,
    ),
    {
      target: "x86_64-apple-darwin",
      minimum: "14.0",
      sourceArchive: undefined,
      fetchOnly: false,
      verifyOnly: false,
    },
  );
  assert.equal(
    parseOptions(
      ["--target", "aarch64-apple-darwin", "--minimum-system-version", "13.0"],
      { MACOSX_DEPLOYMENT_TARGET: "14.0" },
      manifest,
    ).minimum,
    "13.0",
  );
  for (const args of [
    ["--target", "aarch64-unknown-linux-gnu"],
    ["--minimum-system-version", "12.9"],
    ["--minimum-system-version", "13;echo"],
    ["--target"],
    ["--verify-only", "--fetch-only"],
    ["--target", "aarch64-apple-darwin", "--target", "x86_64-apple-darwin"],
  ])
    assert.throws(() => parseOptions(args, {}, manifest));
});

test("2026-09-08: archive名が正しくても別bytesを実build入力にしない", () => {
  assert.throws(
    () => verifyArchive(Buffer.from("not the official archive"), manifest),
    /SHA-256不一致/,
  );
  const bytes = Buffer.from("synthetic source");
  verifyArchive(bytes, { source: { sha256: sha256(bytes) } });
});

test("展開は固定version配下に限定し、親への脱出と別rootを拒否する", () => {
  validateArchiveMembers(
    "git-2.55.0/\ngit-2.55.0/Makefile\ngit-2.55.0/templates/info--exclude\n",
    "2.55.0",
  );
  for (const entry of [
    "git-2.55.0/../escape",
    "/git-2.55.0/file",
    "git-2.54.0/file",
    "git-2.55.0/a\\b",
    "",
  ])
    assert.throws(() => validateArchiveMembers(entry, "2.55.0"));
});

test("OS標準dylibだけを許可しHomebrew/CLT/rpath依存を拒否する", () => {
  assert.deepEqual(
    verifySystemDependencies(
      "git:\n\t/usr/lib/libSystem.B.dylib (compatibility version 1)\n\t/System/Library/Frameworks/Security.framework/Versions/A/Security (compatibility version 1)",
    ),
    [
      "/usr/lib/libSystem.B.dylib",
      "/System/Library/Frameworks/Security.framework/Versions/A/Security",
    ],
  );
  for (const library of [
    "/opt/homebrew/opt/libiconv/lib/libiconv.2.dylib",
    "/Library/Developer/CommandLineTools/usr/lib/libfoo.dylib",
    "@rpath/libcurl.dylib",
  ])
    assert.throws(
      () =>
        verifySystemDependencies(
          `git:\n\t${library} (compatibility version 1)`,
        ),
      /OS標準以外/,
    );
});

test("同じ入力の照合はprovenanceを更新せず、source/recipeも照合する", (t) => {
  const f = fixture(t);
  const before = readFileSync(join(f.runtime, "provenance.json"));
  verifyPrepared(f.runtime, f.binary, f.expected);
  verifyPrepared(f.runtime, f.binary, f.expected);
  assert.deepEqual(readFileSync(join(f.runtime, "provenance.json")), before);
  assert.throws(
    () =>
      verifyPrepared(f.runtime, f.binary, {
        ...f.expected,
        minimum_system_version: "14.0",
      }),
    /minimum_system_version/,
  );
  f.put("source/prepare-git.mjs", "changed recipe");
  f.receipt();
  assert.throws(
    () => verifyPrepared(f.runtime, f.binary, f.expected),
    /build recipe/,
  );
});

test("2026-09-08: helper欠損/改変/非実行属性を登録済み生成物と誤認しない", (t) => {
  const f = fixture(t);
  const path = join(f.runtime, "git-core", "git-remote-https");
  chmodSync(path, 0o644);
  assert.throws(
    () => verifyPrepared(f.runtime, f.binary, f.expected),
    /実行可能/,
  );
  chmodSync(path, 0o755);
  f.put("git-core/git-remote-https", "changed helper", 0o755);
  assert.throws(
    () => verifyPrepared(f.runtime, f.binary, f.expected),
    /inventory/,
  );
  rmSync(path);
  assert.throws(() => verifyPrepared(f.runtime, f.binary, f.expected));
});

test("source/license欠損をinventory書換えで隠せず、symlinkも拒否する", (t) => {
  const f = fixture(t);
  rmSync(join(f.runtime, "licenses", "COPYING"));
  f.receipt();
  assert.throws(() => verifyPrepared(f.runtime, f.binary, f.expected));
  f.put("licenses/COPYING", "synthetic license");
  symlinkSync(f.binary, join(f.runtime, "git-core", "unexpected-link"));
  assert.throws(() => fileInventory(f.runtime), /symlink/);
});

test("2026-09-08: 不明ファイル追加やsource tar改変をcache hitにしない", (t) => {
  const f = fixture(t);
  f.put("git-core/extra", "unexpected");
  assert.throws(
    () => verifyPrepared(f.runtime, f.binary, f.expected),
    /inventory/,
  );
  f.receipt();
  f.put("source/git-2.55.0.tar.xz", "not the verified source");
  f.receipt();
  assert.throws(
    () => verifyPrepared(f.runtime, f.binary, f.expected),
    /SHA-256不一致/,
  );
});

test("2026-09-08: 同じbytesでも開発用Gitの実行属性欠損は修復する", (t) => {
  const f = fixture(t);
  const alias = join(f.runtime, "development-git");
  writeFileSync(alias, readFileSync(f.binary), { mode: 0o644 });
  syncDevelopmentAlias(f.binary, alias);
  assert.ok(lstatSync(alias).mode & 0o111);
  assert.deepEqual(readFileSync(alias), readFileSync(f.binary));
});

test("PATH隔離受入は個人設定を継承せずGit helperとOS道具を明示する", () => {
  const environment = isolatedEnvironment(
    "/synthetic/allowed-tools",
    "/synthetic/runtime",
  );
  assert.equal(environment.PATH, "/synthetic/allowed-tools");
  assert.equal(environment.GIT_EXEC_PATH, "/synthetic/runtime/git-core");
  assert.equal(environment.GIT_CONFIG_GLOBAL, "/dev/null");
  assert.equal(environment.GIT_CONFIG_SYSTEM, "/dev/null");
  assert.equal(environment.GIT_CONFIG_COUNT, undefined);
  assert.equal(
    isolatedEnvironment(
      "/bin-only",
      "/app/Resources",
      "/app/Resources/git-templates",
    ).GIT_TEMPLATE_DIR,
    "/app/Resources/git-templates",
  );
});

test("2026-09-08: template欠損をGitの警告だけで受入成功にしない", (t) => {
  const f = fixture(t);
  const templates = join(f.runtime, "templates");
  validateTemplates(templates);
  rmSync(join(templates, "info", "exclude"));
  assert.throws(() => validateTemplates(templates));
});
