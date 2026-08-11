import { useEffect, useRef } from "react";

import { useTheme } from "@/hooks/useTheme";
import type { GraphData } from "@/lib/api";
import { createForceGraph } from "@/lib/graph/force-graph";

export interface GraphCanvasProps {
  data: GraphData;
  centerId: string | null;
  hint: string;
  onOpen: (id: string) => void;
}

/**
 * つながりグラフ。描画そのものは lib/graph(React 非依存)に置き、
 * ここは生成と後片付けだけを担う。
 */
export function GraphCanvas({ data, centerId, hint, onOpen }: GraphCanvasProps) {
  const wrap = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  // canvas は生成時に CSS 変数を読んで色を決めるため、テーマが変わったら作り直す
  const theme = useTheme();
  // 再生成の条件に onOpen を混ぜたくないので、最新の関数だけ差し替える
  const open = useRef(onOpen);
  useEffect(() => {
    open.current = onOpen;
  }, [onOpen]);

  useEffect(() => {
    if (!wrap.current || !canvas.current) return;
    const graph = createForceGraph({
      container: wrap.current,
      canvas: canvas.current,
      data,
      centerId,
      onOpen: (id) => open.current(id),
    });
    return () => graph.destroy();
  }, [data, centerId, theme]);

  return (
    <div ref={wrap} className="relative min-w-0 flex-1 overflow-hidden">
      <canvas ref={canvas} className="absolute inset-0 h-full w-full" />
      <div className="text-muted pointer-events-none absolute bottom-3 left-3.5 text-xs opacity-80">
        {hint}
      </div>
    </div>
  );
}
