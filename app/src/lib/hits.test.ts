import { describe, expect, it } from "vitest";

import type { Hit } from "@/lib/api";

import { activeFilterCount, matchesFilter, paginate, sortHits } from "./hits";

const hit = (over: Partial<Hit>): Hit => ({
  id: "notes/x",
  title: "x",
  status: "stable",
  snippet: "",
  via: "recent",
  origin: "agent",
  tags: [],
  created: null,
  updated: null,
  note_uid: null,
  namespace: null,
  authority_role: null,
  authority_status: null,
  authority_scope: null,
  ...over,
});

const NOW = new Date("2026-08-11T00:00:00Z").getTime();
const daysAgo = (n: number) => new Date(NOW - n * 86_400_000).toISOString();

describe("matchesFilter", () => {
  it("選んだタグをすべて持つノートだけ残す(AND)", () => {
    const h = hit({ tags: ["手続き", "税金"] });
    expect(matchesFilter(h, { tags: ["手続き"], period: "all" }, NOW)).toBe(true);
    expect(matchesFilter(h, { tags: ["手続き", "税金"], period: "all" }, NOW)).toBe(true);
    expect(matchesFilter(h, { tags: ["手続き", "旅行"], period: "all" }, NOW)).toBe(false);
  });

  it("期間はちょうど境界の日を含む", () => {
    const h = hit({ updated: daysAgo(7) });
    expect(matchesFilter(h, { tags: [], period: "7" }, NOW)).toBe(true);
    expect(matchesFilter(hit({ updated: daysAgo(8) }), { tags: [], period: "7" }, NOW)).toBe(false);
  });

  it("更新日が無いノートは期間指定で外れる(全期間なら残る)", () => {
    const h = hit({ updated: null });
    expect(matchesFilter(h, { tags: [], period: "all" }, NOW)).toBe(true);
    expect(matchesFilter(h, { tags: [], period: "30" }, NOW)).toBe(false);
  });
});

describe("sortHits", () => {
  const a = hit({ id: "a", title: "あ", updated: daysAgo(1), created: daysAgo(9) });
  const b = hit({ id: "b", title: "い", updated: daysAgo(5), created: daysAgo(2) });

  it("更新順・作成順で並べ替える", () => {
    expect(sortHits([b, a], "updated", "ja").map((h) => h.id)).toEqual(["a", "b"]);
    expect(sortHits([a, b], "created", "ja").map((h) => h.id)).toEqual(["b", "a"]);
  });

  it("タイトル順はロケールに従う", () => {
    expect(sortHits([b, a], "title", "ja").map((h) => h.id)).toEqual(["a", "b"]);
  });

  it("元の配列を壊さない", () => {
    const input = [b, a];
    sortHits(input, "updated", "ja");
    expect(input.map((h) => h.id)).toEqual(["b", "a"]);
  });
});

describe("paginate", () => {
  const items = Array.from({ length: 7 }, (_, i) => i);

  it("範囲外のページ番号は最後のページに丸める", () => {
    const p = paginate(items, 3, 99);
    expect(p.page).toBe(2);
    expect(p.items).toEqual([6]);
    expect(p.from).toBe(7);
    expect(p.to).toBe(7);
  });

  it("空でも壊れない", () => {
    const p = paginate([], 30, 0);
    expect(p).toMatchObject({ page: 0, pageCount: 1, total: 0, from: 0, to: 0 });
  });
});

describe("activeFilterCount", () => {
  it("タグ数と期間指定の有無を足す", () => {
    expect(activeFilterCount({ tags: ["a", "b"], period: "all" })).toBe(2);
    expect(activeFilterCount({ tags: ["a"], period: "30" })).toBe(2);
  });
});
