import assert from "node:assert/strict";
import { log } from "node:console";
import { mkdir, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { ESLint } from "eslint";

const appRoot = path.resolve(import.meta.dirname, "../..");
const componentSandbox = path.join(
  appRoot,
  "src/components/molecules/__eslint_invoke_boundary_test__",
);
const apiSandbox = path.join(appRoot, "src/lib/api/__eslint_invoke_boundary_test__");
const eslint = new ESLint({ cwd: appRoot });

async function restrictedImportMessages(directory, name, source) {
  const file = path.join(directory, name);
  await writeFile(file, source, "utf8");
  const [result] = await eslint.lintFiles([file]);
  return result.messages.filter((message) => message.ruleId === "no-restricted-imports");
}

await mkdir(componentSandbox, { recursive: true });
await mkdir(apiSandbox, { recursive: true });

try {
  const direct = await restrictedImportMessages(
    componentSandbox,
    "direct.ts",
    'import { invoke } from "@tauri-apps/api/core";\nvoid invoke;\n',
  );
  assert.equal(direct.length, 1, "component からの invoke import は拒否する");

  const namespace = await restrictedImportMessages(
    componentSandbox,
    "namespace.ts",
    'import * as tauriCore from "@tauri-apps/api/core";\nvoid tauriCore;\n',
  );
  assert.equal(namespace.length, 1, "namespace import で invoke 境界を迂回させない");

  const reexport = await restrictedImportMessages(
    componentSandbox,
    "reexport.ts",
    'export { invoke as callTauri } from "@tauri-apps/api/core";\n',
  );
  assert.equal(reexport.length, 1, "invoke の再exportも拒否する");

  const ordinaryApi = await restrictedImportMessages(
    componentSandbox,
    "ordinary-api.ts",
    'import { convertFileSrc } from "@tauri-apps/api/core";\nvoid convertFileSrc;\n',
  );
  assert.equal(ordinaryApi.length, 0, "invoke ではないTauri APIは許可する");

  const apiBoundary = await restrictedImportMessages(
    apiSandbox,
    "allowed.ts",
    'import { invoke } from "@tauri-apps/api/core";\nvoid invoke;\n',
  );
  assert.equal(apiBoundary.length, 0, "src/lib/api は invoke を包める");

  log("invoke boundary lint: 5 cases passed");
} finally {
  await rm(componentSandbox, { recursive: true, force: true });
  await rm(apiSandbox, { recursive: true, force: true });
}
