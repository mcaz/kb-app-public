import { describe, expect, it } from "vitest";

import { buildCategoryTree, categoryAncestorPaths } from "./categoryTree";

describe("buildCategoryTree", () => {
  it("カテゴリを任意の深さのディレクトリ階層へ変換する", () => {
    expect(
      buildCategoryTree([
        { path: "research/ai", name: "ai", count: 1 },
        { path: "notes", name: "notes", count: 2 },
        { path: "research", name: "research", count: 3 },
        { path: "", name: "", count: 1 },
      ]),
    ).toEqual([
      { path: "", name: "", count: 1, children: [] },
      { path: "notes", name: "notes", count: 2, children: [] },
      {
        path: "research",
        name: "research",
        count: 3,
        children: [{ path: "research/ai", name: "ai", count: 1, children: [] }],
      },
    ]);
  });
});

describe("categoryAncestorPaths", () => {
  it("浅い順に祖先 path を返す", () => {
    expect(categoryAncestorPaths("research/ai/models")).toEqual(["research", "research/ai"]);
    expect(categoryAncestorPaths("research")).toEqual([]);
  });
});
