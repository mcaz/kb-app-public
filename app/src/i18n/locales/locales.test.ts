import { describe, expect, it } from "vitest";

import en from "./en";
import ja from "./ja";

/** ネストした辞書を "notes.filter.sort" 形式のキー一覧へ潰す。 */
function flatten(value: unknown, prefix = ""): string[] {
  if (typeof value !== "object" || value === null) return [prefix];
  return Object.entries(value).flatMap(([k, v]) => flatten(v, prefix ? `${prefix}.${k}` : k));
}

describe("翻訳リソース", () => {
  const jaKeys = flatten(ja).sort();
  const enKeys = flatten(en).sort();

  it("日本語にあるキーは英語にもある", () => {
    expect(jaKeys.filter((k) => !enKeys.includes(k))).toEqual([]);
  });

  it("英語だけに存在する余分なキーが無い", () => {
    expect(enKeys.filter((k) => !jaKeys.includes(k))).toEqual([]);
  });

  it("空文字の訳が無い", () => {
    const empty = (o: unknown, p = ""): string[] =>
      typeof o === "string"
        ? o.trim() === ""
          ? [p]
          : []
        : typeof o === "object" && o !== null
          ? Object.entries(o).flatMap(([k, v]) => empty(v, p ? `${p}.${k}` : k))
          : [];
    expect([...empty(ja), ...empty(en)]).toEqual([]);
  });
});
