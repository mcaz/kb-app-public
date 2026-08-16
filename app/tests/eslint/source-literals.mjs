import assert from "node:assert/strict";
import { log } from "node:console";
import { mkdir, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { ESLint } from "eslint";

const appRoot = path.resolve(import.meta.dirname, "../..");
const sandbox = path.join(appRoot, "src/components/molecules/__eslint_source_literal_test__");
const eslint = new ESLint({ cwd: appRoot });

async function restrictedSyntaxMessages(name, source) {
  const file = path.join(sandbox, name);
  await writeFile(file, source, "utf8");
  const [result] = await eslint.lintFiles([file]);
  return result.messages.filter((message) => message.ruleId === "no-restricted-syntax");
}

await mkdir(sandbox, { recursive: true });

try {
  const japaneseText = await restrictedSyntaxMessages(
    "japanese-text.tsx",
    "export const Example = () => <p>日本語</p>;\n",
  );
  assert.equal(japaneseText.length, 1, "JSXText の日本語は拒否する");

  const japaneseAttribute = await restrictedSyntaxMessages(
    "japanese-attribute.tsx",
    'export const Example = () => <button aria-label="保存" />;\n',
  );
  assert.equal(japaneseAttribute.length, 1, "JSX 属性の日本語は拒否する");

  const japaneseExpression = await restrictedSyntaxMessages(
    "japanese-expression.tsx",
    'export const Example = () => <p>{"日本語"}</p>;\n',
  );
  assert.equal(japaneseExpression.length, 1, "JSX 式内の日本語文字列は拒否する");

  const japaneseTemplate = await restrictedSyntaxMessages(
    "japanese-template.tsx",
    "export const Example = () => <p>{`日本語`}</p>;\n",
  );
  assert.equal(japaneseTemplate.length, 1, "JSX 式内の日本語テンプレートは拒否する");

  const japaneseNonUi = await restrictedSyntaxMessages(
    "japanese-non-ui.tsx",
    "// 日本語コメントは許可する\nexport const matcher = /お気に入り/;\n",
  );
  assert.equal(japaneseNonUi.length, 0, "コメントと正規表現の日本語は許可する");

  const rawHex = await restrictedSyntaxMessages("raw-hex.tsx", 'export const color = "#A97B2F";\n');
  assert.equal(rawHex.length, 1, "TSX の hex 文字列は拒否する");

  const rawHexTemplate = await restrictedSyntaxMessages(
    "raw-hex-template.tsx",
    "export const color = `#fff`;\n",
  );
  assert.equal(rawHexTemplate.length, 1, "TSX の hex テンプレートは拒否する");

  const token = await restrictedSyntaxMessages(
    "token.tsx",
    'export const Example = () => <p className="text-ink">text</p>;\n',
  );
  assert.equal(token.length, 0, "design token は許可する");

  const canvasFallback = await restrictedSyntaxMessages(
    "canvas-fallback.ts",
    'export const fallback = "#3E7550";\n',
  );
  assert.equal(canvasFallback.length, 0, ".ts の canvas fallback は許可する");

  log("source literal lint: 9 cases passed");
} finally {
  await rm(sandbox, { recursive: true, force: true });
}
