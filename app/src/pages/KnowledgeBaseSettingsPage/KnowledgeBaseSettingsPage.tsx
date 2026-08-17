import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Switch } from "@/components/atoms/ui/switch";
import { SinglePaneLayout } from "@/components/templates/SinglePaneLayout";
import { useErrorText } from "@/hooks/useErrorText";
import {
  useSetAiKbEnabled,
  useSetClaudeKbEnabled,
  useSetGptKbEnabled,
  useSettings,
} from "@/lib/queries";

interface SettingSwitchRowProps {
  id: string;
  label: string;
  description: string;
  checked: boolean;
  disabled: boolean;
  onCheckedChange: (checked: boolean) => void;
}

function SettingSwitchRow({
  id,
  label,
  description,
  checked,
  disabled,
  onCheckedChange,
}: SettingSwitchRowProps) {
  const descriptionId = `${id}-description`;
  return (
    <div className="flex items-center justify-between gap-5 px-4 py-3.5">
      <div className="min-w-0">
        <label htmlFor={id} className="text-[13px] font-semibold">
          {label}
        </label>
        <p id={descriptionId} className="text-muted mt-1 text-xs leading-relaxed">
          {description}
        </p>
      </div>
      <Switch
        id={id}
        checked={checked}
        disabled={disabled}
        aria-describedby={descriptionId}
        onCheckedChange={onCheckedChange}
      />
    </div>
  );
}

export function KnowledgeBaseSettingsPage() {
  const { t } = useTranslation("common");
  const { data: settings, isPending, error } = useSettings();
  const setAiKbEnabled = useSetAiKbEnabled();
  const setClaudeKbEnabled = useSetClaudeKbEnabled();
  const setGptKbEnabled = useSetGptKbEnabled();
  const errorText = useErrorText();
  const enabled = settings?.ai_kb_enabled ?? false;
  const claudeEnabled = settings?.claude_kb_enabled ?? false;
  const gptEnabled = settings?.gpt_kb_enabled ?? false;
  const isSaving =
    setAiKbEnabled.isPending || setClaudeKbEnabled.isPending || setGptKbEnabled.isPending;

  return (
    <SinglePaneLayout>
      <div className="max-w-[46em] px-6 py-5">
        <h1 className="mb-4 text-lg font-bold">{t("settings.kbUsage")}</h1>
        <h2 className="text-muted mb-2 text-xs tracking-[0.08em]">{t("settings.aiUsage")}</h2>

        <div className="border-line bg-panel overflow-hidden rounded-xl border">
          <SettingSwitchRow
            id="setting-ai-kb"
            label={t("settings.aiKbEnabled")}
            description={
              enabled ? t("settings.aiKbOnDescription") : t("settings.aiKbOffDescription")
            }
            checked={enabled}
            disabled={isPending || isSaving}
            onCheckedChange={(checked) => {
              setAiKbEnabled.mutate(checked, {
                onSuccess: () => toast(t(checked ? "settings.aiKbOnDone" : "settings.aiKbOffDone")),
                onError: (mutationError) => toast(errorText(mutationError)),
              });
            }}
          />
        </div>

        <h2 className="text-muted mt-6 mb-2 text-xs tracking-[0.08em]">{t("settings.perApp")}</h2>
        <p className="text-muted mb-2 text-xs leading-relaxed">{t("settings.perAppDescription")}</p>
        <div className="border-line bg-panel divide-line divide-y overflow-hidden rounded-xl border">
          <SettingSwitchRow
            id="setting-claude-kb"
            label={t("settings.claudeKbEnabled")}
            description={
              !enabled
                ? t("settings.clientKbPausedDescription")
                : claudeEnabled
                  ? t("settings.claudeKbOnDescription")
                  : t("settings.claudeKbOffDescription")
            }
            checked={claudeEnabled}
            disabled={isPending || isSaving || !enabled}
            onCheckedChange={(checked) => {
              setClaudeKbEnabled.mutate(checked, {
                onSuccess: () =>
                  toast(t(checked ? "settings.claudeKbOnDone" : "settings.claudeKbOffDone")),
                onError: (mutationError) => toast(errorText(mutationError)),
              });
            }}
          />
          <SettingSwitchRow
            id="setting-gpt-kb"
            label={t("settings.gptKbEnabled")}
            description={
              !enabled
                ? t("settings.clientKbPausedDescription")
                : gptEnabled
                  ? t("settings.gptKbOnDescription")
                  : t("settings.gptKbOffDescription")
            }
            checked={gptEnabled}
            disabled={isPending || isSaving || !enabled}
            onCheckedChange={(checked) => {
              setGptKbEnabled.mutate(checked, {
                onSuccess: () =>
                  toast(t(checked ? "settings.gptKbOnDone" : "settings.gptKbOffDone")),
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
