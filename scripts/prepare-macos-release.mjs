import { createHash } from "node:crypto";
import {
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  writeFileSync,
} from "node:fs";
import { dirname, relative, resolve, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawnSync } from "node:child_process";

export const PLAN_SCHEMA = "kb-app.macos-release-plan/v1";
export const TARGETS = {
  "aarch64-apple-darwin": "arm64",
  "x86_64-apple-darwin": "x86_64",
};
export const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export const SOURCE_INPUTS = [
  "Cargo.lock",
  "app/package-lock.json",
  "scripts/git-lfs-assets.json",
  "scripts/git-assets.json",
  "app/src-tauri/updater-config.json",
  "crates/kb-core/update-compatibility.json",
];
export const sha256 = (bytes) =>
  createHash("sha256").update(bytes).digest("hex");

export const lfsSidecar = (target) =>
  `app/src-tauri/binaries/git-lfs-${target}`;
export const gitSidecar = (target) => `app/src-tauri/binaries/git-${target}`;
export const GIT_RESOURCE_DIRS = {
  "git-source": "source",
  "licenses/git": "licenses",
  "git-templates": "templates",
};

// 生成schemaとは独立に、配布する対応ソース・ライセンス・templateの実bytesを固定する。
export function gitResources(root) {
  const resources = {};
  for (const [destination, source] of Object.entries(GIT_RESOURCE_DIRS)) {
    const base = resolve(root, "app/src-tauri/git-runtime", source);
    requireValue(
      lstatSync(base).isDirectory(),
      "Git配布資材が通常directoryではありません",
    );
    function walk(directory) {
      for (const name of readdirSync(directory).sort()) {
        const path = resolve(directory, name);
        const stat = lstatSync(path);
        if (stat.isDirectory()) walk(path);
        else {
          requireValue(stat.isFile(), "Git配布資材に非通常ファイルがあります");
          const key = `${destination}/${relative(base, path).split(sep).join("/")}`;
          resources[key] = sha256(readFileSync(path));
        }
      }
    }
    walk(base);
  }
  return resources;
}

export function requireValue(condition, message) {
  if (!condition) throw new Error(message);
}

function strictBase64(value) {
  requireValue(
    typeof value === "string" && /^[A-Za-z0-9+/]+={0,2}$/.test(value),
    "更新公開鍵のbase64形式が不正です",
  );
  const bytes = Buffer.from(value, "base64");
  requireValue(
    bytes.toString("base64") === value,
    "更新公開鍵のbase64形式が不正です",
  );
  return bytes;
}

export function validateUpdaterConfiguration(config) {
  requireValue(
    config &&
      config.format_version === 1 &&
      Object.keys(config).sort().join(",") ===
        "endpoint,format_version,public_key",
    "updater設定schemaが不正です",
  );
  if (config.endpoint === null && config.public_key === null) return config;
  requireValue(
    typeof config.endpoint === "string" &&
      config.endpoint.length < 4096 &&
      config.endpoint === config.endpoint.trim() &&
      !/[\u0000-\u0020\u007f]/.test(config.endpoint),
    "更新endpointが不正または片方だけ設定されています",
  );
  let endpoint;
  try {
    endpoint = new URL(config.endpoint);
  } catch {
    throw new Error("更新endpointのURIが不正です");
  }
  requireValue(
    endpoint.protocol === "https:" &&
      endpoint.hostname &&
      !endpoint.username &&
      !endpoint.password &&
      !endpoint.hash,
    "更新endpointは資格情報・fragmentなしのHTTPSにしてください",
  );
  requireValue(
    typeof config.public_key === "string" && config.public_key.length < 4096,
    "更新公開鍵が未設定または不正です",
  );
  const decoded = new TextDecoder("utf-8", { fatal: true }).decode(
    strictBase64(config.public_key),
  );
  const lines = decoded.replace(/\r\n/g, "\n").replace(/\n$/, "").split("\n");
  requireValue(
    lines.length === 2 && lines[0].startsWith("untrusted comment: "),
    "更新公開鍵はTauriの公開key形式にしてください",
  );
  const key = strictBase64(lines[1]);
  requireValue(
    key.length === 42 &&
      ["Ed", "ED"].includes(key.subarray(0, 2).toString("ascii")),
    "更新公開鍵のalgorithmまたは長さが不正です",
  );
  return config;
}

