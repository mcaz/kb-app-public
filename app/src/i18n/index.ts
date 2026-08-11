import i18n from "i18next";
import { initReactI18next } from "react-i18next";

import en from "./locales/en";
import ja from "./locales/ja";

export const SUPPORTED_LANGUAGES = ["ja", "en"] as const;
export type Language = (typeof SUPPORTED_LANGUAGES)[number];

export const isLanguage = (v: unknown): v is Language =>
  typeof v === "string" && (SUPPORTED_LANGUAGES as readonly string[]).includes(v);

/** OS/ブラウザのロケールから既定言語を決める。判別できなければ日本語。 */
export function detectLanguage(): Language {
  const tags = typeof navigator === "undefined" ? [] : [navigator.language, ...navigator.languages];
  for (const tag of tags) {
    const base = tag?.split("-")[0]?.toLowerCase();
    if (isLanguage(base)) return base;
  }
  return "ja";
}

export function setupI18n(language: Language) {
  if (i18n.isInitialized) return i18n;
  void i18n.use(initReactI18next).init({
    resources: { ja, en },
    lng: language,
    // 文言の正本は日本語。英語に未訳のキーが出たら日本語で見せる(空表示にしない)
    fallbackLng: "ja",
    defaultNS: "common",
    interpolation: { escapeValue: false },
  });
  return i18n;
}

export { i18n };
