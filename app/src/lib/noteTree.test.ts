import { describe, expect, it } from "vitest";

import { buildNoteTree, noteAncestorPaths } from "./noteTree";

import type { Hit } from "@/lib/api";

const hit = (id: string, title: string | null): Hit => ({
  id,
  title,
  status: "stable",
  snippet: "",
  via: "recent",
  origin: "agent",
  tags: [],
  created: null,
  updated: null,
});

describe("buildNoteTree", () => {
  it("ノート ID を任意の深さのディレクトリ階層へ変換する", () => {
    expect(
      buildNoteTree([
        hit("research/ai/検索", "検索"),
        hit("notes/買い物", "買い物"),
        hit("research/概要", "概要"),
        hit("入口", "入口"),
      ]),
    ).toEqual([
      {
        kind: "folder",
        name: "notes",
        path: "notes",
        children: [{ kind: "note", id: "notes/買い物", title: "買い物" }],
      },
      {
        kind: "folder",
        name: "research",
        path: "research",
        children: [
          {
            kind: "folder",
            name: "ai",
            path: "research/ai",
            children: [{ kind: "note", id: "research/ai/検索", title: "検索" }],
          },
          { kind: "note", id: "research/概要", title: "概要" },
        ],
      },
      { kind: "note", id: "入口", title: "入口" },
    ]);
  });

  it("タイトルが無い場合は ID の末尾を表示名にする", () => {
    expect(buildNoteTree([hit("notes/無題", null)])).toEqual([
      {
        kind: "folder",
        name: "notes",
        path: "notes",
        children: [{ kind: "note", id: "notes/無題", title: "無題" }],
      },
    ]);
  });
});

describe("noteAncestorPaths", () => {
  it("浅い順に祖先 path を返す", () => {
    expect(noteAncestorPaths("research/ai/検索")).toEqual(["research", "research/ai"]);
    expect(noteAncestorPaths("入口")).toEqual([]);
  });
});
