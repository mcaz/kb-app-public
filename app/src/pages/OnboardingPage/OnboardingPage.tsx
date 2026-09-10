import { useState } from "react";

import { CloudDownload, Sprout } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";

import { Button } from "@/components/atoms/ui/button";
import { GitHubAuthPanel } from "@/components/organisms/GitHubAuthPanel";
import { OnboardingLayout } from "@/components/templates/OnboardingLayout";
import { useErrorText } from "@/hooks/useErrorText";
import { useVaultRestoreProgress } from "@/hooks/useVaultRestoreProgress";
import { useGitHubAuthState, useOnboard, useOnboardExisting } from "@/lib/queries";

/** 最初の vault を作る(FR-A1)。 */
export function OnboardingPage({ onReady }: { onReady: () => void }) {
  const { t } = useTranslation("onboarding");
  const errorText = useErrorText();
  const onboard = useOnboard(onReady);
  const existing = useOnboardExisting(onReady);
  const [showExisting, setShowExisting] = useState(false);
  const { data: githubAuth } = useGitHubAuthState(showExisting);
  const restoreProgress = useVaultRestoreProgress();
  const [remoteUrl, setRemoteUrl] = useState("");
  const busy = onboard.isPending || existing.isPending;
  const restoreStatus =
    existing.isPending && restoreProgress
      ? (() => {
          switch (restoreProgress.phase) {
            case "checking":
              return t("progress.checking");
            case "cloning":
              return t("progress.cloning");
            case "restoring_files":
              return t("progress.files", restoreProgress);
            case "finalizing":
              return t("progress.finalizing");
          }
        })()
      : null;

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
            <GitHubAuthPanel heading />
            {githubAuth?.signed_in && (
              <div className="mt-3">
                <label
                  className="text-ink mb-1.5 block text-xs font-medium"
                  htmlFor="vault-remote-url"
                >
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
                  {existing.isPending
                    ? t("restoring")
                    : existing.isError
                      ? t("resume")
                      : t("restore")}
                </Button>
                {restoreStatus && (
                  <p className="text-muted mt-2 mb-0 text-[11px]" aria-live="polite">
                    {restoreStatus}
                  </p>
                )}
                {existing.isError && (
                  <p className="text-muted mt-2 mb-0 text-[11px]">{t("resumeHelp")}</p>
                )}
                <p className="text-muted mt-2 mb-0 text-[11px]">{t("existingHelp")}</p>
              </div>
            )}
          </div>
        )}
        {(onboard.error || existing.error) && (
          <p className="text-danger mt-2 text-xs">{errorText(onboard.error ?? existing.error)}</p>
        )}
      </div>
    </OnboardingLayout>
  );
}
