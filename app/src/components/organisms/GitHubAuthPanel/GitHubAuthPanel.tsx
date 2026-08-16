import { GitFork } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/atoms/ui/button";
import { useGitHubDeviceAuthorization } from "@/hooks/useGitHubDeviceAuthorization";
import { useErrorText } from "@/hooks/useErrorText";
import {
  useGitHubAuthState,
  useGitHubOpenDevicePage,
  useGitHubSignIn,
  useGitHubSignOut,
} from "@/lib/queries";

export interface GitHubAuthPanelProps {
  heading?: boolean;
}

/** 通常の接続画面と、2台目の初回復元で共用する GitHub 認証導線。 */
export function GitHubAuthPanel({ heading = false }: GitHubAuthPanelProps) {
  const { t } = useTranslation("github");
  const errorText = useErrorText();
  const { data: auth, error: authError, isPending: checking } = useGitHubAuthState();
  const signIn = useGitHubSignIn();
  const signOut = useGitHubSignOut();
  const openPage = useGitHubOpenDevicePage();
  const { authorization, clear } = useGitHubDeviceAuthorization();

  return (
    <div className="border-line bg-surface rounded-lg border p-3 text-left">
      {heading && (
        <div className="text-ink mb-1.5 flex items-center gap-1.5 text-xs font-bold">
          <GitFork className="size-4" />
          {t("title")}
        </div>
      )}
      {authError ? (
        <p className="text-danger m-0 text-[11px]">{errorText(authError)}</p>
      ) : checking || !auth ? (
        <p className="text-muted m-0 text-[11px]">{t("checking")}</p>
      ) : !auth.configured ? (
        <div>
          <p className="text-danger m-0 text-xs font-medium">{t("notConfigured")}</p>
          <p className="text-muted mt-1 mb-0 text-[11px]">{t("notConfiguredHelp")}</p>
        </div>
      ) : auth.signed_in ? (
        <div>
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div>
              <p className="text-ink m-0 text-xs font-medium">
                {t("signedIn", { account: auth.account_login })}
              </p>
              <p className="text-muted mt-1 mb-0 text-[11px]">{t("storedInKeychain")}</p>
            </div>
            <Button
              variant="quiet"
              size="sm"
              disabled={signOut.isPending}
              onClick={() => signOut.mutate()}
            >
              {t("signOut")}
            </Button>
          </div>
          {signOut.error && (
            <p className="text-danger mt-2 mb-0 text-[11px]">{errorText(signOut.error)}</p>
          )}
        </div>
      ) : (
        <div>
          <p className="text-muted mt-0 mb-2 text-[11px]">{t("scopeDisclosure")}</p>
          {!authorization ? (
            <Button
              variant="primary"
              size="sm"
              disabled={signIn.isPending}
              onClick={() => {
                clear();
                signIn.mutate();
              }}
            >
              <GitFork className="size-4" />
              {signIn.isPending ? t("starting") : t("signIn")}
            </Button>
          ) : (
            <div aria-live="polite">
              <p className="text-muted mt-0 mb-1 text-[11px]">{t("enterCode")}</p>
              <code className="border-line bg-chip text-ink mb-2 block w-fit rounded border px-2 py-1 text-sm font-bold tracking-[0.16em]">
                {authorization.user_code}
              </code>
              <Button
                variant="primary"
                size="sm"
                disabled={openPage.isPending}
                onClick={() => openPage.mutate()}
              >
                {t("openGitHub")}
              </Button>
              {signIn.isError && (
                <Button
                  className="ml-1"
                  variant="default"
                  size="sm"
                  onClick={() => {
                    clear();
                    signIn.reset();
                  }}
                >
                  {t("retry")}
                </Button>
              )}
              <p className="text-muted mt-2 mb-0 text-[11px]">{t("waiting")}</p>
            </div>
          )}
          {(signIn.error || openPage.error) && (
            <p className="text-danger mt-2 mb-0 text-[11px]">
              {errorText(signIn.error ?? openPage.error)}
            </p>
          )}
        </div>
      )}
    </div>
  );
}
