import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Button } from "@/components/atoms/ui/button";
import { Switch } from "@/components/atoms/ui/switch";
import { SinglePaneLayout } from "@/components/templates/SinglePaneLayout";
import { useErrorText } from "@/hooks/useErrorText";
import type { DistillationAiProvider, ImmediateDistillationScope, Settings } from "@/lib/api";
import {
  useDistillationProviders,
  useDistillationQueue,
  useDistillationSettings,
  useRetryDistillation,
  useRequestDistillationNow,
  useSetDistillationSettings,
  useSettings,
} from "@/lib/queries";

import { distillationSettingsVariants } from "./variants";
import { issueErrorKey } from "./issueErrorKey";
import { useDistillationSettingsDraft } from "./useDistillationSettingsDraft";
import { DistillationModelFields } from "./DistillationModelFields";
import { DistillationProgressCard } from "./DistillationProgressCard";
import { DistillationTimingCard } from "./DistillationTimingCard";

function isProviderPaused(
  provider: DistillationAiProvider | null | undefined,
  kb: Settings | undefined,
) {
  return (
    kb?.ai_kb_enabled === false ||
    (provider === "claude_code" && kb?.claude_kb_enabled === false) ||
    (provider === "codex" && kb?.gpt_kb_enabled === false)
  );
}

