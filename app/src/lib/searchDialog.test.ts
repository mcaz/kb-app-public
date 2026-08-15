import { describe, expect, it } from "vitest";

import { effectiveSearchPane, resolveSearchSelection } from "./searchDialog";

describe("resolveSearchSelection", () => {
  it("検索結果に残っている選択を維持する", () => {
    expect(resolveSearchSelection("b", ["a", "b", "c"])).toBe("b");
  });

  it("選択が消えたら先頭へ寄せ、空なら未選択にする", () => {
    expect(resolveSearchSelection("x", ["a", "b"])).toBe("a");
    expect(resolveSearchSelection("x", [])).toBeNull();
  });
});

describe("effectiveSearchPane", () => {
  it("狭幅だけプレビューへの画面遷移を持つ", () => {
    expect(effectiveSearchPane(true, "preview")).toBe("preview");
    expect(effectiveSearchPane(false, "preview")).toBe("results");
  });
});
