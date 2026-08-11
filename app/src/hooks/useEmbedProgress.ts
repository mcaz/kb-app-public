import { useEffect, useState } from "react";

import { IN_TAURI } from "@/lib/api";
import { events, type EmbedProgress } from "@/lib/bindings";

/**
 * かしこい検索の進み具合。
 *
 * 数分かかる処理なので、コア側が1バッチごとに投げてくるイベントを受けて
 * 「準備中…」だけの表示から進捗が見える表示にする。
 */
export function useEmbedProgress(): EmbedProgress | null {
  const [progress, setProgress] = useState<EmbedProgress | null>(null);

  useEffect(() => {
    if (!IN_TAURI) return;
    let dispose: (() => void) | undefined;
    let cancelled = false;

    void events.embedProgress
      .listen((e) => setProgress(e.payload))
      .then((unlisten) => {
        if (cancelled) unlisten();
        else dispose = unlisten;
      });

    return () => {
      cancelled = true;
      dispose?.();
    };
  }, []);

  return progress;
}
