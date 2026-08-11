export const THEMES = ["system", "light", "dark"] as const;
export type Theme = (typeof THEMES)[number];
export type ResolvedTheme = "light" | "dark";

export const isTheme = (v: unknown): v is Theme =>
  typeof v === "string" && (THEMES as readonly string[]).includes(v);

const DARK_QUERY = "(prefers-color-scheme: dark)";

/** OS の設定。matchMedia が無い環境(テスト等)ではライト扱い。 */
export function systemTheme(): ResolvedTheme {
  return typeof window !== "undefined" && window.matchMedia(DARK_QUERY).matches ? "dark" : "light";
}

export function resolveTheme(theme: Theme): ResolvedTheme {
  return theme === "system" ? systemTheme() : theme;
}

/**
 * 解決した結果を <html data-theme> に貼る。CSS 側は必ずこの属性だけを見るので、
 * 「システムに従う」の判断は常にここに一本化される。
 */
export function setThemeAttribute(resolved: ResolvedTheme): void {
  document.documentElement.dataset.theme = resolved;
}

/** 起動時に一度だけ使う(描画前に配色を確定させる)。 */
export function applyTheme(theme: Theme): ResolvedTheme {
  const resolved = resolveTheme(theme);
  setThemeAttribute(resolved);
  return resolved;
}

/** OS 側の切り替えを購読する(theme が "system" のときだけ意味がある)。 */
export function watchSystemTheme(onChange: () => void): () => void {
  if (typeof window === "undefined") return () => undefined;
  const media = window.matchMedia(DARK_QUERY);
  media.addEventListener("change", onChange);
  return () => media.removeEventListener("change", onChange);
}