export function DistillationSettingsPage() {
  const { t } = useTranslation("common");
  const { data: saved, error } = useDistillationSettings();
  const { data: providers, error: providersError } = useDistillationProviders();
  const { data: queue, error: queueError } = useDistillationQueue();
  const { data: kb } = useSettings();
  const save = useSetDistillationSettings();
  const retry = useRetryDistillation();
  const requestNow = useRequestDistillationNow();
  const errorText = useErrorText();
  const form = useDistillationSettingsDraft(saved);
  const { value, dirty, change, reset, unsupportedEffort } = form;
  const styles = distillationSettingsVariants();
  const provider = providers?.find((candidate) => candidate.provider === value?.provider);
  const kbPaused = isProviderPaused(saved?.provider, kb);
  const jobs = kbPaused ? null : queue?.jobs;
  const selectedProviderPaused = isProviderPaused(value?.provider, kb);
  const unavailable = value?.provider != null && provider?.installed === false;
  const unsupported = provider?.installed === true && provider.unavailable_reason !== null;
  const saveDisabled =
    save.isPending ||
    !dirty ||
    (value?.model !== null && value?.model !== undefined && value.model.trim().length === 0) ||
    (value?.enabled === true &&
      (value.provider == null || provider?.installed !== true || unsupported || unsupportedEffort));
  const savedProvider = providers?.find((candidate) => candidate.provider === saved?.provider);
  const runBlockedReason = getRunBlockedReason();

  function getRunBlockedReason() {
    if (error || providersError || queueError) {
      return errorText(error ?? providersError ?? queueError);
    }
    if (!saved || !providers || !kb || !queue) return t("state.loading");
    if (dirty || save.isPending) return t("settings.distillation.runSaveFirst");
    if (kbPaused || (queue.paused && saved.provider !== null)) {
      return t("settings.distillation.kbPaused");
    }
    if (!saved.enabled || saved.provider === null) return t("settings.distillation.notConfigured");
    if (!savedProvider?.installed) return t("settings.distillation.providerUnavailable");
    if (savedProvider.unavailable_reason !== null) {
      return t(issueErrorKey({ code: "ai", kind: savedProvider.unavailable_reason }));
    }
    return unsupportedEffort ? t("settings.distillation.effortUnsupported") : null;
  }

  function runNow(scope: ImmediateDistillationScope) {
    if (runBlockedReason || requestNow.isPending || retry.isPending) return;
    requestNow.mutate(scope, {
      onSuccess: (result) => {
        toast(
          t(
            result.jobs.pending > 0
              ? "settings.distillation.runAccepted"
              : result.jobs.running > 0
                ? "settings.distillation.runAlreadyActive"
                : "settings.distillation.runNoEligibleNotes",
            { count: result.jobs.pending },
          ),
        );
      },
      onError: (failure) => toast(errorText(failure)),
    });
  }

  return (
    <SinglePaneLayout>
      <div className={styles.page()}>
        <h1 className={styles.title()}>{t("settings.distillation.title")}</h1>
        <p className={styles.description()}>{t("settings.distillation.description")}</p>

        <section className={styles.section()} aria-labelledby="distillation-run-now">
          <h2 id="distillation-run-now" className={styles.heading()}>
            {t("settings.distillation.runNowTitle")}
          </h2>
          <p id="distillation-run-description" className={styles.description()}>
            {t("settings.distillation.runDescription")}
          </p>
          <div className={styles.actions()}>
            <Button
              type="button"
              variant="primary"
              disabled={!!runBlockedReason || requestNow.isPending || retry.isPending}
              aria-describedby="distillation-run-description distillation-run-status"
              onClick={() => runNow("unreviewed")}
            >
              {t(
                requestNow.isPending && requestNow.variables === "unreviewed"
                  ? "settings.distillation.runRequesting"
                  : "settings.distillation.runUnreviewed",
              )}
            </Button>
            <Button
              type="button"
              variant="quiet"
              disabled={!!runBlockedReason || requestNow.isPending || retry.isPending}
              aria-describedby="distillation-run-description distillation-run-status"
              onClick={() => runNow("all")}
            >
              {t(
                requestNow.isPending && requestNow.variables === "all"
                  ? "settings.distillation.runRequesting"
                  : "settings.distillation.runAll",
              )}
            </Button>
          </div>
          <p id="distillation-run-status" className={styles.description()} role="status">
            {runBlockedReason ??
              t("settings.distillation.runProgress", {
                pending: jobs?.pending ?? 0,
                running: jobs?.running ?? 0,
              })}
          </p>
        </section>

        <DistillationTimingCard
          metrics={queue?.metrics}
          paused={kbPaused || queue?.paused === true}
          failed={!!queueError}
        />

        {!value && !error && <p className={styles.description()}>{t("state.loading")}</p>}
        {value && (
          <form
            className={styles.section()}
            onSubmit={(event) => {
              event.preventDefault();
              if (saveDisabled) return;
              save.mutate(
                { ...value, model: value.model?.trim() ?? null },
                {
                  onSuccess: () => {
                    reset();
                    toast(t("settings.distillation.saved"));
                  },
                  onError: (failure) => toast(errorText(failure)),
                },
              );
            }}
          >
            <div className={styles.row()}>
              <div className={styles.field()}>
                <label className={styles.heading()} htmlFor="distillation-enabled">
                  {t("settings.distillation.enabled")}
                </label>
                <p id="distillation-enabled-description" className={styles.description()}>
                  {t("settings.distillation.enabledDescription")}
                </p>
              </div>
              <Switch
                id="distillation-enabled"
                checked={value.enabled}
                disabled={save.isPending}
                aria-describedby="distillation-enabled-description"
                onCheckedChange={(enabled) => change({ enabled })}
              />
            </div>

            <DistillationModelFields form={form} disabled={save.isPending} />

            <div className={styles.controls()}>
              <div className={styles.field()}>
                <label className={styles.label()} htmlFor="distillation-periodic">
                  {t("settings.distillation.periodicHours")}
                </label>
                <input
                  id="distillation-periodic"
                  className={styles.input()}
                  type="number"
                  required
                  min={1}
                  max={720}
                  step={1}
                  value={value.periodic_hours}
                  disabled={save.isPending}
                  aria-describedby="distillation-periodic-description"
                  onChange={(event) => change({ periodic_hours: Number(event.target.value) })}
                />
              </div>
              <div className={styles.field()}>
                <label className={styles.label()} htmlFor="distillation-timeout">
                  {t("settings.distillation.timeoutSeconds")}
                </label>
                <input
                  id="distillation-timeout"
                  className={styles.input()}
                  type="number"
                  required
                  min={30}
                  max={1800}
                  step={1}
                  value={value.timeout_seconds}
                  disabled={save.isPending}
                  onChange={(event) => change({ timeout_seconds: Number(event.target.value) })}
                />
              </div>
            </div>
            <p id="distillation-periodic-description" className={styles.description()}>
              {t("settings.distillation.periodicDescription")}
            </p>

            {unavailable && (
              <p className={styles.warning()} role="status">
                {t("settings.distillation.providerUnavailable")}
              </p>
            )}
            {unsupported && (
              <p className={styles.warning()} role="status">
                {t(
                  provider?.unavailable_reason === "guard_outdated"
                    ? "settings.distillation.failures.guard_outdated"
                    : provider?.unavailable_reason === "guard_unavailable"
                      ? "settings.distillation.failures.guard_unavailable"
                      : "settings.distillation.providerUnsupported",
                )}
              </p>
            )}
            {selectedProviderPaused && (
              <p className={styles.warning()} role="status">
                {t("settings.distillation.kbPaused")}
              </p>
            )}
            {value.provider != null && !unavailable && !unsupported && (
              <p className={styles.description()}>{t("settings.distillation.authentication")}</p>
            )}
            <div className={styles.actions()}>
              <Button type="submit" variant="primary" disabled={saveDisabled}>
                {t(save.isPending ? "settings.distillation.saving" : "action.save")}
              </Button>
              {dirty && (
                <Button type="button" variant="quiet" disabled={save.isPending} onClick={reset}>
                  {t("action.cancel")}
                </Button>
              )}
            </div>
          </form>
        )}

        <DistillationProgressCard
          saved={saved}
          queue={queue}
          paused={kbPaused}
          retryPending={retry.isPending}
          requestPending={requestNow.isPending}
          onRetry={() => {
            retry.mutate(undefined, {
              onSuccess: () => toast(t("settings.distillation.retryQueued")),
              onError: (failure) => toast(errorText(failure)),
            });
          }}
        />

        {(error || providersError || queueError) && (
          <p className={styles.warning()} role="alert">
            {errorText(error ?? providersError ?? queueError)}
          </p>
        )}
        <p className={styles.description()}>{t("settings.note")}</p>
      </div>
    </SinglePaneLayout>
  );
}
