import { useEffect, useState } from "react";

import { localDayBoundaries } from "@/lib/time/dayBoundaries";

/** 開いたままの日付変更と、スリープ中の時刻・タイムゾーン変更を取得キーへ反映する。 */
export function useLocalDayBoundaries() {
  const [boundaries, setBoundaries] = useState(localDayBoundaries);

  useEffect(() => {
    let timer: ReturnType<typeof setTimeout>;
    const refresh = () => {
      const next = localDayBoundaries();
      setBoundaries((current) =>
        current.every((value, index) => value === next[index]) ? current : next,
      );
      clearTimeout(timer);
      timer = setTimeout(refresh, Math.max(1, next[14]! - Date.now()));
    };
    refresh();
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refresh);
    return () => {
      clearTimeout(timer);
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", refresh);
    };
  }, []);

  return boundaries;
}
