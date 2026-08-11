import {
  forceCenter,
  forceCollide,
  forceLink,
  forceManyBody,
  forceSimulation,
  type SimulationLinkDatum,
  type SimulationNodeDatum,
} from "d3-force";

import type { GraphData, GraphNode } from "@/lib/api";

type Node = SimulationNodeDatum & GraphNode;
type Link = SimulationLinkDatum<Node>;

export interface ForceGraphOptions {
  container: HTMLElement;
  canvas: HTMLCanvasElement;
  data: GraphData;
  /** 局所グラフの中心(色と大きさを変える)。 */
  centerId?: string | null;
  onOpen: (id: string) => void;
}

export interface ForceGraphHandle {
  destroy: () => void;
}

/**
 * つながりグラフ(canvas + d3-force)。React に依存しない — 描画は毎フレーム
 * 自前で行うため、フレームワーク側の再描画に混ぜない方が素直なので分けている。
 * 後片付け(シミュレーション停止・監視/購読の解除)は destroy() に集約する。
 */
export function createForceGraph({
  container,
  canvas,
  data,
  centerId,
  onOpen,
}: ForceGraphOptions): ForceGraphHandle {
  const css = getComputedStyle(document.documentElement);
  const color = (name: string, fallback: string) => css.getPropertyValue(name).trim() || fallback;
  const colNode = color("--color-grow", "#3E7550");
  const colCenter = color("--color-prop", "#A97B2F");
  const colLine = color("--color-line", "#888");
  const colInk = color("--color-ink", "#222");

  const ctx = canvas.getContext("2d");
  if (!ctx) return { destroy: () => undefined };
  const dpr = window.devicePixelRatio || 1;

  const nodes: Node[] = data.nodes.map((n) => ({ ...n }));
  const links: Link[] = data.edges.map(([source, target]) => ({ source, target }));
  const view = { x: 0, y: 0, k: 1 };
  let hovered: Node | null = null;

  const radius = (n: Node) =>
    n.id === centerId ? 7 : 3.5 + Math.min(6, Math.sqrt(n.degree) * 1.6);

  const resize = () => {
    canvas.width = container.clientWidth * dpr;
    canvas.height = container.clientHeight * dpr;
  };

  function draw() {
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, canvas.width / dpr, canvas.height / dpr);
    ctx.translate(view.x, view.y);
    ctx.scale(view.k, view.k);

    ctx.strokeStyle = colLine;
    ctx.lineWidth = 1 / view.k;
    ctx.globalAlpha = 0.35;
    for (const l of links) {
      const s = l.source as Node;
      const d = l.target as Node;
      if (s.x == null || d.x == null || s.y == null || d.y == null) continue;
      ctx.beginPath();
      ctx.moveTo(s.x, s.y);
      ctx.lineTo(d.x, d.y);
      ctx.stroke();
    }

    ctx.globalAlpha = 1;
    for (const n of nodes) {
      if (n.x == null || n.y == null) continue;
      ctx.beginPath();
      ctx.arc(n.x, n.y, radius(n), 0, Math.PI * 2);
      ctx.fillStyle = n.id === centerId ? colCenter : colNode;
      ctx.fill();
      if (n === hovered) {
        ctx.strokeStyle = colInk;
        ctx.lineWidth = 2 / view.k;
        ctx.stroke();
      }
      if (centerId || view.k > 1.05 || n === hovered) {
        ctx.fillStyle = colInk;
        ctx.font = `${10.5 / view.k}px sans-serif`;
        const label = n.title.length > 16 ? `${n.title.slice(0, 16)}…` : n.title;
        ctx.fillText(label, n.x + radius(n) + 3 / view.k, n.y + 4 / view.k);
      }
    }
  }

  resize();
  const observer = new ResizeObserver(() => {
    resize();
    draw();
  });
  observer.observe(container);

  const sim = forceSimulation(nodes)
    .force(
      "link",
      forceLink<Node, Link>(links)
        .id((d) => d.id)
        .distance(70)
        .strength(0.5),
    )
    .force("charge", forceManyBody().strength(-130))
    .force("center", forceCenter(container.clientWidth / 2, container.clientHeight / 2))
    .force(
      "collide",
      forceCollide<Node>().radius((d) => radius(d) + 3),
    )
    .on("tick", draw);

  const toGraph = (mx: number, my: number) => ({
    x: (mx - view.x) / view.k,
    y: (my - view.y) / view.k,
  });

  const hit = (mx: number, my: number): Node | null => {
    const p = toGraph(mx, my);
    for (const n of nodes) {
      if (n.x == null || n.y == null) continue;
      const dx = p.x - n.x;
      const dy = p.y - n.y;
      if (dx * dx + dy * dy <= (radius(n) + 3) ** 2) return n;
    }
    return null;
  };

  const onWheel = (e: WheelEvent) => {
    e.preventDefault();
    const rect = canvas.getBoundingClientRect();
    const mx = e.clientX - rect.left;
    const my = e.clientY - rect.top;
    const k = Math.min(4, Math.max(0.2, view.k * (e.deltaY < 0 ? 1.12 : 0.89)));
    view.x = mx - ((mx - view.x) / view.k) * k;
    view.y = my - ((my - view.y) / view.k) * k;
    view.k = k;
    draw();
  };

  /** ドラッグ中だけ document に張る購読。destroy 時に必ず外せるよう控えておく。 */
  let releaseDrag: (() => void) | null = null;

  const onMouseDown = (e: MouseEvent) => {
    const rect = canvas.getBoundingClientRect();
    const start = { x: e.clientX - rect.left, y: e.clientY - rect.top, moved: false };
    const node = hit(start.x, start.y);

    const move = (ev: MouseEvent) => {
      const cx = ev.clientX - rect.left;
      const cy = ev.clientY - rect.top;
      if (Math.abs(cx - start.x) + Math.abs(cy - start.y) > 4) start.moved = true;
      if (node) {
        const p = toGraph(cx, cy);
        node.fx = p.x;
        node.fy = p.y;
        sim.alphaTarget(0.25).restart();
      } else {
        view.x += cx - start.x;
        view.y += cy - start.y;
        start.x = cx;
        start.y = cy;
        draw();
      }
    };

    const up = () => {
      releaseDrag?.();
      if (node) {
        node.fx = null;
        node.fy = null;
        sim.alphaTarget(0);
        // 動かしていなければクリック = 開く
        if (!start.moved) onOpen(node.id);
      }
    };

    releaseDrag = () => {
      document.removeEventListener("mousemove", move);
      document.removeEventListener("mouseup", up);
      releaseDrag = null;
    };
    document.addEventListener("mousemove", move);
    document.addEventListener("mouseup", up);
  };

  const onMouseMove = (e: MouseEvent) => {
    const rect = canvas.getBoundingClientRect();
    const h = hit(e.clientX - rect.left, e.clientY - rect.top);
    if (h !== hovered) {
      hovered = h;
      canvas.style.cursor = h ? "pointer" : "default";
      draw();
    }
  };

  canvas.addEventListener("wheel", onWheel, { passive: false });
  canvas.addEventListener("mousedown", onMouseDown);
  canvas.addEventListener("mousemove", onMouseMove);

  return {
    destroy() {
      sim.stop();
      observer.disconnect();
      releaseDrag?.();
      canvas.removeEventListener("wheel", onWheel);
      canvas.removeEventListener("mousedown", onMouseDown);
      canvas.removeEventListener("mousemove", onMouseMove);
    },
  };
}
