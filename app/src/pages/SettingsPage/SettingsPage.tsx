import { useTranslation } from "react-i18next";

import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/atoms/ui/select";
import { SinglePaneLayout } from "@/components/templates/SinglePaneLayout";
import { SUPPORTED_LANGUAGES, isLanguage } from "@/i18n";
import { usePrefs } from "@/lib/stores/prefs";
import { isTheme, THEMES } from "@/lib/theme";

import { SettingRow } from "./SettingRow";

/**
 * 端末ごとの設定。ここに置くのは「この端末での見え方」だけで、
 * vault の中身に影響するものは置かない(ノートは AI の領分 — 原則)。
 */
export function SettingsPage() {
  const { t, i18n } = useTranslation();
  const theme = usePrefs((s) => s.theme);
  const language = usePrefs((s) => s.language);
  const setPrefs = usePrefs((s) => s.set);

  const themeLabel = {
    system: t("settings.themeSystem"),
    light: t("settings.themeLight"),
    dark: t("settings.themeDark"),
  };
  const languageLabel = { ja: t("settings.languageJa"), en: t("settings.languageEn") };

  return (
    <SinglePaneLayout>
      <div className="max-w-[46em] px-6 py-5">
        <h1 className="mb-4 text-lg font-bold">{t("settings.title")}</h1>
        <h2 className="text-muted mb-2 text-xs tracking-[0.08em]">{t("settings.appearance")}</h2>

        <div className="border-line bg-panel flex flex-col gap-3 rounded-xl border px-4 py-3.5">
          <SettingRow id="setting-theme" label={t("settings.theme")}>
            <Select
              value={theme}
              onValueChange={(v) => {
                if (isTheme(v)) setPrefs({ theme: v });
              }}
            >
              <SelectTrigger id="setting-theme" size="sm" className="w-[200px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {THEMES.map((value) => (
                  <SelectItem key={value} value={value}>
                    {themeLabel[value]}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </SettingRow>

          <SettingRow id="setting-language" label={t("settings.language")}>
            <Select
              value={language}
              onValueChange={(v) => {
                if (!isLanguage(v)) return;
                setPrefs({ language: v });
                void i18n.changeLanguage(v);
              }}
            >
              <SelectTrigger id="setting-language" size="sm" className="w-[200px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {SUPPORTED_LANGUAGES.map((value) => (
                  <SelectItem key={value} value={value}>
                    {languageLabel[value]}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </SettingRow>
        </div>

        <p className="text-muted mt-3 text-xs">{t("settings.note")}</p>
      </div>
    </SinglePaneLayout>
  );
}
