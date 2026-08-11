import type { GraphData } from "@/lib/api";

/** 中心ノートから hops ホップ以内の部分グラフを取り出す(ローカルグラフ用)。 */
export function subgraph(data: GraphData, centerId: string, hops = 2): GraphData {
  const adjacency = new Map<string, Set<string>>();
  const link = (a: string, b: string) => {
    const set = adjacency.get(a) ?? new Set<string>();
    set.add(b);
    adjacency.set(a, set);
  };
  for (const [a, b] of data.edges) {
    link(a, b);
    link(b, a);
  }

  const keep = new Set<string>([centerId]);
  let frontier = [centerId];
  for (let h = 0; h < hops && frontier.length > 0; h++) {
    const next: string[] = [];
    for (const id of frontier) {
      for (const neighbour of adjacency.get(id) ?? []) {
        if (!keep.has(neighbour)) {
          keep.add(neighbour);
          next.push(neighbour);
        }
      }
    }
    frontier = next;
  }

  return {
    nodes: data.nodes.filter((n) => keep.has(n.id)),
    edges: data.edges.filter(([a, b]) => keep.has(a) && keep.has(b)),
  };
}
