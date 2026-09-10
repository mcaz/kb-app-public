import {
  closeSync,
  copyFileSync,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readFileSync,
  readlinkSync,
  readSync,
  readdirSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import {
  basename,
  dirname,
  isAbsolute,
  relative,
  resolve,
  sep,
} from "node:path";
import { pathToFileURL } from "node:url";
import {
  argumentsFor,
  gitResources,
  gitSidecar,
  lfsSidecar,
  requireValue,
  ROOT,
  run,
  sha256,
  TARGETS,
  validatePlan,
} from "./prepare-macos-release.mjs";

function inside(root, path) {
  const rel = relative(root, path);
  return (
    rel === "" ||
    (!rel.startsWith(`..${sep}`) && rel !== ".." && !isAbsolute(rel))
  );
}

export function fileHash(path) {
  requireValue(lstatSync(path).isFile(), "配布物が通常ファイルではありません");
  const fd = openSync(path, "r");
  const digest = createHash("sha256");
  const buffer = Buffer.alloc(1024 * 1024);
  try {
    let length;
    while ((length = readSync(fd, buffer, 0, buffer.length, null)) > 0)
      digest.update(buffer.subarray(0, length));
  } finally {
    closeSync(fd);
  }
  return digest.digest("hex");
}

// mtimeやディレクトリ列挙順は配布物の同一性に使わない。内部frameworkのsymlinkは内容も記録する。
export function appInventory(app) {
  const root = realpathSync(app);
  requireValue(
    lstatSync(app).isDirectory(),
    "appが通常directoryではありません",
  );
  const entries = [];
  function walk(directory) {
    for (const name of readdirSync(directory).sort()) {
      const path = resolve(directory, name);
      const relativePath = relative(root, path).split(sep).join("/");
      const stat = lstatSync(path);
      if (stat.isSymbolicLink()) {
        const target = readlinkSync(path);
        requireValue(
          !isAbsolute(target) && inside(root, realpathSync(path)),
          "app外へ出るsymlinkがあります",
        );
        entries.push({ path: relativePath, type: "symlink", target });
      } else if (stat.isDirectory()) {
        entries.push({ path: relativePath, type: "directory" });
        walk(path);
      } else {
        requireValue(stat.isFile(), "appに非通常ファイルがあります");
        entries.push({
          path: relativePath,
          type: "file",
          executable: (stat.mode & 0o111) !== 0,
          size: stat.size,
          sha256: fileHash(path),
        });
      }
    }
  }
  walk(root);
  return { sha256: sha256(JSON.stringify(entries)), entries };
}

export function signingIdentity(display) {
  return {
    developer_id: /^Authority=Developer ID Application:/m.test(display),
    team_id: display.match(/^TeamIdentifier=([A-Z0-9]{10})$/m)?.[1] ?? null,
    hardened_runtime: /^CodeDirectory .*flags=.*\bruntime\b/m.test(display),
    adhoc: /^Signature=adhoc$/m.test(display),
  };
}

function macho(path) {
  const fd = openSync(path, "r");
  const magic = Buffer.alloc(4);
  try {
    readSync(fd, magic, 0, 4, 0);
  } finally {
    closeSync(fd);
  }
  return [
    "feedface",
    "cefaedfe",
    "feedfacf",
    "cffaedfe",
    "cafebabe",
    "bebafeca",
    "cafebabf",
    "bfbafeca",
  ].includes(magic.toString("hex"));
}

function machoKind(path) {
  const fd = openSync(path, "r");
  const header = Buffer.alloc(16);
  try {
    requireValue(
      readSync(fd, header, 0, 16, 0) === 16,
      "Mach-O headerが短すぎます",
    );
  } finally {
    closeSync(fd);
  }
  const magic = header.subarray(0, 4).toString("hex");
  if (["cefaedfe", "cffaedfe"].includes(magic)) return header.readUInt32LE(12);
  if (["feedface", "feedfacf"].includes(magic)) return header.readUInt32BE(12);
  throw new Error("単一CPUのthin Mach-Oだけを配布候補にできます");
}

// Homebrew等の開発機専用dylibを同梱済みと誤認しない。loaderの相対指定は実際のapp内へ解決する。
export function verifyMachO(
  app,
  inventory,
  target,
  minimumSystemVersion,
  execute = run,
) {
  const loadedCommands = new Map();
  const commands = (path) => {
    if (!loadedCommands.has(path))
      loadedCommands.set(path, execute("/usr/bin/otool", ["-l", path]));
    return loadedCommands.get(path);
  };
  const rpaths = (path) =>
    [
      ...commands(path).matchAll(
        /cmd LC_RPATH\s+cmdsize \d+\s+path (.+?) \(offset \d+\)/g,
      ),
    ].map((match) => match[1]);
  const expand = (path, loader) =>
    path.startsWith("@loader_path/")
      ? resolve(dirname(loader), path.slice(13))
      : path.startsWith("@executable_path/")
        ? machoKind(loader) === 2
          ? resolve(dirname(loader), path.slice(17))
          : null
        : path;
  const checked = [];
  for (const entry of inventory.entries.filter(
    (entry) => entry.type === "file",
  )) {
    const path = resolve(app, entry.path);
    if (!macho(path)) continue;
    const architectures = execute("/usr/bin/lipo", ["-archs", path]).split(
      /\s+/,
    );
    requireValue(
      architectures.length === 1 && architectures[0] === TARGETS[target],
      "同梱Mach-OのCPUが一致しません",
    );
    const minimum =
      commands(path).match(
        /cmd LC_BUILD_VERSION\s+cmdsize \d+\s+platform (?:1|MACOS)\s+minos (\d+\.\d+(?:\.\d+)?)/,
      )?.[1] ??
      commands(path).match(
        /cmd LC_VERSION_MIN_MACOSX\s+cmdsize \d+\s+version (\d+\.\d+(?:\.\d+)?)/,
      )?.[1];
    const parts = (version) =>
      version.split(".").map(Number).concat([0, 0]).slice(0, 3);
    const supported =
      minimum &&
      parts(minimum).reduce(
        (order, part, index) =>
          order || Math.sign(part - parts(minimumSystemVersion)[index]),
        0,
      ) <= 0;
    requireValue(
      supported,
      "同梱Mach-OのmacOS下限が不明または指定下限より新しいです",
    );
    // Git/helperは独立processで起動するため、kb-appのRPATHを継承したと仮定しない。
    const search = rpaths(path)
      .map((item) => expand(item, path))
      .filter((item) => item && isAbsolute(item));
    const ownId = commands(path).match(
      /cmd LC_ID_DYLIB\s+cmdsize \d+\s+name (.+?) \(offset \d+\)/,
    )?.[1];
    const dependencies = execute("/usr/bin/otool", ["-L", path])
      .split("\n")
      .slice(1)
      .map((line) => line.trim().split(" (compatibility version")[0])
      .filter((dependency) => dependency && dependency !== ownId);
    for (const dependency of dependencies) {
      if (
        isAbsolute(dependency) &&
        (resolve(dependency).startsWith("/System/Library/") ||
          resolve(dependency).startsWith("/usr/lib/"))
      )
        continue;
      const candidates = dependency.startsWith("@rpath/")
        ? search.map((base) => resolve(base, dependency.slice(7)))
        : [expand(dependency, path)];
      requireValue(
        candidates.some(
          (candidate) =>
            candidate &&
            isAbsolute(candidate) &&
            existsSync(candidate) &&
            inside(realpathSync(app), realpathSync(candidate)) &&
            lstatSync(realpathSync(candidate)).isFile() &&
            macho(realpathSync(candidate)) &&
            machoKind(realpathSync(candidate)) === 6,
        ),
        "app外の非OSライブラリに依存しています",
      );
    }
    checked.push(entry.path);
  }
  return checked;
}

export function verifyBundle(plan, app, execute = run) {
  validatePlan(plan);
  const inventory = appInventory(app);
  // 同版の別commitのappを、検査時に指定したsourceへ後付けで紐付けない。
  requireValue(
    fileHash(resolve(app, "Contents/Resources/release/plan.json")) ===
      sha256(`${JSON.stringify(plan, null, 2)}\n`),
    "app内のbuild計画が指定sourceと一致しません",
  );
  const plist = resolve(app, "Contents/Info.plist");
  const value = (key) =>
    execute("/usr/bin/plutil", ["-extract", key, "raw", "-o", "-", plist]);
  requireValue(
    value("CFBundleIdentifier") === plan.identifier,
    "bundle identifierが一致しません",
  );
  requireValue(
    value("CFBundleShortVersionString") === plan.version,
    "bundle versionが一致しません",
  );
  requireValue(
    value("CFBundleVersion") === plan.version,
    "bundle build versionが一致しません",
  );
  requireValue(
    value("LSMinimumSystemVersion") === plan.minimum_system_version,
    "macOS下限が一致しません",
  );
  requireValue(
    value("CFBundleExecutable") === "kb-app",
    "app executableが一致しません",
  );
  const bin = resolve(app, "Contents/MacOS");
  const architectures = {};
  for (const name of ["kb-app", "git-lfs"]) {
    const path = resolve(bin, name);
    requireValue(
      lstatSync(path).isFile() && (lstatSync(path).mode & 0o111) !== 0,
      `${name} が同梱されていません`,
    );
    const arch = execute("/usr/bin/lipo", ["-archs", path]).split(/\s+/).sort();
    requireValue(
      arch.length === 1 && arch[0] === TARGETS[plan.target],
      `${name} のCPUが一致しません`,
    );
    architectures[name] = arch;
  }
  const lfsVersion = execute(resolve(bin, "git-lfs"), ["version"]);
  requireValue(
    lfsVersion.startsWith(`git-lfs/${plan.git_lfs_version} `),
    "Git LFS版が一致しません",
  );
  for (const [path, digest] of Object.entries(plan.resources)) {
    requireValue(
      fileHash(resolve(app, "Contents/Resources", path)) === digest,
      `同梱ライセンスが一致しません: ${path}`,
    );
  }
  const mach_o_files = verifyMachO(
    app,
    inventory,
    plan.target,
    plan.minimum_system_version,
    execute,
  );
  execute("/usr/bin/codesign", ["--verify", "--deep", "--strict", app]);
  const signature = signingIdentity(
    execute("/usr/bin/codesign", ["--display", "--verbose=4", app]),
  );
  const blockers = [];
  if (plan.mode === "release") {
    requireValue(
      signature.developer_id &&
        signature.team_id === plan.apple_team_id &&
        signature.hardened_runtime,
      "Developer ID・Team ID・hardened runtimeを確認できません",
    );
    execute("/usr/bin/xcrun", ["stapler", "validate", app]);
    execute("/usr/sbin/spctl", [
      "--assess",
      "--type",
      "execute",
      "--verbose=2",
      app,
    ]);
  } else {
    requireValue(
      signature.adhoc,
      "candidateは明示的なadhoc署名で生成してください",
    );
    blockers.push("developer_id_and_notarization_not_verified");
  }
  // Git LFSの存在をGit本体の同梱と読み替えない。環境の/usr/bin/gitへもfallbackしない。
  const git = resolve(bin, "git");
  let bundledGit = false;
  if (existsSync(git)) {
    requireValue(plan.git_provenance, "同梱Gitの生成証拠が計画にありません");
    requireValue(
      fileHash(resolve(app, "Contents/Resources/git-provenance.json")) ===
        plan.git_provenance.document_sha256,
      "同梱Gitの生成証拠が変わりました",
    );
    for (const [path, digest] of Object.entries(
      plan.git_provenance.resources,
    )) {
      requireValue(
        fileHash(resolve(app, "Contents/Resources", path)) === digest,
        "同梱Gitの対応ソース・ライセンス・templateが変わりました",
      );
    }
    requireValue(
      lstatSync(git).isFile() && (lstatSync(git).mode & 0o111) !== 0,
      "同梱Gitが通常実行ファイルではありません",
    );
    const gitCore = resolve(app, "Contents/Resources/git-core");
    for (const name of [
      "git-remote-http",
      "git-remote-https",
      "git-upload-pack",
      "git-receive-pack",
    ]) {
      const helper = resolve(gitCore, name);
      requireValue(
        existsSync(helper) &&
          lstatSync(helper).isFile() &&
          (lstatSync(helper).mode & 0o111) !== 0,
        "同梱Git helperが欠けています",
      );
    }
    const env = {
      PATH: bin,
      GIT_EXEC_PATH: gitCore,
      GIT_TEMPLATE_DIR: resolve(app, "Contents/Resources/git-templates"),
      GIT_CONFIG_NOSYSTEM: "1",
      GIT_CONFIG_GLOBAL: "/dev/null",
      LC_ALL: "C",
    };
    requireValue(
      execute(git, ["--version"], { env }) ===
        `git version ${plan.git_provenance.version}`,
      "同梱Gitを実行できません",
    );
    const execPath = execute(git, ["--exec-path"], { env });
    requireValue(
      inside(realpathSync(app), realpathSync(execPath)),
      "Git helperがapp外を参照しています",
    );
    bundledGit = true;
  } else {
    blockers.push("bundled_git_missing");
  }
  requireValue(
    appInventory(app).sha256 === inventory.sha256,
    "検査中にappが変わりました",
  );
  return {
    inventory,
    architectures,
    mach_o_files,
    signature,
    bundled_git: bundledGit,
    blockers,
  };
}

// 成果物の存在やhashだけでは署名検証を通したことにしない。
export function inspectUpdaterArtifacts(plan, app) {
  if (plan.updater === "not_configured") return { artifacts: [], blockers: [] };
  requireValue(
    plan.updater === "configured" && plan.mode === "release",
    "更新成果物はconfigured releaseだけを扱います",
  );
  const artifacts = [
    {
      path: `${app}.tar.gz`,
      suffix: ".app.tar.gz",
      maximum: 128 * 1024 * 1024,
    },
    { path: `${app}.tar.gz.sig`, suffix: ".app.tar.gz.sig", maximum: 4096 },
  ].map((artifact) => {
    requireValue(
      existsSync(artifact.path) && lstatSync(artifact.path).isFile(),
      "更新archiveまたは署名fileが欠落しています",
    );
    const size = lstatSync(artifact.path).size;
    requireValue(
      size > 0 && size <= artifact.maximum,
      "更新archiveまたは署名fileのsizeが不正です",
    );
    return { ...artifact, size, sha256: fileHash(artifact.path) };
  });
  return {
    artifacts,
    blockers: [
      "updater_signature_not_verified",
      "updater_archive_contents_not_verified",
      "coexistence_receipt_not_verified",
    ],
  };
}

export function verifyUpdaterArtifacts(plan, app, options, execute = run) {
  const evidence = inspectUpdaterArtifacts(plan, app);
  if (plan.updater === "not_configured" || !options["updater-verifier"])
    return evidence;
  const verifier = resolve(options["updater-verifier"]);
  const pinnedHash = options["updater-verifier-sha256"];
  requireValue(
    /^[a-f0-9]{64}$/.test(pinnedHash ?? "") &&
      lstatSync(verifier).isFile() &&
      (lstatSync(verifier).mode & 0o111) !== 0 &&
      fileHash(verifier) === pinnedHash,
    "固定した更新verifierと実bytesが一致しません",
  );
  requireValue(
    typeof options["updater-current-version"] === "string" &&
      options["updater-current-version"],
    "更新元の表示versionを明示してください（共存受入は別検査です）",
  );
  requireValue(
    execute(verifier, ["--version"]) === `kb ${plan.version}`,
    "更新verifierのversionが一致しません",
  );
  const temporary = mkdtempSync(resolve(tmpdir(), "kb-update-verification-"));
  let receipt;
  try {
    const key = resolve(temporary, "public-key.txt");
    writeFileSync(key, plan.updater_config.public_key, {
      flag: "wx",
      mode: 0o600,
    });
    receipt = JSON.parse(
      execute(verifier, [
        "verify-update-package",
        "--archive",
        `${app}.tar.gz`,
        "--signature",
        `${app}.tar.gz.sig`,
        "--public-key",
        key,
        "--current-version",
        options["updater-current-version"],
        "--version",
        plan.version,
        "--target",
        plan.target,
      ]),
    );
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
  requireValue(
    receipt.schema === "kb-app.update-package-verification/v1" &&
      receipt.verified === true &&
      receipt.verifier_version === plan.version &&
      receipt.version === plan.version &&
      receipt.target === plan.target &&
      receipt.source_commit === plan.source_commit &&
      receipt.archive_sha256 === evidence.artifacts[0].sha256 &&
      receipt.plan_sha256 ===
        fileHash(resolve(app, "Contents/Resources/release/plan.json")) &&
      [
        "runtime_store",
        "database_schema",
        "persistent_compatibility_epoch",
      ].every(
        (key) => receipt.compatibility?.[key] === plan.compatibility[key],
      ),
    "更新verifierの署名・内容receiptが候補と一致しません",
  );
  // 同じplanを入れた別appの署名を、候補app自体の内容検査として受け取らない。
  const expected = new Map([
    [
      "kb-app.app",
      {
        directory: true,
        mode: lstatSync(app).mode & 0o777,
        size: 0,
        sha256: sha256(""),
      },
    ],
    ...appInventory(app).entries.map((entry) => {
      requireValue(entry.type !== "symlink", "更新候補のlinkは受け付けません");
      return [
        `kb-app.app/${entry.path}`,
        {
          directory: entry.type === "directory",
          mode: lstatSync(resolve(app, entry.path)).mode & 0o777,
          size: entry.size ?? 0,
          sha256: entry.sha256 ?? sha256(""),
        },
      ];
    }),
  ]);
  const seen = new Set();
  requireValue(
    Array.isArray(receipt.entries) &&
      receipt.entries.length === expected.size &&
      receipt.entries.every((entry) => {
        const match = expected.get(entry.path);
        if (!match || seen.has(entry.path)) return false;
        seen.add(entry.path);
        return Object.keys(match).every((key) => entry[key] === match[key]);
      }),
    "更新archiveの全file・directory・権限が候補appと一致しません",
  );
  requireValue(
    fileHash(verifier) === pinnedHash &&
      evidence.artifacts.every(
        (artifact) => fileHash(artifact.path) === artifact.sha256,
      ),
    "更新検査中にverifierまたは入力が変更されました",
  );
  return {
    ...evidence,
    blockers: ["coexistence_receipt_not_verified"],
    verification: {
      signature_verified: true,
      contents_verified: true,
      coexistence_verified: false,
      verifier_sha256: pinnedHash,
    },
  };
}

export function verifyArtifacts(options, execute = run, root = ROOT) {
  requireValue(process.platform === "darwin", "macOS上で検査してください");
  const planBytes = readFileSync(resolve(options.plan));
  const plan = validatePlan(JSON.parse(planBytes));
  requireValue(
    execute("git", ["rev-parse", "HEAD"], { cwd: root }) === plan.source_commit,
    "検査checkoutとsource commitが一致しません",
  );
  requireValue(
    execute("git", ["status", "--porcelain", "--untracked-files=all"], {
      cwd: root,
    }) === "",
    "build中にsourceが変更されました",
  );
  for (const [path, digest] of Object.entries(plan.source_inputs))
    requireValue(
      fileHash(resolve(root, path)) === digest,
      "build入力が変更されました",
    );
  requireValue(
    JSON.stringify(plan.compatibility) ===
      JSON.stringify(
        JSON.parse(
          readFileSync(
            resolve(root, "crates/kb-core/update-compatibility.json"),
          ),
        ),
      ),
    "更新互換性metadataとbuild入力が一致しません",
  );
  requireValue(
    JSON.stringify(plan.updater_config) ===
      JSON.stringify(
        JSON.parse(
          readFileSync(resolve(root, "app/src-tauri/updater-config.json")),
        ),
      ),
    "updater公開設定とbuild入力が一致しません",
  );
  requireValue(
    fileHash(resolve(root, lfsSidecar(plan.target))) ===
      plan.git_lfs_provenance.binary_sha256,
    "build中に署名前Git LFSが変更されました",
  );
  if (plan.git_provenance) {
    requireValue(
      fileHash(resolve(root, gitSidecar(plan.target))) ===
        plan.git_provenance.binary_sha256,
      "build中に署名前Gitが変更されました",
    );
    requireValue(
      JSON.stringify(gitResources(root)) ===
        JSON.stringify(plan.git_provenance.resources),
      "build中にGit配布資材が変更されました",
    );
  }
  const app = resolve(options.app);
  const dmg = resolve(options.dmg);
  requireValue(
    basename(app) === "kb-app.app" && dmg.endsWith(".dmg"),
    "app/dmgの指定が不正です",
  );
  const report = verifyBundle(plan, app, execute);
  const updateArtifacts = verifyUpdaterArtifacts(plan, app, options, execute);
  report.blockers.push(...updateArtifacts.blockers);
  const dmgHash = fileHash(dmg);
  execute("/usr/bin/hdiutil", ["verify", dmg]);
  if (plan.mode === "release") {
    execute("/usr/bin/codesign", ["--verify", "--strict", dmg]);
    const signature = signingIdentity(
      execute("/usr/bin/codesign", ["--display", "--verbose=4", dmg]),
    );
    requireValue(
      signature.developer_id && signature.team_id === plan.apple_team_id,
      "DMGのDeveloper IDが一致しません",
    );
    execute("/usr/bin/xcrun", ["stapler", "validate", dmg]);
    execute("/usr/sbin/spctl", [
      "--assess",
      "--type",
      "open",
      "--context",
      "context:primary-signature",
      "--verbose=2",
      dmg,
    ]);
  }
  const temporary = mkdtempSync(resolve(tmpdir(), "kb-macos-release-"));
  const mount = resolve(temporary, "mounted");
  mkdirSync(mount);
  let mounted = false;
  try {
    execute("/usr/bin/hdiutil", [
      "attach",
      "-readonly",
      "-nobrowse",
      "-mountpoint",
      mount,
      dmg,
    ]);
    mounted = true;
    const packaged = verifyBundle(plan, resolve(mount, "kb-app.app"), execute);
    requireValue(
      packaged.inventory.sha256 === report.inventory.sha256,
      "DMGのappが候補appと一致しません",
    );
  } finally {
    if (mounted) execute("/usr/bin/hdiutil", ["detach", mount]);
    rmSync(temporary, { recursive: true, force: true });
  }
  requireValue(
    fileHash(dmg) === dmgHash &&
      appInventory(app).sha256 === report.inventory.sha256,
    "検査中に配布物が変更されました",
  );
  const output = resolve(options.output);
  mkdirSync(output, { recursive: true });
  requireValue(
    !inside(realpathSync(app), realpathSync(output)),
    "成果物の出力先をapp内には置けません",
  );
  const stem = `kb-app-${plan.version}-${plan.target}-${plan.mode}`;
  const appZip = resolve(output, `${stem}.app.zip`);
  const dmgOut = resolve(output, `${stem}.dmg`);
  const updateCopies = updateArtifacts.artifacts.map((artifact) => ({
    ...artifact,
    destination: resolve(output, `${stem}${artifact.suffix}`),
  }));
  for (const path of [
    appZip,
    dmgOut,
    resolve(output, "manifest.json"),
    ...updateCopies.map((artifact) => artifact.destination),
  ])
    requireValue(!existsSync(path), "既存の候補成果物を上書きしません");
  execute("/usr/bin/ditto", [
    "-c",
    "-k",
    "--sequesterRsrc",
    "--keepParent",
    app,
    appZip,
  ]);
  copyFileSync(dmg, dmgOut);
  for (const artifact of updateCopies) {
    requireValue(
      fileHash(artifact.path) === artifact.sha256,
      "検査中に更新成果物が変更されました",
    );
    copyFileSync(artifact.path, artifact.destination);
    requireValue(
      fileHash(artifact.destination) === artifact.sha256,
      "更新成果物のコピーが一致しません",
    );
  }
  requireValue(
    fileHash(dmgOut) === dmgHash &&
      appInventory(app).sha256 === report.inventory.sha256,
    "成果物のコピー中に内容が変更されました",
  );
  const manifest = {
    schema: "kb-app.macos-release/v1",
    source_commit: plan.source_commit,
    version: plan.version,
    target: plan.target,
    minimum_system_version: plan.minimum_system_version,
    mode: plan.mode,
    plan_sha256: sha256(planBytes),
    git_lfs_provenance: plan.git_lfs_provenance,
    git_provenance: plan.git_provenance,
    distribution_verified:
      plan.mode === "release" && report.blockers.length === 0,
    public_release_accepted: false,
    blockers: report.blockers,
    unverified: [
      "clean_mac_first_record_and_search",
      "app_update_and_database_recovery",
      "live_client_reconnection",
    ],
    updater: plan.updater,
    updater_artifacts: updateArtifacts.verification ?? {
      signature_verified: false,
      coexistence_verified: false,
    },
    compatibility: plan.compatibility,
    compatible_sources: plan.compatible_sources,
    signature: report.signature,
    architectures: report.architectures,
    mach_o_files: report.mach_o_files,
    bundled_git: report.bundled_git,
    app: report.inventory,
    artifacts: [
      appZip,
      dmgOut,
      ...updateCopies.map((artifact) => artifact.destination),
    ].map((path) => ({
      name: basename(path),
      size: lstatSync(path).size,
      sha256: fileHash(path),
    })),
  };
  writeFileSync(
    resolve(output, "manifest.json"),
    `${JSON.stringify(manifest, null, 2)}\n`,
    { flag: "wx" },
  );
  return manifest;
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  try {
    const report = verifyArtifacts(
      argumentsFor(
        process.argv.slice(2),
        [
          "plan",
          "app",
          "dmg",
          "output",
          "updater-verifier",
          "updater-verifier-sha256",
          "updater-current-version",
        ],
        ["plan", "app", "dmg", "output"],
      ),
    );
    process.stdout.write(
      `候補検査完了。一般配布受入は未確認。blockers: ${report.blockers.join(", ") || "none"}\n`,
    );
    if (report.mode === "release" && !report.distribution_verified)
      process.exitCode = 1;
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
