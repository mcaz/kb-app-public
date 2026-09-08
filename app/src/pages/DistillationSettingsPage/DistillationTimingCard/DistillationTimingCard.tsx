import { Fragment, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Button } from "@/components/atoms/ui/button";
import { formatDateTime } from "@/lib/format";

import { effortLabel } from "../modelChoice";
import {
  failureCode,
  formatDuration,
  stageOrder,
  stageSummary,
  type Metrics,
  type Run,
} from "./timing";
import { distillationTimingVariants } from "./variants";

function TimingRun({ run }: { run: Run }) {
  const { t, i18n } = useTranslation("common");
  const styles = distillationTimingVariants();
  const active = run.outcome === null;
  const stage = active
    ? [...run.stages].reverse().find((entry) => entry.finished_at_ms === null)
    : null;
  const aiCalls = run.stages.filter((entry) => entry.stage === "ai_response").length;
  const duration = (ms: number | null) =>
    formatDuration(ms, i18n.language, t("settings.distillation.timing.notMeasured"));
  const elapsed = duration(run.elapsed_ms);
  const result = t(`settings.distillation.timing.outcomes.${run.outcome ?? "running"}`);
  const code = failureCode(run.failure);
  const configuration = [
    run.provider === "codex"
      ? "Codex"
      : run.provider === "claude_code"
        ? "Claude Code"
        : t("settings.distillation.timing.notRecorded"),
    run.model ?? t("settings.distillation.defaultModel"),
    run.reasoning_effort
      ? effortLabel(run.reasoning_effort)
      : t("settings.distillation.defaultEffort"),
  ].join(" / ");
  return (
    <details className={styles.run()} open={active ? true : undefined}>
      <summary className={styles.summary()}>
        <span className={styles.result()}>{result}</span>
        <span className={styles.metadata()}>
          {" · "}
          {t("settings.distillation.timing.batchSize", { count: run.batch_size })}
        </span>
        <span className={styles.elapsed()}>
          {run.outcome === "interrupted"
            ? t("settings.distillation.timing.interruptedDuration", { duration: elapsed })
            : run.elapsed_is_estimate
              ? t("settings.distillation.timing.estimatedDuration", { duration: elapsed })
              : elapsed}
        </span>
        <span className={styles.metadata()}>
          {" · "}
          {formatDateTime(new Date(run.started_at_ms).toJSON(), i18n.language, t("date.unknown"))}
        </span>
      </summary>
      <div className={styles.content()}>
        {active && (
          <p className={styles.help()}>
            {t("settings.distillation.timing.currentStage", {
              stage: stage
                ? t(`settings.distillation.timing.stages.${stage.stage}`)
                : t("settings.distillation.timing.betweenStages"),
            })}
          </p>
        )}
        <p className={styles.metadata()}>
          {t("settings.distillation.timing.configuration", { configuration })}
        </p>
        <p className={styles.help()}>
          {t("settings.distillation.timing.completedNotes", {
            completed: run.completed_notes ?? t("settings.distillation.timing.notRecorded"),
            total: run.batch_size,
          })}
          {" · "}
          {t("settings.distillation.timing.inputBytes", {
            bytes:
              run.input_bytes == null
                ? t("settings.distillation.timing.notRecorded")
                : new Intl.NumberFormat(i18n.language).format(run.input_bytes),
          })}
        </p>
        <p className={styles.help()}>
          {t("settings.distillation.timing.attempt", {
            attempt: run.attempt,
          })}
          {" · "}
          {t("settings.distillation.timing.aiCalls", { count: aiCalls })}
        </p>
        {code && (
          <p className={styles.code()}>{t("settings.distillation.timing.failureCode", { code })}</p>
        )}
        <dl className={styles.stages()}>
          {stageOrder.map((name) => {
            const summary = stageSummary(run, name);
            const elapsed = duration(summary.elapsed);
            return (
              <Fragment key={name}>
                <dt className={styles.stageLabel()}>
                  {t(`settings.distillation.timing.stages.${name}`)}
                  {summary.calls > 1 && (
                    <span className={styles.state()}>
                      {t("settings.distillation.timing.calls", { count: summary.calls })}
                    </span>
                  )}
                </dt>
                <dd className={styles.stageValue()}>
                  {summary.elapsed === null
                    ? elapsed
                    : summary.interrupted
                      ? t("settings.distillation.timing.recordedDuration", { duration: elapsed })
                      : summary.estimated
                        ? t("settings.distillation.timing.estimatedDuration", { duration: elapsed })
                        : elapsed}
                  {(summary.active || summary.failed) && (
                    <span className={styles.state()}>
                      {t(
                        summary.active
                          ? "settings.distillation.timing.stageRunning"
                          : "settings.distillation.timing.stageFailed",
                      )}
                    </span>
                  )}
                </dd>
              </Fragment>
            );
          })}
        </dl>
      </div>
    </details>
  );
}

export function DistillationTimingCard({
  metrics,
  paused,
  failed,
}: {
  metrics: Metrics | null | undefined;
  paused: boolean;
  failed: boolean;
}) {
  const { t } = useTranslation("common");
  const styles = distillationTimingVariants();
  const [copying, setCopying] = useState(false);
  const runs = metrics?.runs.slice(0, 20) ?? [];
  const running = runs.filter((run) => run.outcome === null);
  const history = runs.filter((run) => run.outcome !== null);
  async function copy() {
    if (!metrics) return;
    setCopying(true);
    try {
      await navigator.clipboard.writeText(JSON.stringify(metrics, null, 2));
      toast(t("settings.distillation.timing.copied"));
    } catch {
      toast(t("settings.distillation.timing.copyFailed"));
    } finally {
      setCopying(false);
    }
  }
  return (
    <section className={styles.section()} aria-labelledby="distillation-timing">
      <div className={styles.header()}>
        <h2 id="distillation-timing" className={styles.heading()}>
          {t("settings.distillation.timing.title")}
        </h2>
        <Button
          type="button"
          variant="quiet"
          disabled={!metrics || runs.length === 0 || copying || paused}
          onClick={() => void copy()}
        >
          {t("settings.distillation.timing.copy")}
        </Button>
      </div>
      <p className={styles.help()}>{t("settings.distillation.timing.description")}</p>
      {paused ? (
        <p className={styles.help()}>{t("settings.distillation.timing.paused")}</p>
      ) : (
        <>
          {(failed || metrics === null || metrics?.available === false) && (
            <p className={styles.warning()} role="alert">
              {t("settings.distillation.timing.unavailable")}
            </p>
          )}
          {metrics === undefined && !failed && (
            <p className={styles.help()}>{t("state.loading")}</p>
          )}
          {runs.length === 0 && metrics?.available && (
            <p className={styles.help()}>{t("settings.distillation.timing.empty")}</p>
          )}
          <div className={styles.list()}>
            {running.map((run) => (
              <TimingRun key={run.run_id} run={run} />
            ))}
          </div>
          {history.length > 0 && (
            <details>
              <summary className={styles.summary()}>
                {t("settings.distillation.timing.history", { count: history.length })}
              </summary>
              <div className={styles.content()}>
                {history.map((run) => (
                  <TimingRun key={run.run_id} run={run} />
                ))}
              </div>
            </details>
          )}
          {runs.length > 0 && (
            <>
              <p className={styles.help()}>
                {t("settings.distillation.timing.durationDescription")}
              </p>
              <p className={styles.help()}>{t("settings.distillation.timing.inputDescription")}</p>
            </>
          )}
        </>
      )}
    </section>
  );
}
