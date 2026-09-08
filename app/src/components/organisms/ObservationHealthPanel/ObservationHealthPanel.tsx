import { RefreshCw } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/atoms/ui/button";
import { useObservationHealth } from "@/lib/queries";

import { ObservationSurfaceCard } from "./ObservationSurfaceCard";

/** 台帳の状態はノート一覧と独立させ、読取失敗を空の実績へ置き換えない。 */
export function ObservationHealthPanel() {
  const { t } = useTranslation("home");
  const { data: health, isPending, isError, isFetching, refetch } = useObservationHealth();
  const unavailable = isError || health?.status === "unavailable";
  const showRecords =
    !isError &&
    health !== undefined &&
    (health.status === "available" || health.status === "no_observations");

  return (
    <section className="mt-7" aria-labelledby="observation-health-heading">
      <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
        <h2
          id="observation-health-heading"
          className="text-muted text-xl font-medium tracking-[0.04em]"
        >
          {t("observation.head")}
        </h2>
        {unavailable && (
          <Button variant="quiet" size="sm" disabled={isFetching} onClick={() => void refetch()}>
            <RefreshCw className="size-3.5" />
            {t("observation.retry")}
          </Button>
        )}
      </div>
      <div className="border-line bg-panel rounded-xl border p-4">
        {unavailable ? (
          <p role="status" className="text-danger text-sm">
            {t("observation.unavailable")}
          </p>
        ) : isPending || !health ? (
          <p role="status" className="text-muted text-sm">
            {t("observation.loading")}
          </p>
        ) : health.status === "disabled" ? (
          <p role="status" className="text-muted text-sm">
            {t("observation.disabled")}
          </p>
        ) : null}

        {showRecords && (
          <>
            <p className="text-muted mb-3 text-xs">
              {t("observation.period", { days: health.period_days })}
            </p>
            {health.surfaces.length === 0 ? (
              <p className="text-muted text-sm">{t("observation.noObservations")}</p>
            ) : (
              <div className="grid grid-cols-[repeat(auto-fit,minmax(min(100%,250px),1fr))] gap-3">
                {health.surfaces.map((surface) => (
                  <ObservationSurfaceCard
                    key={surface.surface}
                    health={surface}
                    retentionDays={health.retention_days}
                  />
                ))}
              </div>
            )}

            {health.unassigned.length > 0 && (
              <details className="border-line mt-4 border-t pt-3">
                <summary className="text-prop cursor-pointer text-xs font-medium">
                  {t("observation.unassignedHead")}
                </summary>
                <p className="text-muted mt-2 mb-3 text-xs">{t("observation.unassignedHelp")}</p>
                <div className="grid grid-cols-[repeat(auto-fit,minmax(min(100%,250px),1fr))] gap-3">
                  {health.unassigned.map((surface) => (
                    <ObservationSurfaceCard
                      key={surface.surface}
                      health={surface}
                      retentionDays={health.retention_days}
                    />
                  ))}
                </div>
              </details>
            )}
          </>
        )}
        <p className="text-muted mt-3 text-[11px] leading-relaxed">{t("observation.disclosure")}</p>
      </div>
    </section>
  );
}