export function validateCompatibility(value) {
  requireValue(
    value &&
      Object.keys(value).sort().join(",") ===
        "database_schema,persistent_compatibility_epoch,runtime_store" &&
      value.runtime_store === "db-v1" &&
      Number.isSafeInteger(value.database_schema) &&
      value.database_schema > 0 &&
      Number.isSafeInteger(value.persistent_compatibility_epoch) &&
      value.persistent_compatibility_epoch > 0,
    "更新互換性metadataが欠落または不正です",
  );
  return value;
}

export function validateEnvironment(mode, environment) {
  requireValue(["candidate", "release"].includes(mode), "modeが不正です");
  if (mode === "release") {
    requireValue(
      environment.APPLE_SIGNING_IDENTITY?.startsWith(
        "Developer ID Application:",
      ),
      "Developer ID Application署名が必要です",
    );
    for (const name of [
      "APPLE_API_ISSUER",
      "APPLE_API_KEY",
      "APPLE_API_KEY_PATH",
    ]) {
      requireValue(environment[name], `${name} が未設定です`);
    }
  } else {
    // 2026-09-08: 取得・buildの子processを起動する前にも、候補へ資格が渡ることを防ぐ。
    requireValue(
      ![
        "APPLE_ID",
        "APPLE_PASSWORD",
        "APPLE_API_KEY",
        "APPLE_API_ISSUER",
        "APPLE_API_KEY_PATH",
        "APPLE_CERTIFICATE",
        "APPLE_CERTIFICATE_PASSWORD",
        "APPLE_API_PRIVATE_KEY",
        "KEYCHAIN_PASSWORD",
        "TAURI_SIGNING_PRIVATE_KEY",
        "TAURI_SIGNING_PRIVATE_KEY_PASSWORD",
      ].some((name) => environment[name]) &&
        (!environment.APPLE_SIGNING_IDENTITY ||
          environment.APPLE_SIGNING_IDENTITY === "-"),
      "candidateにはApple認証情報を渡さないでください",
    );
  }
}

export function argumentsFor(argv, allowed, required = allowed) {
  const values = {};
  for (let i = 0; i < argv.length; i += 2) {
    const name = argv[i]?.replace(/^--/, "");
    requireValue(
      argv[i]?.startsWith("--") && allowed.includes(name),
      "未知の引数です",
    );
    requireValue(
      argv[i + 1] && !argv[i + 1].startsWith("--") && !(name in values),
      "引数の値が欠落または重複しています",
    );
    values[name] = argv[i + 1];
  }
  for (const name of required)
    requireValue(values[name], `--${name} が必要です`);
  return values;
}

export function run(program, args, options = {}) {
  const result = spawnSync(program, args, {
    encoding: "utf8",
    timeout: 120_000,
    maxBuffer: 4 * 1024 * 1024,
    ...options,
  });
  // 子processのstderrやenvには認証情報を含み得るので、失敗時もそのままログへ渡さない。
  requireValue(
    !result.error && result.status === 0,
    `${program} の検査に失敗しました`,
  );
  return `${result.stdout ?? ""}${result.stderr ?? ""}`.trim();
}

