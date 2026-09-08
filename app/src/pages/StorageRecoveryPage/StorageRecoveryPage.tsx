import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/atoms/ui/button";
import { useErrorText } from "@/hooks/useErrorText";
import { isKbError } from "@/lib/api";
import { useRecoveryApply, useRecoveryExit, useRecoveryPlan } from "@/lib/queries";

import { storageRecoveryVariants } from "./variants";

export function StorageRecoveryPage() {
  const { t } = useTranslation("recovery");
  const styles = storageRecoveryVariants();
  const errorText = useErrorText();
  const planning = useRecoveryPlan();
  const applying = useRecoveryApply();
  const exiting = useRecoveryExit();
  const [acknowledged, setAcknowledged] = useState(false);
  const [attempted, setAttempted] = useState(false);
  const [copying, setCopying] = useState(false);
  const [copyStatus, setCopyStatus] = useState<"copied" | "copyFailed" | null>(null);
  const actionInFlight = useRef(false);
  const plan = planning.data;
  const receipt = applying.data;
  const unproven = plan?.summary?.latest_state_unproven;
  const ready =
    plan !== undefined &&
    plan.supported_reset_shape &&
    plan.snapshot_complete &&
    plan.existing_markdown_coverage_complete &&
    plan.blocking_issue_count === 0 &&
    plan.plan_digest !== null &&
    plan.markdown_notes !== null &&
    plan.markdown_notes > 0 &&
    unproven !== undefined;
  const busy = planning.isPending || applying.isPending || exiting.isPending || copying;
  const canApply = ready && !busy && !attempted && (unproven === 0 || acknowledged);
  const count = (value: number | null | undefined) =>
    value === null || value === undefined ? t("unknown") : t("count", { count: value });

  async function compare() {
    if (actionInFlight.current || receipt) return;
    actionInFlight.current = true;
    setAcknowledged(false);
    planning.reset();
    applying.reset();
    try {
      await planning.mutateAsync();
      setAttempted(false);
    } catch {
      // 取得失敗はmutationのerror状態で表示し、古い照合結果へ戻さない。
    } finally {
      actionInFlight.current = false;
    }
  }

  async function recover() {
    if (actionInFlight.current || !canApply || !plan?.plan_digest) return;
    actionInFlight.current = true;
    // 結果不明でも同じ計画を再適用しない。新たな照合が成功するまで保持する。
    setAttempted(true);
    try {
      await applying.mutateAsync({
        expected_plan_digest: plan.plan_digest,
        acknowledge_unproven: acknowledged,
      });
    } catch {
      // 自動retryはせず、再照合への案内だけを表示する。
    } finally {
      actionInFlight.current = false;
    }
  }

  async function copyResult() {
    if (actionInFlight.current || !receipt) return;
    actionInFlight.current = true;
    setCopying(true);
    setCopyStatus(null);
    try {
      await navigator.clipboard.writeText(JSON.stringify(receipt, null, 2));
      setCopyStatus("copied");
    } catch {
      setCopyStatus("copyFailed");
    } finally {
      setCopying(false);
      actionInFlight.current = false;
    }
  }

  async function quit() {
    if (actionInFlight.current) return;
    actionInFlight.current = true;
    try {
      await exiting.mutateAsync();
    } catch {
      // エラー詳細にはパス等を含み得るため、画面は固定した案内だけを表示する。
    } finally {
      actionInFlight.current = false;
    }
  }

  return (
    <main className={styles.root()}>
      <div className={styles.content()}>
        <h1 className={styles.title()}>{t("title")}</h1>
        <p className={styles.description()}>{t("paused")}</p>
        {receipt ? (
          <section className={styles.card()} aria-labelledby="recovery-complete">
            <h2 id="recovery-complete" className={styles.heading()}>
              {t("successTitle")}
            </h2>
            <dl className={styles.stats()}>
              <div className={styles.statistic()}>
                <dt className={styles.label()}>{t("restored")}</dt>
                <dd className={styles.value()}>{count(receipt.restored_notes)}</dd>
              </div>
              <div className={styles.statistic()}>
                <dt className={styles.label()}>{t("preservedHistory")}</dt>
                <dd className={styles.value()}>
                  {count(
                    receipt.preserved_ledgers.find(
                      (ledger) => ledger.table === "distillation_job_runs",
                    )?.rows,
                  )}
                </dd>
              </div>
            </dl>
            <div>
              <p className={styles.label()}>{t("backup")}</p>
              <p className={styles.resultId()}>{receipt.backup_id}</p>
            </div>
            <p className={styles.description()}>{t("successHelp")}</p>
            <div className={styles.actions()}>
              <Button disabled={busy} onClick={() => void copyResult()}>
                {t(copying ? "copying" : "copy")}
              </Button>
            </div>
            {copyStatus && (
              <p
                role={copyStatus === "copyFailed" ? "alert" : "status"}
                className={copyStatus === "copyFailed" ? styles.error() : styles.description()}
              >
                {t(copyStatus)}
              </p>
            )}
          </section>
        ) : (
          <>
            <p className={styles.description()}>{t("intro")}</p>
            <div className={styles.actions()}>
              <Button disabled={busy} onClick={() => void compare()}>
                {t(planning.isPending ? "planning" : attempted || plan ? "planAgain" : "plan")}
              </Button>
            </div>
            {planning.isError && (
              <p role="alert" className={styles.error()}>
                {t("planFailed")}
              </p>
            )}
            {plan && (
              <section className={styles.card()} aria-labelledby="recovery-plan-title">
                <h2 id="recovery-plan-title" className={styles.heading()}>
                  {t("planTitle")}
                </h2>
                <dl className={styles.stats()}>
                  {(
                    [
                      ["candidates", plan.markdown_notes],
                      ["confirmed", plan.summary?.completed_review_matches],
                      ["unproven", unproven],
                      ["history", plan.history_runs],
                      ["problems", plan.blocking_issue_count],
                    ] as const
                  ).map(([label, value]) => (
                    <div key={label} className={styles.statistic()}>
                      <dt className={styles.label()}>{t(label)}</dt>
                      <dd className={styles.value()}>{count(value)}</dd>
                    </div>
                  ))}
                </dl>
                {unproven !== undefined && unproven > 0 && (
                  <>
                    <p className={styles.description()}>{t("unprovenHelp")}</p>
                    <details className={styles.details()}>
                      <summary className={styles.summary()}>
                        {t("unprovenDetails", { count: unproven })}
                      </summary>
                      <ul className={styles.list()}>
                        {plan.notes
                          .filter((note) => note.freshness !== "current_completed_review")
                          .map((note) => (
                            <li key={note.note}>{note.note}</li>
                          ))}
                      </ul>
                      {plan.notes_truncated && (
                        <p className={styles.description()}>{t("detailsLimited")}</p>
                      )}
                    </details>
                    <label className={styles.acknowledgement()}>
                      <input
                        type="checkbox"
                        className={styles.checkbox()}
                        checked={acknowledged}
                        disabled={busy || attempted || !ready}
                        onChange={(event) => setAcknowledged(event.target.checked)}
                      />
                      <span>{t("acknowledge")}</span>
                    </label>
                  </>
                )}
                {!ready && (
                  <p role="alert" className={styles.error()}>
                    {t("notReady")}
                  </p>
                )}
                <div className={styles.actions()}>
                  <Button variant="primary" disabled={!canApply} onClick={() => void recover()}>
                    {applying.isPending
                      ? t("applying")
                      : plan.markdown_notes === null
                        ? t("applyUnavailable")
                        : t("apply", { count: plan.markdown_notes })}
                  </Button>
                </div>
              </section>
            )}
            {applying.isPending && (
              <p role="status" className={styles.description()}>
                {t("applyingHelp")}
              </p>
            )}
            {applying.isError && (
              <div role="alert" className={styles.error()}>
                {isKbError(applying.error) && <p>{errorText(applying.error)}</p>}
                <p>{t("applyFailed")}</p>
              </div>
            )}
          </>
        )}
        <div className={styles.actions()}>
          <Button disabled={busy} variant="quiet" onClick={() => void quit()}>
            {t("exit")}
          </Button>
        </div>
        {exiting.isError && (
          <p role="alert" className={styles.error()}>
            {t("exitFailed")}
          </p>
        )}
      </div>
    </main>
  );
}
