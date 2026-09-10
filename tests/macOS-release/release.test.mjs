import assert from "node:assert/strict";
import {
  chmodSync,
  cpSync,
  mkdirSync,
  lstatSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import test from "node:test";
import {
  argumentsFor,
  prepare,
  sha256,
  validatePlan,
  validateUpdaterConfiguration,
} from "../../scripts/prepare-macos-release.mjs";
import {
  appInventory,
  signingIdentity,
  verifyArtifacts,
  verifyBundle,
  inspectUpdaterArtifacts,
  verifyUpdaterArtifacts,
} from "../../scripts/verify-macos-release.mjs";

const COMMIT = "a".repeat(40);
const ENV = {
  APPLE_TEAM_ID: "ABCDEFGHIJ",
  APPLE_SIGNING_IDENTITY: "Developer ID Application: Fixture (ABCDEFGHIJ)",
  APPLE_API_ISSUER: "fixture",
  APPLE_API_KEY: "fixture",
  APPLE_API_KEY_PATH: "/fixture/key",
};

function configuredUpdater() {
  // 公開keyの形式だけのfixture。対応秘密鍵も署名成功の証拠も持たない。
  const key = Buffer.alloc(42, 1);
  key.write("Ed");
  return {
    format_version: 1,
    endpoint:
      "https://updates.example.invalid/kb-app/{{target}}/{{arch}}/{{current_version}}",
    public_key: Buffer.from(
      `untrusted comment: synthetic public key\n${key.toString("base64")}\n`,
    ).toString("base64"),
  };
}

function fixture(t, mode = "candidate", bundledGit = false) {
  const root = mkdtempSync(resolve(tmpdir(), "kb-release-test-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  function write(path, value) {
    const absolute = resolve(root, path);
    mkdirSync(dirname(absolute), { recursive: true });
    writeFileSync(absolute, value);
    return absolute;
  }
  write("Cargo.toml", '[workspace.package]\nversion = "0.0.1"\n');
  write("Cargo.lock", "fixture cargo lock");
  write("app/package-lock.json", "{}");
  write(
    "app/src-tauri/updater-config.json",
    JSON.stringify({ format_version: 1, endpoint: null, public_key: null }),
  );
  write(
    "crates/kb-core/update-compatibility.json",
    JSON.stringify({
      runtime_store: "db-v1",
      database_schema: 13,
      persistent_compatibility_epoch: 1,
    }),
  );
  const gitAssets = JSON.stringify({
    version: "2.55.0",
    source: { sha256: sha256("fixture Git source archive") },
  });
  write("scripts/git-assets.json", gitAssets);
  write("scripts/prepare-git.mjs", "fixture Git build recipe");
  write("app/package.json", '{"version":"0.0.1"}');
  write(
    "app/src-tauri/tauri.conf.json",
    JSON.stringify({
      productName: "kb-app",
      identifier: "app.kb.desktop",
      version: "0.0.1",
      bundle: { resources: {} },
    }),
  );
  write(
    "scripts/git-lfs-assets.json",
    JSON.stringify({
      version: "3.7.1",
      targets: { "aarch64-apple-darwin": { sha256: "b".repeat(64) } },
    }),
  );
  const sidecar = "app/src-tauri/binaries/git-lfs-aarch64-apple-darwin";
  write(sidecar, "署名前fixture");
  write(
    `${sidecar}.provenance.json`,
    JSON.stringify({
      schema: 1,
      version: "3.7.1",
      target: "aarch64-apple-darwin",
      archive_sha256: "b".repeat(64),
      binary_sha256: sha256("署名前fixture"),
    }),
  );
  write("third_party/git-lfs/LICENSE.md", "Git LFS contributors fixture");
  write("THIRD_PARTY_NOTICES.md", "合成ライセンスfixture");
  const gitEvidence = JSON.stringify({
    schema: "1.0",
    version: "2.55.0",
    target: "aarch64-apple-darwin",
    minimum_system_version: "13.0",
    source: { sha256: sha256("fixture Git source archive") },
    binary_sha256: sha256("署名前Git fixture"),
  });
  if (bundledGit) {
    write(
      "app/src-tauri/binaries/git-aarch64-apple-darwin",
      "署名前Git fixture",
    );
    write("app/src-tauri/git-runtime/provenance.json", gitEvidence);
    write(
      "app/src-tauri/git-runtime/source/git-2.55.0.tar.xz",
      "fixture Git source archive",
    );
    write(
      "app/src-tauri/git-runtime/source/prepare-git.mjs",
      "fixture Git build recipe",
    );
    write("app/src-tauri/git-runtime/source/git-assets.json", gitAssets);
    write("app/src-tauri/git-runtime/licenses/COPYING", "fixture Git COPYING");
    write(
      "app/src-tauri/git-runtime/templates/info/exclude",
      "fixture Git template",
    );
  }
  const options = {
    mode,
    version: "0.0.1",
    "source-commit": COMMIT,
    target: "aarch64-apple-darwin",
    "minimum-system-version": "13.0",
    output: resolve(root, "plan"),
  };
  const executeGit = (_, args) => (args[0] === "rev-parse" ? COMMIT : "");
  const plan = prepare(
    options,
    root,
    executeGit,
    mode === "release" ? ENV : {},
  );
  const app = resolve(root, "kb-app.app");
  write(
    "kb-app.app/Contents/Resources/release/plan.json",
    `${JSON.stringify(plan, null, 2)}\n`,
  );
  write("kb-app.app/Contents/Info.plist", "fixture plist");
  const magic = Buffer.from("cffaedfe000000000000000002000000", "hex");
  for (const name of ["kb-app", "git-lfs"])
    chmodSync(write(`kb-app.app/Contents/MacOS/${name}`, magic), 0o755);
  if (bundledGit) {
    chmodSync(write("kb-app.app/Contents/MacOS/git", magic), 0o755);
    write("kb-app.app/Contents/Resources/git-provenance.json", gitEvidence);
    for (const [source, destination] of [
      ["source", "git-source"],
      ["templates", "git-templates"],
      ["licenses", "licenses/git"],
    ]) {
      cpSync(
        resolve(root, "app/src-tauri/git-runtime", source),
        resolve(app, "Contents/Resources", destination),
        { recursive: true },
      );
    }
    for (const name of [
      "git-remote-http",
      "git-remote-https",
      "git-upload-pack",
      "git-receive-pack",
    ]) {
      chmodSync(
        write(`kb-app.app/Contents/Resources/git-core/${name}`, magic),
        0o755,
      );
    }
  }
  for (const [source, destination] of [
    ["third_party/git-lfs/LICENSE.md", "git-lfs-LICENSE.md"],
    ["THIRD_PARTY_NOTICES.md", "THIRD_PARTY_NOTICES.md"],
  ]) {
    write(
      `kb-app.app/Contents/Resources/licenses/${destination}`,
      readFileSync(resolve(root, source)),
    );
  }
  const signature =
    mode === "candidate"
      ? "Signature=adhoc"
      : "Authority=Developer ID Application: Fixture (ABCDEFGHIJ)\nTeamIdentifier=ABCDEFGHIJ\nCodeDirectory v=20500 size=1 flags=0x10000(runtime)";
  const commands = [];
  const execute = (program, args, options) => {
    commands.push([program, args, options]);
    if (program === "git") return executeGit(program, args);
    if (program.endsWith("plutil"))
      return {
        CFBundleIdentifier: "app.kb.desktop",
        CFBundleShortVersionString: "0.0.1",
        CFBundleVersion: "0.0.1",
        LSMinimumSystemVersion: "13.0",
        CFBundleExecutable: "kb-app",
      }[args[1]];
    if (program.endsWith("lipo")) return "arm64";
    if (program.endsWith("otool"))
      return args[0] === "-l"
        ? "cmd LC_BUILD_VERSION\ncmdsize 32\nplatform 1\nminos 13.0\nsdk 15.0"
        : `${args.at(-1)}:\n`;
    if (program.endsWith("git-lfs")) return "git-lfs/3.7.1 (fixture)";
    if (program.endsWith("/git"))
      return args[0] === "--version"
        ? "git version 2.55.0"
        : resolve(dirname(program), "../Resources/git-core");
    if (program.endsWith("codesign") && args[0] === "--display")
      return signature;
    if (program.endsWith("hdiutil") && args[0] === "attach")
      cpSync(app, resolve(args[4], "kb-app.app"), { recursive: true });
    if (program.endsWith("ditto"))
      writeFileSync(args.at(-1), "fixture archive bytes");
    return "";
  };
  return { root, write, app, plan, options, execute, commands, executeGit };
}

// 2026-09-08: shellへ渡す前に引数を閉じ、表示版だけ違う成果物や任意refを生成しない。
test("明示引数・版・source commit・targetを検査する", (t) => {
  const f = fixture(t);
  assert.throws(() =>
    argumentsFor(["--mode", "candidate", "--mode", "release"], ["mode"]),
  );
  assert.throws(() => argumentsFor(["--evil", "x"], ["mode"]));
  for (const invalid of [
    { version: "0.1.0" },
    { target: "universal-apple-darwin" },
    { "source-commit": "main" },
  ]) {
    assert.throws(() =>
      prepare(
        { ...f.options, ...invalid, output: resolve(f.root, "invalid") },
        f.root,
        f.executeGit,
        {},
      ),
    );
  }
  assert.throws(() =>
    prepare(
      { ...f.options, output: resolve(f.root, "dirty") },
      f.root,
      (_, args) => (args[0] === "rev-parse" ? COMMIT : " M Cargo.toml"),
      {},
    ),
  );
  assert.throws(() =>
    prepare(
      { ...f.options, output: resolve(f.root, "secret") },
      f.root,
      f.executeGit,
      ENV,
    ),
  );
  const override = JSON.parse(
    readFileSync(resolve(f.root, "plan/tauri-release.json")),
  );
  assert.equal(override.bundle.macOS.signingIdentity, "-");
  assert.equal(override.bundle.createUpdaterArtifacts, false);
});

test("計画の未知path・欠落hash・不正modeを拒否する", (t) => {
  const { plan } = fixture(t);
  assert.throws(() => validatePlan({ ...plan, mode: "published" }));
  assert.throws(() =>
    validatePlan({
      ...plan,
      resources: { ...plan.resources, "../../private": "a".repeat(64) },
    }),
  );
  assert.throws(() => validatePlan({ ...plan, source_inputs: {} }));
});

// 2026-09-09: endpoint/keyの片方だけから更新を有効化せず、私的資格入りURIも配布しない。
test("updaterの公開設定は完全なHTTPS/key pairだけを許す", () => {
  const good = configuredUpdater();
  assert.deepEqual(validateUpdaterConfiguration(good), good);
  for (const changed of [
    { endpoint: null },
    { public_key: null },
    { format_version: 2 },
    { endpoint: "http://updates.example.invalid/manifest.json" },
    { endpoint: "https://user:password@updates.example.invalid/manifest.json" },
    { endpoint: "https://updates.example.invalid/manifest.json#fragment" },
    { endpoint: "https://updates.example.invalid/\nmanifest.json" },
    { public_key: "private key text" },
    {
      public_key: Buffer.from(
        "untrusted comment: private key\nAAAA\n",
      ).toString("base64"),
    },
  ])
    assert.throws(() => validateUpdaterConfiguration({ ...good, ...changed }));
});

test("互換性metadataと公開設定のbytesを固定し更新元の受入を捏造しない", (t) => {
  const f = fixture(t);
  for (const path of [
    "app/src-tauri/updater-config.json",
    "crates/kb-core/update-compatibility.json",
  ])
    assert.equal(
      f.plan.source_inputs[path],
      sha256(readFileSync(resolve(f.root, path))),
    );
  assert.deepEqual(f.plan.compatibility, {
    runtime_store: "db-v1",
    database_schema: 13,
    persistent_compatibility_epoch: 1,
  });
  assert.deepEqual(f.plan.compatible_sources, []);
  assert.throws(
    () => validatePlan({ ...f.plan, compatibility: undefined }),
    /互換性metadata/,
  );
  assert.throws(
    () => validatePlan({ ...f.plan, compatible_sources: [{ passed: true }] }),
    /共存receipt/,
  );
  const incomplete = { runtime_store: "db-v1", database_schema: 13 };
  f.write(
    "crates/kb-core/update-compatibility.json",
    JSON.stringify(incomplete),
  );
  assert.throws(
    () =>
      prepare(
        { ...f.options, output: resolve(f.root, "missing-epoch") },
        f.root,
        f.executeGit,
        {},
      ),
    /互換性metadata/,
  );
});

test("configured releaseだけが公式updater署名用成果物を生成し秘密鍵をplanへ残さない", (t) => {
  const f = fixture(t);
  const config = configuredUpdater();
  f.write("app/src-tauri/updater-config.json", JSON.stringify(config));
  const options = {
    ...f.options,
    mode: "release",
    output: resolve(f.root, "updater-plan"),
  };
  assert.throws(
    () => prepare(options, f.root, f.executeGit, ENV),
    /TAURI_SIGNING_PRIVATE_KEY/,
  );
  const syntheticSecret = "synthetic-not-a-real-secret-key";
  const plan = prepare(options, f.root, f.executeGit, {
    ...ENV,
    TAURI_SIGNING_PRIVATE_KEY: syntheticSecret,
  });
  assert.equal(plan.updater, "configured");
  assert.deepEqual(plan.updater_config, config);
  const override = readFileSync(
    resolve(options.output, "tauri-release.json"),
    "utf8",
  );
  assert.equal(JSON.parse(override).bundle.createUpdaterArtifacts, true);
  assert.ok(
    !override.includes(syntheticSecret) &&
      !JSON.stringify(plan).includes(syntheticSecret),
  );
  assert.throws(
    () =>
      prepare(
        { ...f.options, output: resolve(f.root, "configured-candidate") },
        f.root,
        f.executeGit,
        {},
      ),
    /release/,
  );
});

test("更新archiveとsigの欠落を拒否し、hashを署名の成功証拠にしない", (t) => {
  const f = fixture(t);
  const plan = {
    ...f.plan,
    mode: "release",
    updater: "configured",
    updater_config: configuredUpdater(),
  };
  assert.throws(() => inspectUpdaterArtifacts(plan, f.app), /欠落/);
  f.write("kb-app.app.tar.gz", "synthetic archive; not authenticated");
  assert.throws(() => inspectUpdaterArtifacts(plan, f.app), /欠落/);
  f.write("kb-app.app.tar.gz.sig", "synthetic signature; not verified");
  const evidence = inspectUpdaterArtifacts(plan, f.app);
  assert.equal(evidence.artifacts.length, 2);
  assert.equal(
    evidence.artifacts[0].sha256,
    sha256("synthetic archive; not authenticated"),
  );
  assert.deepEqual(evidence.blockers, [
    "updater_signature_not_verified",
    "updater_archive_contents_not_verified",
    "coexistence_receipt_not_verified",
  ]);
});

test(
  "configured release成果物を記録しても未検証の署名と共存を配布完了にしない",
  { skip: process.platform !== "darwin" },
  (t) => {
    const f = fixture(t, "release", true);
    f.write(
      "app/src-tauri/updater-config.json",
      JSON.stringify(configuredUpdater()),
    );
    const options = {
      ...f.options,
      output: resolve(f.root, "configured-plan"),
    };
    const plan = prepare(options, f.root, f.executeGit, {
      ...ENV,
      TAURI_SIGNING_PRIVATE_KEY: "synthetic-only",
    });
    f.write(
      "kb-app.app/Contents/Resources/release/plan.json",
      `${JSON.stringify(plan, null, 2)}\n`,
    );
    f.write("kb-app.app.tar.gz", "synthetic archive; not authenticated");
    f.write("kb-app.app.tar.gz.sig", "synthetic signature; not verified");
    const report = verifyArtifacts(
      {
        plan: resolve(options.output, "plan.json"),
        app: f.app,
        dmg: f.write("fixture.dmg", "fixture disk image"),
        output: resolve(f.root, "configured-output"),
      },
      f.execute,
      f.root,
    );
    assert.equal(report.distribution_verified, false);
    assert.equal(report.public_release_accepted, false);
    assert.equal(report.artifacts.length, 4);
    assert.deepEqual(report.updater_artifacts, {
      signature_verified: false,
      coexistence_verified: false,
    });
    assert.deepEqual(report.blockers, [
      "updater_signature_not_verified",
      "updater_archive_contents_not_verified",
      "coexistence_receipt_not_verified",
    ]);
    assert.equal(
      report.artifacts.find((artifact) => artifact.name.endsWith(".app.tar.gz"))
        .sha256,
      sha256("synthetic archive; not authenticated"),
    );
  },
);

// 暗号そのものはRustの公開署名fixtureで検査する。ここではCLI receiptの結合だけを検査する。
test("固定verifierの成功receiptだけを候補全内容へ結合し共存blockerを保持する", (t) => {
  const f = fixture(t);
  const plan = {
    ...f.plan,
    mode: "release",
    updater: "configured",
    updater_config: configuredUpdater(),
  };
  f.write("kb-app.app.tar.gz", "synthetic archive");
  f.write("kb-app.app.tar.gz.sig", "synthetic signature");
  const verifier = f.write("verifier", "synthetic verifier fixture");
  chmodSync(verifier, 0o755);
  const options = {
    "updater-verifier": verifier,
    "updater-verifier-sha256": sha256("synthetic verifier fixture"),
    "updater-current-version": "0.0.0",
  };
  const entries = [
    {
      path: "kb-app.app",
      directory: true,
      mode: lstatSync(f.app).mode & 0o777,
      size: 0,
      sha256: sha256(""),
    },
    ...appInventory(f.app).entries.map((entry) => ({
      path: `kb-app.app/${entry.path}`,
      directory: entry.type === "directory",
      mode: lstatSync(resolve(f.app, entry.path)).mode & 0o777,
      size: entry.size ?? 0,
      sha256: entry.sha256 ?? sha256(""),
    })),
  ];
  const receipt = {
    schema: "kb-app.update-package-verification/v1",
    verified: true,
    verifier_version: plan.version,
    version: plan.version,
    target: plan.target,
    source_commit: plan.source_commit,
    archive_sha256: sha256("synthetic archive"),
    plan_sha256: sha256(
      readFileSync(resolve(f.app, "Contents/Resources/release/plan.json")),
    ),
    // Rust serde_json::Valueのsorted key順でも値が一致すれば検証を継続する。
    compatibility: {
      database_schema: plan.compatibility.database_schema,
      persistent_compatibility_epoch:
        plan.compatibility.persistent_compatibility_epoch,
      runtime_store: plan.compatibility.runtime_store,
    },
    entries,
  };
  const execute = (_, args) =>
    args[0] === "--version" ? `kb ${plan.version}` : JSON.stringify(receipt);
  const evidence = verifyUpdaterArtifacts(plan, f.app, options, execute);
  assert.deepEqual(evidence.blockers, ["coexistence_receipt_not_verified"]);
  assert.equal(evidence.verification.signature_verified, true);
  assert.equal(evidence.verification.contents_verified, true);
  for (const changed of [
    { verified: false },
    { archive_sha256: "0".repeat(64) },
    { plan_sha256: "0".repeat(64) },
    { version: "0.0.2" },
    { target: "x86_64-apple-darwin" },
    { source_commit: "0".repeat(40) },
    { entries: entries.slice(1) },
    {
      entries: entries.map((entry, i) =>
        i === 0 ? { ...entry, mode: entry.mode ^ 0o100 } : entry,
      ),
    },
  ]) {
    assert.throws(() =>
      verifyUpdaterArtifacts(plan, f.app, options, (_, args) =>
        args[0] === "--version"
          ? `kb ${plan.version}`
          : JSON.stringify({ ...receipt, ...changed }),
      ),
    );
  }
  assert.throws(() =>
    verifyUpdaterArtifacts(
      plan,
      f.app,
      { ...options, "updater-verifier-sha256": "0".repeat(64) },
      execute,
    ),
  );
  assert.throws(() =>
    verifyUpdaterArtifacts(plan, f.app, options, () => {
      throw new Error("signature verification failed");
    }),
  );
});

// 2026-09-08: versionを名乗るだけの置換sidecarを公式取得済みと扱わない。
test("Git LFSの実bytesと公式archive証拠が一致しなければ計画を作らない", (t) => {
  const f = fixture(t);
  f.write(
    "app/src-tauri/binaries/git-lfs-aarch64-apple-darwin",
    "別の同version binary",
  );
  assert.throws(
    () =>
      prepare(
        { ...f.options, output: resolve(f.root, "changed-sidecar") },
        f.root,
        f.executeGit,
        {},
      ),
    /署名前実bytes/,
  );
});

test("candidateはGit未同梱と公証未確認を明示する", (t) => {
  const f = fixture(t);
  const result = verifyBundle(f.plan, f.app, f.execute);
  assert.deepEqual(result.blockers, [
    "developer_id_and_notarization_not_verified",
    "bundled_git_missing",
  ]);
  assert.equal(result.bundled_git, false);
  assert.equal(result.mach_o_files.length, 2);
  assert.ok(
    !f.commands.some(
      ([program]) => program.endsWith("spctl") || program.endsWith("xcrun"),
    ),
  );
});

// 2026-09-08: version文字列だけではなく、build計画に固定したGit生成証拠を照合する。
test("同梱Gitの生成証拠を照合し、未記録・改変を拒否する", (t) => {
  const f = fixture(t, "release", true);
  const result = verifyBundle(f.plan, f.app, f.execute);
  assert.equal(result.bundled_git, true);
  assert.deepEqual(result.blockers, []);
  assert.equal(result.mach_o_files.length, 7);
  assert.ok(
    f.commands
      .filter(([program]) => program.endsWith("/git"))
      .every(
        ([, , options]) =>
          options.env.PATH === resolve(f.app, "Contents/MacOS") &&
          options.env.GIT_CONFIG_GLOBAL === "/dev/null",
      ),
  );
  const withoutEvidence = { ...f.plan, git_provenance: null };
  f.write(
    "kb-app.app/Contents/Resources/release/plan.json",
    `${JSON.stringify(withoutEvidence, null, 2)}\n`,
  );
  assert.throws(
    () => verifyBundle(withoutEvidence, f.app, f.execute),
    /生成証拠が計画/,
  );
  f.write(
    "kb-app.app/Contents/Resources/release/plan.json",
    `${JSON.stringify(f.plan, null, 2)}\n`,
  );
  f.write(
    "kb-app.app/Contents/Resources/git-provenance.json",
    "改変済み生成証拠",
  );
  assert.throws(
    () => verifyBundle(f.plan, f.app, f.execute),
    /生成証拠が変わりました/,
  );
});

// 2026-09-08: 生成記録が正しくても、対応ソースやライセンスの別bytesを配布しない。
test("Git対応ソース・ライセンス・build手順の欠落や改変を拒否する", (t) => {
  const f = fixture(t, "release", true);
  f.write("kb-app.app/Contents/Resources/licenses/git/COPYING", "別のlicense");
  assert.throws(
    () => verifyBundle(f.plan, f.app, f.execute),
    /対応ソース・ライセンス/,
  );
  f.write(
    "app/src-tauri/git-runtime/source/git-2.55.0.tar.xz",
    "同版の別archive",
  );
  assert.throws(
    () =>
      prepare(
        { ...f.options, output: resolve(f.root, "changed-source") },
        f.root,
        f.executeGit,
        ENV,
      ),
    /sourcearchive|ソースarchive/,
  );
  f.write(
    "app/src-tauri/git-runtime/source/git-2.55.0.tar.xz",
    "fixture Git source archive",
  );
  f.write(
    "app/src-tauri/git-runtime/source/prepare-git.mjs",
    "実行していないbuild手順",
  );
  assert.throws(
    () =>
      prepare(
        { ...f.options, output: resolve(f.root, "changed-recipe") },
        f.root,
        f.executeGit,
        ENV,
      ),
    /build手順/,
  );
  rmSync(resolve(f.root, "app/src-tauri/git-runtime/source/prepare-git.mjs"));
  assert.throws(
    () =>
      prepare(
        { ...f.options, output: resolve(f.root, "missing-recipe") },
        f.root,
        f.executeGit,
        ENV,
      ),
    /build手順/,
  );
});

test("CPU・ライセンス・署名不一致を候補成功にしない", (t) => {
  const f = fixture(t);
  assert.throws(
    () =>
      verifyBundle(f.plan, f.app, (program, args) =>
        program.endsWith("lipo") ? "x86_64" : f.execute(program, args),
      ),
    /CPU/,
  );
  assert.throws(
    () =>
      verifyBundle(f.plan, f.app, (program, args) =>
        program.endsWith("codesign") && args[0] === "--display"
          ? ""
          : f.execute(program, args),
      ),
    /adhoc/,
  );
  f.write(
    "kb-app.app/Contents/Resources/licenses/THIRD_PARTY_NOTICES.md",
    "改変",
  );
  assert.throws(() => verifyBundle(f.plan, f.app, f.execute), /ライセンス/);
});

test("同版でも別commitのappを指定sourceの成果物にしない", (t) => {
  const f = fixture(t);
  assert.throws(
    () =>
      verifyBundle(
        { ...f.plan, source_commit: "c".repeat(40) },
        f.app,
        f.execute,
      ),
    /build計画/,
  );
});

test("全Mach-Oの開発機専用依存と不足rpathを拒否する", (t) => {
  const f = fixture(t);
  for (const library of [
    "/opt/homebrew/lib/libfixture.dylib",
    "@rpath/missing.dylib",
  ]) {
    assert.throws(
      () =>
        verifyBundle(f.plan, f.app, (program, args) =>
          program.endsWith("otool") && args[0] === "-L"
            ? `binary:\n\t${library} (compatibility version 1.0.0, current version 1.0.0)`
            : f.execute(program, args),
        ),
      /非OS/,
    );
  }
});

// 2026-09-08: Info.plistだけを13.0へ下げても、新しいOS専用binaryを合格にしない。
test("native binaryのmacOS下限とライブラリ実体を検査する", (t) => {
  const f = fixture(t);
  assert.throws(
    () =>
      verifyBundle(f.plan, f.app, (program, args) =>
        program.endsWith("otool") && args[0] === "-l"
          ? "cmd LC_BUILD_VERSION\ncmdsize 32\nplatform 1\nminos 14.0\nsdk 15.0"
          : f.execute(program, args),
      ),
    /下限/,
  );
  f.write("kb-app.app/Contents/MacOS/fake.dylib", "libraryではない");
  assert.throws(
    () =>
      verifyBundle(f.plan, f.app, (program, args) =>
        program.endsWith("otool") && args[0] === "-L"
          ? "binary:\n\t@loader_path/fake.dylib (compatibility version 1.0.0, current version 1.0.0)"
          : f.execute(program, args),
      ),
    /非OS/,
  );
});

// 2026-09-08: 独立起動するhelperへkb-appのLC_RPATHを貸して成功扱いしない。
test("別processのRPATHを継承したと推測しない", (t) => {
  const f = fixture(t);
  f.write(
    "kb-app.app/Contents/Frameworks/libfixture.dylib",
    Buffer.from("cffaedfe000000000000000006000000", "hex"),
  );
  assert.throws(
    () =>
      verifyBundle(f.plan, f.app, (program, args) => {
        if (
          program.endsWith("otool") &&
          args[0] === "-l" &&
          args.at(-1).endsWith("/kb-app")
        ) {
          return (
            f.execute(program, args) +
            "\ncmd LC_RPATH\ncmdsize 48\npath @executable_path/../Frameworks (offset 12)"
          );
        }
        if (
          program.endsWith("otool") &&
          args[0] === "-L" &&
          args.at(-1).endsWith("/git-lfs")
        ) {
          return "helper:\n\t@rpath/libfixture.dylib (compatibility version 1.0.0, current version 1.0.0)";
        }
        return f.execute(program, args);
      }),
    /非OS/,
  );
});

test("同じ版を名乗る変更もapp木全体のhashで区別する", (t) => {
  const f = fixture(t);
  const before = appInventory(f.app).sha256;
  f.write("kb-app.app/Contents/Resources/fixture.txt", "before");
  const added = appInventory(f.app).sha256;
  assert.notEqual(added, before);
  f.write("kb-app.app/Contents/Resources/fixture.txt", "after!");
  assert.notEqual(appInventory(f.app).sha256, added);
  symlinkSync("/etc/passwd", resolve(f.app, "Contents/escape"));
  assert.throws(() => appInventory(f.app), /symlink/);
});

test("検査中のファイル変更も拒否する", (t) => {
  const f = fixture(t);
  assert.throws(
    () =>
      verifyBundle(f.plan, f.app, (program, args) => {
        if (program.endsWith("codesign") && args[0] === "--display")
          f.write("kb-app.app/Contents/Resources/race", "changed");
        return f.execute(program, args);
      }),
    /検査中/,
  );
});

test("releaseはDeveloper ID・team・runtime・公証を要求する", (t) => {
  const f = fixture(t, "release");
  assert.equal(signingIdentity("Signature=adhoc").developer_id, false);
  assert.throws(
    () =>
      verifyBundle(f.plan, f.app, (program, args) =>
        program.endsWith("codesign") && args[0] === "--display"
          ? "Signature=adhoc"
          : f.execute(program, args),
      ),
    /Developer ID/,
  );
  assert.throws(
    () =>
      verifyBundle(f.plan, f.app, (program, args) => {
        if (program.endsWith("xcrun")) throw new Error("ticketなし");
        return f.execute(program, args);
      }),
    /ticket/,
  );
  const result = verifyBundle(f.plan, f.app, f.execute);
  assert.deepEqual(result.blockers, ["bundled_git_missing"]);
});

test(
  "releaseでもGit欠落ならdistribution_verified=false、実機受入を捏造しない",
  { skip: process.platform !== "darwin" },
  (t) => {
    const f = fixture(t, "release");
    const dmg = f.write("fixture.dmg", "fixture disk image");
    const report = verifyArtifacts(
      {
        plan: resolve(f.root, "plan/plan.json"),
        app: f.app,
        dmg,
        output: resolve(f.root, "output"),
      },
      f.execute,
      f.root,
    );
    assert.equal(report.distribution_verified, false);
    assert.equal(report.public_release_accepted, false);
    assert.ok(report.unverified.includes("clean_mac_first_record_and_search"));
    assert.equal(report.artifacts.length, 2);
    assert.equal(
      report.artifacts.find((artifact) => artifact.name.endsWith(".dmg"))
        .sha256,
      sha256("fixture disk image"),
    );
  },
);
