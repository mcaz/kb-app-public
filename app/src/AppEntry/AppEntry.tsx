import { useTranslation } from "react-i18next";

import { App } from "@/App";
import { useAppBootMode } from "@/lib/queries";
import { StorageRecoveryPage } from "@/pages/StorageRecoveryPage";

import { appEntryVariants } from "./variants";

/** 通常Appのmount自体がDB取得・保守の入口なので、判定後にだけmountする。 */
export function AppEntry() {
  const { t } = useTranslation("recovery");
  const boot = useAppBootMode();
  const styles = appEntryVariants();
  if (boot.isSuccess && boot.data === "storage_recovery") return <StorageRecoveryPage />;
  if (boot.isSuccess && boot.data === "normal") return <App />;
  return (
    <main className={styles.root()} aria-busy={boot.isPending}>
      <div className={styles.message()} role={boot.isPending ? "status" : "alert"}>
        <h1 className={styles.title()}>{t(boot.isPending ? "starting" : "startupFailed")}</h1>
        {!boot.isPending && <p className={styles.description()}>{t("startupFailedHelp")}</p>}
      </div>
    </main>
  );
}
