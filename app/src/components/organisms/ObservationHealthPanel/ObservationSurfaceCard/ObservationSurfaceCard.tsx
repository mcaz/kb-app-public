import { useTranslation } from "react-i18next";

import type { ObservationSurfaceHealth } from "@/lib/api";

export interface ObservationSurfaceCardProps {
  health: ObservationSurfaceHealth;
  retentionDays: number;
}

/** 0件の系列と観測自体の欠落を分け、現在OFFの過去記録も明示する。 */
export function ObservationSurfaceCard({ health, retentionDays }: ObservationSurfaceCardProps) {
  const { t, i18n } = useTranslation("home");
  const counts = health.counts;
  const count = (value: number) => value.toLocaleString(i18n.resolvedLanguage);
  const hasHook =
    counts.hook_output_prepared +
      counts.hook_output_emitted +
      counts.hook_stdout_failed +
      counts.hook_filtered +
      counts.hook_errors >
    0;
  const hookMetrics = [
    ["hookEmitted", counts.hook_output_emitted],
    ["hookPending", counts.hook_output_prepared],
    ["hookFailed", counts.hook_stdout_failed + counts.hook_errors],
    ["trimmedDocuments", counts.trimmed_documents],
  ] as const;
  const writes = [
    ["propose", counts.propose_successes, counts.propose_errors],
    ["update", counts.update_successes, counts.update_errors],
  ] as const;
  const hookDetails = [
    ["stdoutFailed", counts.hook_stdout_failed],
    ["hookErrors", counts.hook_errors],
    ["hookFiltered", counts.hook_filtered],
    ["cappedOutputs", counts.capped_outputs],
    ["emittedDocuments", counts.emitted_documents],
  ] as const;
  const hasWriteErrors = counts.propose_errors + counts.update_errors > 0;

  return (
    <article className="border-line bg-surface min-w-0 rounded-lg border p-3">
      <h3 className="text-ink text-sm font-semibold">
        {t(`observation.clients.${health.surface}`)}
      </h3>
      {!health.kb_enabled && (
        <p className="text-muted mt-1 text-[11px]">{t("observation.currentlyOff")}</p>
      )}
      <h4 className="text-muted mt-3 mb-1.5 text-[11px] font-medium">
        {t("observation.hookHead")}
      </h4>
      {hasHook ? (
        <dl className="grid grid-cols-2 gap-x-3 gap-y-2">
          {hookMetrics.map(([label, value]) => (
            <div key={label}>
              <dt className="text-muted text-[11px]">{t(`observation.${label}`)}</dt>
              <dd className="text-ink text-base font-semibold tabular-nums">{count(value)}</dd>
            </div>
          ))}
        </dl>
      ) : (
        <p className="text-muted text-xs">{t("observation.noHookObservations")}</p>
      )}

      <dl className="border-line mt-3 space-y-2 border-t pt-3 text-xs">
        {writes.map(([label, successes, errors]) => (
          <div
            key={label}
            className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1"
          >
            <dt className="text-muted">{t(`observation.${label}`)}</dt>
            <dd className="text-ink tabular-nums">
              {successes + errors > 0
                ? t("observation.writeCounts", {
                    successes: count(successes),
                    errors: count(errors),
                  })
                : t("observation.notObserved")}
            </dd>
          </div>
        ))}
        <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
          <dt className="text-muted">{t("observation.lastPropose")}</dt>
          <dd className="text-ink tabular-nums">
            {health.last_propose_days === null
              ? t("observation.noRecentPropose", { days: retentionDays })
              : t("observation.daysAgo", { days: count(health.last_propose_days) })}
          </dd>
        </div>
      </dl>

      {(hasHook || hasWriteErrors) && (
        <details className="border-line mt-3 border-t pt-2">
          <summary className="text-muted cursor-pointer text-xs">
            {t("observation.details")}
          </summary>
          {hasHook && (
            <>
              <dl className="mt-2 space-y-1 text-[11px]">
                {hookDetails.map(([label, value]) => (
                  <div key={label} className="flex justify-between gap-3">
                    <dt className="text-muted">{t(`observation.${label}`)}</dt>
                    <dd className="text-ink tabular-nums">{count(value)}</dd>
                  </div>
                ))}
              </dl>
              <p className="text-muted mt-2 text-[11px] leading-relaxed">
                {t("observation.hookHelp")}
              </p>
            </>
          )}
          {hasWriteErrors && (
            <div className="mt-3">
              <h4 className="text-muted mb-1 text-[11px] font-medium">
                {t("observation.rejectionsHead")}
              </h4>
              <table className="w-full text-left text-[11px]">
                <thead>
                  <tr className="text-muted">
                    <th scope="col" className="pr-2 pb-1 font-normal">
                      {t("observation.reason")}
                    </th>
                    <th scope="col" className="pr-2 pb-1 text-right font-normal">
                      {t("observation.proposeShort")}
                    </th>
                    <th scope="col" className="pb-1 text-right font-normal">
                      {t("observation.updateShort")}
                    </th>
                  </tr>
                </thead>
                <tbody className="text-ink">
                  {counts.write_rejections.map((rejection) => (
                    <tr key={rejection.code}>
                      <th scope="row" className="py-0.5 pr-2 font-normal">
                        {t(`observation.rejections.${rejection.code}`)}
                      </th>
                      <td className="pr-2 text-right tabular-nums">{count(rejection.propose)}</td>
                      <td className="text-right tabular-nums">{count(rejection.update)}</td>
                    </tr>
                  ))}
                  {counts.unclassified_propose_errors + counts.unclassified_update_errors > 0 && (
                    <tr>
                      <th scope="row" className="py-0.5 pr-2 font-normal">
                        {t("observation.unclassified")}
                      </th>
                      <td className="pr-2 text-right tabular-nums">
                        {count(counts.unclassified_propose_errors)}
                      </td>
                      <td className="text-right tabular-nums">
                        {count(counts.unclassified_update_errors)}
                      </td>
                    </tr>
                  )}
                </tbody>
              </table>
            </div>
          )}
        </details>
      )}
    </article>
  );
}
