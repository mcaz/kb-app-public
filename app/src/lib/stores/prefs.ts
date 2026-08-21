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
  /** 関連パネル幅(px)。 */
  relWidth: number;
}

interface PrefsStore extends Prefs {
  set: (patch: Partial<Prefs>) => void;
}

export const usePrefs = create<PrefsStore>()(
  persist(
    (set) => ({
      language: detectLanguage(),
      theme: "dark",
      sideCollapsed: false,
      relWidth: 220,
      set: (patch) => set(patch),
    }),
    {
      name: "kb.prefs",
      version: 1,
    },
  ),
);
