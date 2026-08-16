import { Cloud, MessageSquare, Sparkles } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Button } from "@/components/atoms/ui/button";
import { ConnectCard } from "@/components/molecules/ConnectCard";
import { SinglePaneLayout } from "@/components/templates/SinglePaneLayout";
import { useEmbedProgress } from "@/hooks/useEmbedProgress";
import { useBackupErrorText, useErrorText } from "@/hooks/useErrorText";
import {
  useBackupNow,
  useBackupCreateRepository,
  useBackupSetRemote,
  useConnectDesktop,
  useConnectState,
  useEmbedEnable,
} from "@/lib/queries";

/** 「繋ぐ」画面(AI アプリ・かしこい検索・バックアップ)。 */
export function ConnectPage() {
  const { t } = useTranslation(["connect", "common"]);
  const { data: state, isPending } = useConnectState();
  const errorText = useErrorText();
  const backupErrorText = useBackupErrorText();
  const embedProgress = useEmbedProgress();
  const connectDesktop = useConnectDesktop();
  const embedEnable = useEmbedEnable();
  const backupSetRemote = useBackupSetRemote();
  const backupCreateRepository = useBackupCreateRepository();
  const backupNow = useBackupNow();
  const [remoteUrl, setRemoteUrl] = useState("");
  const [repositoryName, setRepositoryName] = useState("kb-vault");
  const [backupMode, setBackupMode] = useState<"create" | "existing">("create");

  if (isPending || !state) {
    return (
      <SinglePaneLayout>
        <h1 className="px-5 pt-4 text-lg font-bold">{t("common:nav.connect")}</h1>
        <p className="text-muted px-5 py-4">{t("common:state.checking")}</p>
      </SinglePaneLayout>
    );
  }

  const search = state.smart_search;

  return (
    <SinglePaneLayout>
      <h1 className="px-5 pt-4 text-lg font-bold">{t("common:nav.connect")}</h1>
      <div className="grid grid-cols-[repeat(auto-fit,minmax(230px,1fr))] content-start gap-3 px-5 py-4">
        <ConnectCard
          name={t("ai.name")}
          icon={MessageSquare}
          description={t("ai.desc")}
          state={{
            ok: state.desktop === "connected",
            label:
              state.desktop === "connected"
                ? t("ai.connected")
                : state.desktop === "not_found"
                  ? t("ai.notFound")
                  : t("ai.notConnected"),
          }}
        >
          {state.desktop === "not_connected" && (
            <Button
              variant="primary"
              size="sm"
              disabled={connectDesktop.isPending}
              onClick={() =>
                connectDesktop.mutate(undefined, {
                  onSuccess: () => toast(t("ai.done")),
                  onError: (e) => toast(t("ai.failed", { error: errorText(e) })),
                })
              }
            >
              {t("ai.connect")}
            </Button>
          )}
        </ConnectCard>

        <ConnectCard
          name={t("smartSearch.name")}
          icon={Sparkles}
          description={t("smartSearch.desc")}
          state={{
            ok: search.state === "enabled",
            label:
              search.state === "enabled"
                ? t("smartSearch.enabled", { embedded: search.embedded, total: search.total })
                : search.state === "downloading"
                  ? t("smartSearch.downloading")
                  : embedProgress
                    ? t("smartSearch.progress", embedProgress)
                    : t("smartSearch.off"),
          }}
        >
          {search.state === "not_installed" && (
            <Button
              variant="primary"
              size="sm"
              disabled={embedEnable.isPending}
              onClick={() => {
                toast(t("smartSearch.preparing"));
                embedEnable.mutate(undefined, {
                  onSuccess: () => toast(t("smartSearch.done")),
                  onError: (e) => toast(errorText(e)),
                });
              }}
            >
              {t("smartSearch.enable")}
            </Button>
          )}
        </ConnectCard>

        <ConnectCard
          name={t("backup.name")}
          icon={Cloud}
          description={t("backup.desc")}
          state={{
            ok: Boolean(state.backup.remote),
            label: state.backup.remote ? t("backup.connected") : t("backup.unset"),
          }}
          notes={
            <>
              {state.backup.remote && state.backup.pending > 0 && (
                <div className="text-muted mb-3 text-[12.5px]">
                  {t("backup.pending", { count: state.backup.pending })}
                </div>
              )}
              {state.sync_error && (
                <div className="text-danger mb-3 text-[12.5px]">
                  {t("backup.error", {
                    error: backupErrorText(state.sync_error_kind, state.sync_error),
                  })}
                </div>
              )}
            </>
          }
        >
          {state.backup.remote ? (
            <Button
              variant="primary"
              size="sm"
              disabled={backupNow.isPending}
              onClick={() =>
                backupNow.mutate(undefined, {
                  onSuccess: (message) => toast(message),
                  onError: (e) => toast(errorText(e)),
                })
              }
            >
              {t("backup.syncNow")}
            </Button>
          ) : (
            <>
              <div className="mb-2 flex gap-1">
                <Button
                  variant={backupMode === "create" ? "primary" : "default"}
                  size="sm"
                  onClick={() => setBackupMode("create")}
                >
                  {t("backup.createMode")}
                </Button>
                <Button
                  variant={backupMode === "existing" ? "primary" : "default"}
                  size="sm"
                  onClick={() => setBackupMode("existing")}
                >
                  {t("backup.existingMode")}
                </Button>
              </div>
              {backupMode === "create" ? (
                <input
                  className="border-line bg-chip text-ink mb-2 w-full rounded-md border px-2.5 py-1.5 text-xs"
                  placeholder={t("backup.namePlaceholder")}
                  aria-label={t("backup.namePlaceholder")}
                  value={repositoryName}
                  onChange={(e) => setRepositoryName(e.target.value)}
                />
              ) : (
                <input
                  className="border-line bg-chip text-ink mb-2 w-full rounded-md border px-2.5 py-1.5 text-xs"
                  placeholder={t("backup.urlPlaceholder")}
                  aria-label={t("backup.urlPlaceholder")}
                  value={remoteUrl}
                  onChange={(e) => setRemoteUrl(e.target.value)}
                />
              )}
              <Button
                variant="primary"
                size="sm"
                disabled={backupSetRemote.isPending || backupCreateRepository.isPending}
                onClick={() => {
                  if (backupMode === "create") {
                    const name = repositoryName.trim();
                    if (!name) {
                      toast(t("backup.needName"));
                      return;
                    }
                    backupCreateRepository.mutate(name, {
                      onSuccess: () => toast(t("backup.done")),
                      onError: (e) => toast(errorText(e)),
                    });
                    return;
                  }
                  const url = remoteUrl.trim();
                  if (!url) {
                    toast(t("backup.needUrl"));
                    return;
                  }
                  backupSetRemote.mutate(url, {
                    onSuccess: () => toast(t("backup.done")),
                    onError: (e) => toast(errorText(e)),
                  });
                }}
              >
                {backupMode === "create" ? t("backup.create") : t("backup.connect")}
              </Button>
            </>
          )}
        </ConnectCard>
      </div>
    </SinglePaneLayout>
  );
}
