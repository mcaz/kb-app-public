import { Brain, Palette, Plug, Settings } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/atoms/ui/dialog";
import { ConnectPage } from "@/pages/ConnectPage";
import { KnowledgeBaseSettingsPage } from "@/pages/KnowledgeBaseSettingsPage";
import { SettingsPage } from "@/pages/SettingsPage";

export interface SettingsDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

type SettingsSection = "kb" | "connect" | "theme";

/** KB利用・接続・端末設定を、アプリ本体の画面遷移を変えずにまとめて扱う。 */
export function SettingsDialog({ open, onOpenChange }: SettingsDialogProps) {
  const { t } = useTranslation("common");
  const [section, setSection] = useState<SettingsSection>("kb");
  const items = [
    { id: "kb" as const, icon: Brain, label: t("settings.kbUsage") },
    { id: "connect" as const, icon: Plug, label: t("nav.connect") },
    { id: "theme" as const, icon: Palette, label: t("settings.theme") },
  ];

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="h-[80vh] max-h-[760px] w-[calc(100%-1rem)] max-w-[1000px] grid-cols-[176px_minmax(0,1fr)] grid-rows-1 gap-0 overflow-hidden p-0 max-[639px]:grid-cols-1 max-[639px]:grid-rows-[auto_minmax(0,1fr)] sm:max-w-[1000px]">
        <DialogTitle className="sr-only">{t("settings.title")}</DialogTitle>
        <DialogDescription className="sr-only">{t("settings.dialogDescription")}</DialogDescription>

        <aside className="border-line bg-panel-2 flex min-h-0 flex-col border-r px-3 py-4 max-[639px]:flex-row max-[639px]:items-center max-[639px]:gap-2 max-[639px]:overflow-x-auto max-[639px]:border-r-0 max-[639px]:border-b max-[639px]:py-2">
          <div className="mb-4 flex items-center gap-2 px-2 font-bold max-[639px]:mr-2 max-[639px]:mb-0">
            <Icon as={Settings} />
            <span>{t("settings.title")}</span>
          </div>
          <nav className="flex flex-col gap-1 max-[639px]:flex-row">
            {items.map((item) => (
              <button
                key={item.id}
                type="button"
                aria-current={section === item.id ? "page" : undefined}
                onClick={() => setSection(item.id)}
                className={`flex cursor-pointer items-center gap-2 rounded-md border-none px-2.5 py-2 text-left text-[13px] whitespace-nowrap ${
                  section === item.id
                    ? "bg-sel text-ink font-semibold"
                    : "text-muted hover:text-ink bg-transparent"
                }`}
              >
                <Icon as={item.icon} />
                <span>{item.label}</span>
              </button>
            ))}
          </nav>
        </aside>

        <div className="flex min-h-0 min-w-0 overflow-hidden">
          {section === "kb" ? (
            <KnowledgeBaseSettingsPage />
          ) : section === "connect" ? (
            <ConnectPage />
          ) : (
            <SettingsPage />
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