export function validatePlan(plan) {
  requireValue(plan.schema === PLAN_SCHEMA, "候補計画schemaが不正です");
  requireValue(["candidate", "release"].includes(plan.mode), "modeが不正です");
  requireValue(
    /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(plan.version),
    "versionはX.Y.Zで指定してください",
  );
  requireValue(
    /^[a-f0-9]{40}$/.test(plan.source_commit),
    "source commitは完全な40桁SHAが必要です",
  );
  requireValue(
    Object.hasOwn(TARGETS, plan.target),
    "対応していないCPU targetです",
  );
  requireValue(
    /^\d+\.\d+(\.\d+)?$/.test(plan.minimum_system_version),
    "macOS下限の形式が不正です",
  );
  requireValue(
    plan.product_name === "kb-app" && plan.identifier === "app.kb.desktop",
    "配布アプリの識別子が不正です",
  );
  requireValue(
    plan.mode !== "release" || /^[A-Z0-9]{10}$/.test(plan.apple_team_id),
    "releaseにはApple Team IDが必要です",
  );
  requireValue(
    typeof plan.git_lfs_version === "string" &&
      /^\d+\.\d+\.\d+$/.test(plan.git_lfs_version),
    "Git LFS版が不正です",
  );
  for (const path of [
    "licenses/git-lfs-LICENSE.md",
    "licenses/THIRD_PARTY_NOTICES.md",
  ]) {
    requireValue(
      /^[a-f0-9]{64}$/.test(plan.resources?.[path] ?? ""),
      "同梱ライセンスのhashが欠落しています",
    );
  }
  requireValue(
    Object.keys(plan.resources).length === 2,
    "未知の同梱ライセンス指定があります",
  );
  requireValue(
    Object.keys(plan.source_inputs ?? {}).length === SOURCE_INPUTS.length &&
      SOURCE_INPUTS.every((path) =>
        /^[a-f0-9]{64}$/.test(plan.source_inputs[path] ?? ""),
      ),
    "build入力のhashが不正です",
  );
  validateCompatibility(plan.compatibility);
  // 新旧の実行試験を検証するreceipt入口ができるまで、任意JSONの自己申告から有効化しない。
  requireValue(
    Array.isArray(plan.compatible_sources) &&
      plan.compatible_sources.length === 0,
    "更新元の共存receiptは未検証です。compatible_sourcesは空にしてください",
  );
  validateUpdaterConfiguration(plan.updater_config);
  requireValue(
    plan.updater ===
      (plan.updater_config.endpoint === null ? "not_configured" : "configured"),
    "updater設定と計画が一致しません",
  );
  requireValue(
    plan.git_lfs_provenance?.version === plan.git_lfs_version &&
      plan.git_lfs_provenance?.target === plan.target,
    "Git LFS provenanceの版・CPUが不正です",
  );
  for (const key of ["archive_sha256", "binary_sha256"]) {
    requireValue(
      /^[a-f0-9]{64}$/.test(plan.git_lfs_provenance[key] ?? ""),
      "Git LFS provenanceのhashが不正です",
    );
  }
  requireValue(
    plan.git_provenance === null ||
      (plan.git_provenance?.target === plan.target &&
        /^\d+\.\d+\.\d+$/.test(plan.git_provenance.version) &&
        ["document_sha256", "binary_sha256", "source_archive_sha256"].every(
          (key) => /^[a-f0-9]{64}$/.test(plan.git_provenance[key]),
        )),
    "Git provenanceが不正です",
  );
  if (plan.git_provenance) {
    const resources = plan.git_provenance.resources;
    requireValue(
      resources && typeof resources === "object" && !Array.isArray(resources),
      "Git配布資材のhashがありません",
    );
    requireValue(
      Object.entries(resources).every(
        ([path, digest]) =>
          Object.keys(GIT_RESOURCE_DIRS).some((prefix) =>
            path.startsWith(`${prefix}/`),
          ) &&
          path
            .split("/")
            .every((part) => part !== "" && part !== "." && part !== "..") &&
          !/[\\\x00-\x1f\x7f]/.test(path) &&
          /^[a-f0-9]{64}$/.test(digest),
      ),
      "Git配布資材のpath/hashが不正です",
    );
    for (const prefix of Object.keys(GIT_RESOURCE_DIRS)) {
      requireValue(
        Object.keys(resources).some((path) => path.startsWith(`${prefix}/`)),
        "Git対応ソース・ライセンス・templateが欠けています",
      );
    }
    for (const path of [
      `git-source/git-${plan.git_provenance.version}.tar.xz`,
      "git-source/prepare-git.mjs",
      "git-source/git-assets.json",
      "licenses/git/COPYING",
    ]) {
      requireValue(
        resources[path],
        "Gitの対応ソースarchive・build手順が欠けています",
      );
    }
    requireValue(
      resources[`git-source/git-${plan.git_provenance.version}.tar.xz`] ===
        plan.git_provenance.source_archive_sha256,
      "Git対応ソースarchiveのhashが一致しません",
    );
  }
  return plan;
}

