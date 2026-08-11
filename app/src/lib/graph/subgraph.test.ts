import { describe, expect, it } from "vitest";

import type { GraphData } from "@/lib/api";

import { subgraph } from "./subgraph";

const node = (id: string) => ({ id, title: id, origin: "agent", status: "stable", degree: 0 });

// a — b — c — d、および孤立した z
const data: GraphData = {
  nodes: ["a", "b", "c", "d", "z"].map(node),
  edges: [
    ["a", "b"],
    ["b", "c"],
    ["c", "d"],
  ],
};

describe("subgraph", () => {
  it("指定ホップ数までのノードを集める", () => {
    expect(
      subgraph(data, "a", 1)
        .nodes.map((n) => n.id)
        .sort(),
    ).toEqual(["a", "b"]);
    expect(
      subgraph(data, "a", 2)
        .nodes.map((n) => n.id)
        .sort(),
    ).toEqual(["a", "b", "c"]);
  });

  it("両端が残っているエッジだけを持ち帰る", () => {
    expect(subgraph(data, "a", 2).edges).toEqual([
      ["a", "b"],
      ["b", "c"],
    ]);
  });

  it("辿り着けないノートは入らない", () => {
    expect(subgraph(data, "a", 9).nodes.map((n) => n.id)).not.toContain("z");
  });

  it("孤立ノートを中心にしても自分だけ返る", () => {
    expect(subgraph(data, "z", 2)).toEqual({ nodes: [node("z")], edges: [] });
  });
});
