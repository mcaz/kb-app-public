import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Switch } from "@/components/atoms/ui/switch";
import { SinglePaneLayout } from "@/components/templates/SinglePaneLayout";
import { useErrorText } from "@/hooks/useErrorText";
import { useSetAiKbEnabled, useSettings } from "@/lib/queries";

export function KnowledgeBaseSettingsPage() {
  const { t } = useTranslation("common");
  const { data: settings, isPending, error } = useSettings();
  const setAiKbEnabled = useSetAiKbEnabled();
  const errorText = useErrorText();
  const enabled = settings?.ai_kb_enabled ?? false;
  const descriptionId = "setting-ai-kb-description";

  return (
    <SinglePaneLayout>
      <div className="max-w-[46em] px-6 py-5">
        <h1 className="mb-4 text-lg font-bold">{t("settings.kbUsage")}</h1>
        <h2 className="text-muted mb-2 text-xs tracking-[0.08em]">{t("settings.aiUsage")}</h2>

        <div className="border-line bg-panel flex items-center justify-between gap-5 rounded-xl border px-4 py-3.5">
          <div className="min-w-0">
            <label htmlFor="setting-ai-kb" className="text-[13px] font-semibold">
              {t("settings.aiKbEnabled")}
            </label>
            <p id={descriptionId} className="text-muted mt-1 text-xs leading-relaxed">
              {enabled ? t("settings.aiKbOnDescription") : t("settings.aiKbOffDescription")}
            </p>
          </div>
          <Switch
            id="setting-ai-kb"
            checked={enabled}
            disabled={isPending || setAiKbEnabled.isPending}
            aria-describedby={descriptionId}
            onCheckedChange={(checked) => {
              setAiKbEnabled.mutate(checked, {
                onSuccess: () => toast(t(checked ? "settings.aiKbOnDone" : "settings.aiKbOffDone")),
                onError: (mutationError) => toast(errorText(mutationError)),
              });
            }}
          />
        </div>

        {error && (
          <p className="text-danger mt-3 text-xs" role="alert">
            {errorText(error)}
          </p>
        )}
        <p className="text-muted mt-3 text-xs leading-relaxed">{t("settings.aiKbRestartNote")}</p>
        <p className="text-muted mt-2 text-xs">{t("settings.note")}</p>
      </div>
    </SinglePaneLayout>
  );
}
