import { BookOpen, Brain, Palette, Plug, RefreshCw, Settings } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";
import { Button } from "@/components/atoms/ui/button";
import { GettingStarted } from "@/components/organisms/GettingStarted";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/atoms/ui/dialog";
import { ConnectPage } from "@/pages/ConnectPage";
import { DistillationSettingsPage } from "@/pages/DistillationSettingsPage";
import { KnowledgeBaseSettingsPage } from "@/pages/KnowledgeBaseSettingsPage";
import { SettingsPage } from "@/pages/SettingsPage";
import { settingsDialogVariants } from "./variants";

export interface SettingsDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  section: SettingsSection;
  onSectionChange: (section: SettingsSection) => void;
  onOpenSearch: () => void;
}

export type SettingsSection = "getting_started" | "kb" | "distillation" | "connect" | "general";

/** KB利用・接続・端末設定を、アプリ本体の画面遷移を変えずにまとめて扱う。 */
export function SettingsDialog({
  open,
  onOpenChange,
  section,
  onSectionChange,
  onOpenSearch,
}: SettingsDialogProps) {
  const { t } = useTranslation(["common", "gettingStarted"]);
  const styles = settingsDialogVariants();
  const items = [
    { id: "getting_started" as const, icon: BookOpen, label: t("gettingStarted:nav") },
    { id: "kb" as const, icon: Brain, label: t("settings.kbUsage") },
    { id: "distillation" as const, icon: RefreshCw, label: t("settings.distillation.title") },
    { id: "connect" as const, icon: Plug, label: t("nav.connect") },
    { id: "general" as const, icon: Palette, label: t("settings.general") },
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
                onClick={() => onSectionChange(item.id)}
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
          {section === "getting_started" ? (
            <GettingStarted
              onOpenConnections={() => onSectionChange("connect")}
              onOpenProtection={() => onSectionChange("kb")}
              onOpenSearch={onOpenSearch}
            />
          ) : section === "kb" ? (
            <KnowledgeBaseSettingsPage />
          ) : section === "distillation" ? (
            <DistillationSettingsPage />
          ) : section === "connect" ? (
            <div className={styles.connectionPane()}>
              <div className={styles.guideEntry()}>
                <p>{t("gettingStarted:afterConnection")}</p>
                <Button size="sm" onClick={() => onSectionChange("getting_started")}>
                  {t("gettingStarted:openGuide")}
                </Button>
              </div>
              <ConnectPage />
            </div>
          ) : (
            <SettingsPage />
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