export function prepare(
  options,
  root = ROOT,
  execute = run,
  environment = process.env,
) {
  validateEnvironment(options.mode, environment);
  const read = (path) => readFileSync(resolve(root, path));
  const json = (path) => JSON.parse(read(path));
  const config = json("app/src-tauri/tauri.conf.json");
  const packageJson = json("app/package.json");
  const cargo = read("Cargo.toml").toString();
  const workspaceSection = cargo.match(
    /\[workspace\.package\]([\s\S]*?)(?=\n\[|$)/,
  )?.[1];
  const cargoVersion = workspaceSection?.match(
    /^version\s*=\s*"([^"]+)"/m,
  )?.[1];
  const lfs = json("scripts/git-lfs-assets.json");
  const git = json("scripts/git-assets.json");
  const updaterConfig = validateUpdaterConfiguration(
    json("app/src-tauri/updater-config.json"),
  );
  const compatibility = validateCompatibility(
    json("crates/kb-core/update-compatibility.json"),
  );
  if (updaterConfig.endpoint !== null) {
    requireValue(
      options.mode === "release",
      "更新配信用設定はreleaseでのみ使用してください",
    );
    requireValue(
      typeof environment.TAURI_SIGNING_PRIVATE_KEY === "string" &&
        environment.TAURI_SIGNING_PRIVATE_KEY.trim(),
      "更新成果物の署名にはTAURI_SIGNING_PRIVATE_KEYが必要です",
    );
  }
  requireValue(
    Object.hasOwn(TARGETS, options.target),
    "対応していないCPU targetです",
  );
  const provenance = json(`${lfsSidecar(options.target)}.provenance.json`);
  requireValue(
    provenance.schema === 1 &&
      provenance.archive_sha256 === lfs.targets[options.target]?.sha256 &&
      provenance.binary_sha256 === sha256(read(lfsSidecar(options.target))),
    "Git LFSの公式archive・署名前実bytesの照合に失敗しました",
  );
  const resources = {
    "licenses/git-lfs-LICENSE.md": sha256(
      read("third_party/git-lfs/LICENSE.md"),
    ),
    "licenses/THIRD_PARTY_NOTICES.md": sha256(read("THIRD_PARTY_NOTICES.md")),
  };
  const gitEvidencePath = resolve(
    root,
    "app/src-tauri/git-runtime/provenance.json",
  );
  const gitBinaryPath = resolve(root, gitSidecar(options.target));
  requireValue(
    existsSync(gitEvidencePath) === existsSync(gitBinaryPath),
    "Git本体とprovenanceの片方が欠けています",
  );
  let gitProvenance = null;
  if (existsSync(gitEvidencePath)) {
    const evidenceBytes = readFileSync(gitEvidencePath);
    const evidence = JSON.parse(evidenceBytes);
    const binarySha = sha256(readFileSync(gitBinaryPath));
    requireValue(
      evidence.schema === "1.0" &&
        evidence.version === git.version &&
        evidence.target === options.target &&
        evidence.minimum_system_version === options["minimum-system-version"] &&
        evidence.source?.sha256 === git.source?.sha256 &&
        /^[a-f0-9]{64}$/.test(git.source?.sha256) &&
        evidence.binary_sha256 === binarySha,
      "Git公式source・build条件・署名前実bytesの照合に失敗しました",
    );
    const resources = gitResources(root);
    for (const script of ["prepare-git.mjs", "git-assets.json"]) {
      requireValue(
        resources[`git-source/${script}`] === sha256(read(`scripts/${script}`)),
        "Git対応ソースのbuild手順が実際の手順と一致しません",
      );
    }
    gitProvenance = {
      version: git.version,
      target: options.target,
      document_sha256: sha256(evidenceBytes),
      binary_sha256: binarySha,
      source_archive_sha256: git.source.sha256,
      resources,
    };
  }
  const plan = validatePlan({
    schema: PLAN_SCHEMA,
    mode: options.mode,
    version: options.version,
    source_commit: options["source-commit"],
    target: options.target,
    minimum_system_version: options["minimum-system-version"],
    product_name: config.productName,
    identifier: config.identifier,
    apple_team_id:
      options.mode === "release" ? environment.APPLE_TEAM_ID : null,
    git_lfs_version: lfs.version,
    git_provenance: gitProvenance,
    git_lfs_provenance: {
      version: provenance.version,
      target: provenance.target,
      archive_sha256: provenance.archive_sha256,
      binary_sha256: provenance.binary_sha256,
    },
    resources,
    source_inputs: Object.fromEntries(
      SOURCE_INPUTS.map((path) => [path, sha256(read(path))]),
    ),
    compatibility,
    compatible_sources: [],
    updater: updaterConfig.endpoint === null ? "not_configured" : "configured",
    updater_config: updaterConfig,
  });
  // 2026-09-08: Tauri overrideだけで版を変えると、MCPのCORE_VERSIONと配布版が食い違う。
  requireValue(
    [config.version, packageJson.version, cargoVersion].every(
      (version) => version === plan.version,
    ),
    "Cargo・npm・Tauriと指定versionが一致しません。正本の版を先に揃えてください",
  );
  requireValue(
    execute("git", ["rev-parse", "HEAD"], { cwd: root }) === plan.source_commit,
    "指定source commitとcheckoutが一致しません",
  );
  requireValue(
    execute("git", ["status", "--porcelain", "--untracked-files=all"], {
      cwd: root,
    }) === "",
    "sourceに未commit変更または未追跡ファイルがあります",
  );
  requireValue(lfs.targets[plan.target], "指定targetのGit LFSが未準備です");
  const output = resolve(options.output);
  const override = {
    version: plan.version,
    bundle: {
      createUpdaterArtifacts: plan.updater === "configured",
      resources: {
        ...config.bundle.resources,
        [resolve(root, "THIRD_PARTY_NOTICES.md")]:
          "licenses/THIRD_PARTY_NOTICES.md",
        [resolve(output, "plan.json")]: "release/plan.json",
      },
      macOS: {
        minimumSystemVersion: plan.minimum_system_version,
        hardenedRuntime: true,
        signingIdentity:
          plan.mode === "candidate" ? "-" : environment.APPLE_SIGNING_IDENTITY,
      },
    },
  };
  mkdirSync(output, { recursive: true });
  writeFileSync(
    resolve(output, "plan.json"),
    `${JSON.stringify(plan, null, 2)}\n`,
    { flag: "wx" },
  );
  writeFileSync(
    resolve(output, "tauri-release.json"),
    `${JSON.stringify(override, null, 2)}\n`,
    { flag: "wx" },
  );
  return plan;
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  try {
    prepare(
      argumentsFor(process.argv.slice(2), [
        "mode",
        "version",
        "source-commit",
        "target",
        "minimum-system-version",
        "output",
      ]),
    );
    process.stdout.write("macOS候補計画とTauri overrideを作成しました\n");
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
