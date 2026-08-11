import { create } from "zustand";
import { persist } from "zustand/middleware";

import { detectLanguage, type Language } from "@/i18n";
import type { Theme } from "@/lib/theme";

/**
 * 端末に残す設定。旧実装では localStorage のキーが6箇所に直書きされていたので、
 * ここ1箇所に集約する(ADR-0002)。
 */
export interface Prefs {
  language: Language;
  /** "system" = OS の設定に従う。 */
  theme: Theme;
  sideCollapsed: boolean;
  filterOpen: boolean;
  pageSize: number;
  /** パネル幅(px)。null = 既定のまま(未調整)。 */
  listWidth: number;
  relWidth: number;
  mainWidth: number | null;
}

interface PrefsStore extends Prefs {
  set: (patch: Partial<Prefs>) => void;
}

export const PAGE_SIZES = [30, 50, 100, 200] as const;

export const usePrefs = create<PrefsStore>()(
  persist(
    (set) => ({
      language: detectLanguage(),
      theme: "system",
      sideCollapsed: false,
      filterOpen: false,
      pageSize: 30,
      listWidth: 260,
      relWidth: 220,
      mainWidth: null,
      set: (patch) => set(patch),
    }),
    {
      name: "kb.prefs",
      version: 1,
    },
  ),
);
