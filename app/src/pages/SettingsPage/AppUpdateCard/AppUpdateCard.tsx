import { useTranslation } from "react-i18next";

import { Button } from "@/components/atoms/ui/button";
import { formatSize } from "@/lib/format";
import { useAppUpdateAction, useAppUpdateStatus } from "@/lib/queries";
import {
  appUpdateActions,
  appUpdateProgress,
  isAppUpdateBusy,
} from "@/lib/queries/appUpdatePolicy";

import { appUpdateCardVariants } from "./variants";

export function AppUpdateCard() {
  const { t } = useTranslation("appUpdate");
  const styles = appUpdateCardVariants();
  const query = useAppUpdateStatus();
  const action = useAppUpdateAction();
  const status = query.data;
  const actions = appUpdateActions(status);
  const busy = action.isPending || isAppUpdateBusy(status?.phase);
  const disabled = busy || query.isFetching || query.isError;
  const progress = status ? appUpdateProgress(status) : null;

  return (
    <section className={styles.section()} aria-labelledby="app-update-heading">
      <h2 id="app-update-heading" className={styles.heading()}>
        {t("title")}
      </h2>
      {status && (
        <p className={styles.description()}>
          {t("currentVersion", { version: status.current_version })}
        </p>
      )}
      <p className={styles.status()} role="status">
        {query.isPending
          ? t("loading")
          : query.isError
            ? t("readFailed")
            : status
              ? status.failure && status.phase !== "unavailable" && !isAppUpdateBusy(status.phase)
                ? t("stopped")
                : t(`phases.${status.phase}`, { version: status.available_version })
              : t("readFailed")}
      </p>
      {status?.failure && (
        <p className={styles.failure()} role="alert">
          {t(`failures.${status.failure}`)}
        </p>
      )}
      {action.isError && (
        <p className={styles.failure()} role="alert">
          {t("actionFailed")}
        </p>
      )}
      {status?.phase === "downloading" && (
        <div className={styles.progressArea()}>
          <progress
            className={styles.progress()}
            max={100}
            value={progress ?? undefined}
            aria-label={t("downloadProgress")}
          />
          <p className={styles.description()}>
            {progress === null
              ? t("downloaded", { downloaded: formatSize(status.downloaded_bytes) })
              : t("downloadedOf", {
                  downloaded: formatSize(status.downloaded_bytes),
                  total: formatSize(status.total_bytes ?? 0),
                  percent: progress,
                })}
          </p>
        </div>
      )}
      {status?.phase === "ready" && status.available_version && (
        <p className={styles.description()}>
          {t("readyVersion", { version: status.available_version })}
        </p>
      )}
      <div className={styles.actions()}>
        {(query.isError || action.isError) && (
          <Button
            type="button"
            className={styles.action()}
            disabled={busy || query.isFetching}
            onClick={() => {
              action.reset();
              void query.refetch();
            }}
          >
            {t("reloadStatus")}
          </Button>
        )}
        {actions.check && (
          <Button
            type="button"
            className={styles.action()}
            disabled={disabled}
            onClick={() => action.mutate("check")}
          >
            {t("check")}
          </Button>
        )}
        {actions.download && (
          <Button
            type="button"
            variant="primary"
            className={styles.action()}
            disabled={disabled}
            onClick={() => action.mutate("download")}
          >
            {t("download")}
          </Button>
        )}
        {actions.install && (
          <Button
            type="button"
            variant="primary"
            className={styles.action()}
            disabled={disabled}
            aria-describedby="app-update-reconnect"
            onClick={() => action.mutate("install")}
          >
            {t("install")}
          </Button>
        )}
      </div>
      <p id="app-update-reconnect" className={styles.description()}>
        {t("reconnect")}
      </p>
    </section>
  );
}
