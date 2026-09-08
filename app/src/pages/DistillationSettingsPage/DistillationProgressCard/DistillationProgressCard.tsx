import { useTranslation } from "react-i18next";

import { Button } from "@/components/atoms/ui/button";
import type { DistillationAiSettings, DistillationQueueView } from "@/lib/api";
import { formatDateTime } from "@/lib/format";

import { issueErrorKey } from "../issueErrorKey";
import { distillationProgressVariants } from "./variants";

interface Props {
  saved: DistillationAiSettings | undefined;
  queue: DistillationQueueView | undefined;
  paused: boolean;
  retryPending: boolean;
  requestPending: boolean;
  onRetry: () => void;
}

export function DistillationProgressCard({
  saved,
  queue,
  paused,
  retryPending,
  requestPending,
  onRetry,
}: Props) {
  const { t, i18n } = useTranslation("common");
  const styles = distillationProgressVariants();
  const jobs = paused ? null : queue?.jobs;
  const issues = jobs ? (queue?.issues ?? []) : [];

  return (
    <section className={styles.section()} aria-labelledby="distillation-progress">
      <h2 id="distillation-progress" className={styles.heading()}>
        {t("settings.distillation.progress")}
      </h2>
      <p className={styles.description()} role="status">
        {t(
          !saved || !queue
            ? "state.loading"
            : paused
              ? "settings.distillation.kbPaused"
              : saved?.provider == null
                ? "settings.distillation.notConfigured"
                : saved?.enabled === false
                  ? "settings.distillation.paused"
                  : "settings.distillation.runningDescription",
        )}
      </p>
      {jobs && (
        <dl className={styles.metrics()}>
          {(
            [
              ["pending", jobs.pending],
              ["running", jobs.running],
              ["retryWait", jobs.retry_wait],
              ["blocked", jobs.blocked],
              ["completed", jobs.completed],
            ] as const
          ).map(([label, count]) => (
            <div key={label} className={styles.metric()}>
              <dt className={styles.description()}>{t(`settings.distillation.${label}`)}</dt>
              <dd className={styles.count()}>{count}</dd>
            </div>
          ))}
        </dl>
      )}
      {jobs?.oldest_pending_at != null && (
        <p className={styles.description()}>
          {t("settings.distillation.oldestPending", {
            when: formatDateTime(
              new Date(jobs.oldest_pending_at * 1000).toJSON(),
              i18n.language,
              t("date.unknown"),
            ),
          })}
        </p>
      )}
      <p className={styles.description()}>{t("settings.distillation.recoveryDescription")}</p>
      {issues.length > 0 && (
        <div className={styles.field()}>
          <h3 className={styles.heading()}>{t("settings.distillation.issues")}</h3>
          <p className={styles.description()}>
            {t("settings.distillation.issueCount", {
              shown: issues.length,
              total: (jobs?.blocked ?? 0) + (jobs?.retry_wait ?? 0),
            })}
          </p>
          <ul className={styles.issueList()}>
            {issues.map((issue) => (
              <li key={issue.note} className={styles.issue()}>
                <div className={styles.issueHeading()}>
                  <h4 className={styles.issueTitle()}>{issue.title ?? issue.note}</h4>
                  <span className={styles.issueState()}>
                    {t(
                      issue.state === "blocked"
                        ? "settings.distillation.blocked"
                        : "settings.distillation.retryWait",
                    )}
                  </span>
                </div>
                <p className={styles.issueNote()}>{issue.note}</p>
                <p className={styles.issueReason()}>
                  {issue.error.code === "needs_review"
                    ? issue.error.reason
                    : t(issueErrorKey(issue.error))}
                </p>
                {issue.state === "retry_wait" && (
                  <p className={styles.description()}>
                    {t("settings.distillation.nextRetry", {
                      when: formatDateTime(
                        new Date(issue.available_at * 1000).toJSON(),
                        i18n.language,
                        t("date.unknown"),
                      ),
                    })}
                  </p>
                )}
              </li>
            ))}
          </ul>
        </div>
      )}
      <div className={styles.actions()}>
        <Button
          type="button"
          disabled={
            retryPending ||
            requestPending ||
            !saved?.enabled ||
            !jobs ||
            jobs.retry_wait + jobs.blocked === 0
          }
          onClick={onRetry}
        >
          {t("settings.distillation.retryNow")}
        </Button>
      </div>
    </section>
  );
}
