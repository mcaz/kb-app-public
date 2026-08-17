import { ShieldAlert, ShieldCheck } from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Icon } from "@/components/atoms/Icon";
import { Button } from "@/components/atoms/ui/button";
import { Switch } from "@/components/atoms/ui/switch";
import { SinglePaneLayout } from "@/components/templates/SinglePaneLayout";
import { useErrorText } from "@/hooks/useErrorText";
import {
  useAiGuardStatus,
  useInstallAiGuard,
  useSetAiKbEnabled,
  useSetClaudeKbEnabled,
  useSetGptKbEnabled,
  useSettings,
} from "@/lib/queries";

import type { GuardTargetState } from "@/lib/bindings";

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

function guardStatusKey(state: GuardTargetState) {
  switch (state) {
    case "enforced":
      return "settings.aiGuardStatusEnforced";
    case "missing":
      return "settings.aiGuardStatusMissing";
    case "outdated":
      return "settings.aiGuardStatusOutdated";
    case "conflict":
      return "settings.aiGuardStatusConflict";
    case "unsupported":
      return "settings.aiGuardStatusUnsupported";
  }
}

export function KnowledgeBaseSettingsPage() {
  const { t } = useTranslation("common");
  const { data: settings, isPending, error } = useSettings();
  const { data: guard, isPending: isGuardPending, error: guardError } = useAiGuardStatus();
  const installGuard = useInstallAiGuard();
  const setAiKbEnabled = useSetAiKbEnabled();
  const setClaudeKbEnabled = useSetClaudeKbEnabled();
  const setGptKbEnabled = useSetGptKbEnabled();
  const errorText = useErrorText();
  const enabled = settings?.ai_kb_enabled ?? false;
  const claudeEnabled = settings?.claude_kb_enabled ?? false;
  const gptEnabled = settings?.gpt_kb_enabled ?? false;
  const isSaving =
    setAiKbEnabled.isPending || setClaudeKbEnabled.isPending || setGptKbEnabled.isPending;
  const guardConflict = guard?.codex === "conflict" || guard?.claude === "conflict";
  const guardUnsupported = guard?.codex === "unsupported" || guard?.claude === "unsupported";
  const guardReady = guard?.ready ?? false;
  const switchesDisabled = isPending || isGuardPending || isSaving || !guardReady;

  return (
    <SinglePaneLayout>
      <div className="max-w-[46em] px-6 py-5">
        <h1 className="mb-4 text-lg font-bold">{t("settings.kbUsage")}</h1>

        <div
          className={`mb-5 rounded-xl border p-4 ${
            guardReady ? "border-grow bg-grow-soft" : "border-line bg-panel"
          }`}
        >
          <div className="flex items-start gap-3">
            <Icon
              as={guardReady ? ShieldCheck : ShieldAlert}
              className={guardReady ? "text-grow mt-0.5" : "text-muted mt-0.5"}
            />
            <div className="min-w-0 flex-1">
              <h2 className="text-[13px] font-semibold">{t("settings.aiGuardTitle")}</h2>
              <p className="text-muted mt-1 text-xs leading-relaxed">
                {guardReady
                  ? t("settings.aiGuardReady")
                  : guardConflict
                    ? t("settings.aiGuardConflict")
                    : t("settings.aiGuardNeeded")}
              </p>
              {guard && (
                <div className="text-muted mt-2 flex flex-wrap gap-x-4 gap-y-1 text-[11px]">
                  <span>
                    {t("settings.aiGuardClientStatus", {
                      client: "Codex",
                      status: t(guardStatusKey(guard.codex)),
                    })}
                  </span>
                  <span>
                    {t("settings.aiGuardClientStatus", {
                      client: "Claude Code",
                      status: t(guardStatusKey(guard.claude)),
                    })}
                  </span>
                </div>
              )}
              {!guardReady && !guardConflict && !guardUnsupported && (
                <Button
                  type="button"
                  variant="primary"
                  className="mt-3"
                  disabled={isGuardPending || installGuard.isPending}
                  onClick={() => {
                    installGuard.mutate(undefined, {
                      onSuccess: () => toast(t("settings.aiGuardInstalled")),
                      onError: (installError) => toast(errorText(installError)),
                    });
                  }}
                >
                  {t(
                    installGuard.isPending
                      ? "settings.aiGuardInstalling"
                      : "settings.aiGuardInstall",
                  )}
                </Button>
              )}
            </div>
          </div>
        </div>

        <h2 className="text-muted mb-2 text-xs tracking-[0.08em]">{t("settings.aiUsage")}</h2>

        <div className="border-line bg-panel overflow-hidden rounded-xl border">
          <SettingSwitchRow
            id="setting-ai-kb"
            label={t("settings.aiKbEnabled")}
            description={
              enabled ? t("settings.aiKbOnDescription") : t("settings.aiKbOffDescription")
            }
            checked={enabled}
            disabled={switchesDisabled}
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
            disabled={switchesDisabled || !enabled}
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
            disabled={switchesDisabled || !enabled}
            onCheckedChange={(checked) => {
              setGptKbEnabled.mutate(checked, {
                onSuccess: () =>
                  toast(t(checked ? "settings.gptKbOnDone" : "settings.gptKbOffDone")),
                onError: (mutationError) => toast(errorText(mutationError)),
              });
            }}
          />
        </div>

        {(error || guardError) && (
          <p className="text-danger mt-3 text-xs" role="alert">
            {errorText(error ?? guardError)}
          </p>
        )}
        <p className="text-muted mt-3 text-xs leading-relaxed">{t("settings.aiKbRestartNote")}</p>
        <p className="text-muted mt-2 text-xs">{t("settings.note")}</p>
      </div>
    </SinglePaneLayout>
  );
}
