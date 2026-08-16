import { useState } from "react";

import { CloudDownload, Sprout } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";

import { Button } from "@/components/atoms/ui/button";
import { OnboardingLayout } from "@/components/templates/OnboardingLayout";
import { useErrorText } from "@/hooks/useErrorText";
import { useOnboard, useOnboardExisting } from "@/lib/queries";

/** 最初の vault を作る(FR-A1)。 */
export function OnboardingPage() {
  const { t } = useTranslation("onboarding");
  const errorText = useErrorText();
  const onboard = useOnboard();
  const existing = useOnboardExisting();
  const [showExisting, setShowExisting] = useState(false);
  const [remoteUrl, setRemoteUrl] = useState("");
  const busy = onboard.isPending || existing.isPending;

  return (
    <OnboardingLayout
      mark={<Icon as={Sprout} className="size-10" />}
      title={t("title")}
      lead={t("lead")}
    >
      <div className="flex w-[min(420px,80vw)] flex-col gap-2">
        <Button variant="primary" disabled={busy} onClick={() => onboard.mutate()}>
          {t("createNew")}
        </Button>
        {!showExisting ? (
          <Button variant="default" disabled={busy} onClick={() => setShowExisting(true)}>
            <CloudDownload className="size-4" />
            {t("useExisting")}
          </Button>
        ) : (
          <div className="border-line bg-surface mt-2 rounded-lg border p-3 text-left">
            <label className="text-ink mb-1.5 block text-xs font-medium" htmlFor="vault-remote-url">
              {t("existingUrl")}
            </label>
            <input
              id="vault-remote-url"
              className="border-line bg-chip text-ink mb-2 w-full rounded-md border px-2.5 py-2 text-xs"
              placeholder="https://github.com/owner/vault.git"
              value={remoteUrl}
              onChange={(event) => setRemoteUrl(event.target.value)}
            />
            <Button
              variant="primary"
              size="sm"
              disabled={busy || remoteUrl.trim().length === 0}
              onClick={() => existing.mutate(remoteUrl.trim())}
            >
              {existing.isPending ? t("restoring") : t("restore")}
            </Button>
            <p className="text-muted mt-2 mb-0 text-[11px]">{t("existingHelp")}</p>
          </div>
        )}
        {(onboard.error || existing.error) && (
          <p className="text-danger mt-2 text-xs">{errorText(onboard.error ?? existing.error)}</p>
        )}
      </div>
    </OnboardingLayout>
  );
}
