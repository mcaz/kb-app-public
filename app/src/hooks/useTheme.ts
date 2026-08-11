import { useEffect, useSyncExternalStore } from "react";

import { usePrefs } from "@/lib/stores/prefs";
import { setThemeAttribute, systemTheme, watchSystemTheme, type ResolvedTheme } from "@/lib/theme";

const LIGHT = (): ResolvedTheme => "light";

/**
 * 選択中のテーマを <html data-theme> へ反映し、実際に適用されている側を返す。
 * 「システムに従う」を選んでいる間は OS の切り替えにも追従する。
 *
 * OS 設定は React の外の状態なので useSyncExternalStore で購読する
 * (effect の中で setState して再描画を連鎖させない)。
 */
export function useTheme(): ResolvedTheme {
  const theme = usePrefs((s) => s.theme);
  const system = useSyncExternalStore(watchSystemTheme, systemTheme, LIGHT);
  const resolved = theme === "system" ? system : theme;

  useEffect(() => {
    setThemeAttribute(resolved);
  }, [resolved]);

  return resolved;
}
