import { RefreshCw } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/atoms/ui/button";
import { type ObservationTrendFilter } from "@/lib/api";
import { useObservationTrend } from "@/lib/queries";

import {
  observationTrendBarVariants,
  observationTrendMetricVariants,
  observationTrendVariants,
} from "./variants";

const metrics = ["hook_output_emitted", "propose_successes", "update_successes", "errors"] as const;
type Metric = (typeof metrics)[number];
const clients: ObservationTrendFilter[] = ["all", "claude", "gpt"];

/** 読取失敗やOFFを、キャッシュされた実績や0件のグラフで覆い隠さない。 */
export function ObservationTrendPanel() {
  const { t, i18n } = useTranslation("home");
  const [client, setClient] = useState<ObservationTrendFilter>("all");
  const { data: trend, isPending, isError, isFetching, refetch } = useObservationTrend(client);
  const [metric, setMetric] = useState<Metric>("hook_output_emitted");
  const clientLabel = t(`trend.clients.${client}`);
  const unavailable = isError || trend?.status === "unavailable";
  const styles = observationTrendVariants({ unavailable });
  const showChart =
    !isPending && !isError && trend?.status === "available" && trend.days.length > 0;
  const days = showChart ? trend.days : [];
  const total = days.reduce((sum, day) => sum + day[metric], 0);
  const maximum = Math.max(1, ...days.map((day) => day[metric]));
  const number = (value: number) => value.toLocaleString(i18n.resolvedLanguage);
  const date = new Intl.DateTimeFormat(i18n.resolvedLanguage, {
    year: "numeric",
    month: "numeric",
    day: "numeric",
  });
  const shortDate = new Intl.DateTimeFormat(i18n.resolvedLanguage, {
    month: "numeric",
    day: "numeric",
  });
  const firstDay = days[0];
  const lastDay = days.at(-1);

  return (
    <section className={styles.section()} aria-labelledby="observation-trend-heading">
      <div className={styles.header()}>
        <h2 id="observation-trend-heading" className={styles.heading()}>
          {t("trend.head")}
        </h2>
        {unavailable && (
          <Button variant="quiet" size="sm" disabled={isFetching} onClick={() => void refetch()}>
            <RefreshCw className={styles.refreshIcon()} />
            {t("observation.retry")}
          </Button>
        )}
      </div>
      <div className={styles.card()}>
        <div role="group" aria-label={t("trend.clientFilter")} className={styles.choiceGroup()}>
          {clients.map((choice) => (
            <button
              key={choice}
              type="button"
              aria-pressed={client === choice}
              className={observationTrendMetricVariants({ selected: client === choice })}
              onClick={() => setClient(choice)}
            >
              {t(`trend.clients.${choice}`)}
            </button>
          ))}
        </div>
        <p className={styles.clientsHelp()}>{t("trend.clientsHelp")}</p>
        {unavailable ? (
          <p role="status" className={styles.status()}>
            {t("observation.unavailable")}
          </p>
        ) : isPending || !trend ? (
          <p role="status" className={styles.status()}>
            {t("observation.loading")}
          </p>
        ) : trend.status === "disabled" ? (
          <p role="status" className={styles.status()}>
            {t("observation.disabled")}
          </p>
        ) : !showChart ? (
          <p role="status" className={styles.status()}>
            {t("trend.noObservations", { client: clientLabel })}
          </p>
        ) : null}

        {showChart && firstDay && lastDay && (
          <>
            <p className={styles.period()}>{t("trend.period", { days: days.length })}</p>
            <div role="group" aria-label={t("trend.metricFilter")} className={styles.choiceGroup()}>
              {metrics.map((choice) => (
                <button
                  key={choice}
                  type="button"
                  aria-pressed={metric === choice}
                  className={observationTrendMetricVariants({ selected: metric === choice })}
                  onClick={() => setMetric(choice)}
                >
                  {t(`trend.metrics.${choice}`)}
                </button>
              ))}
            </div>
            <figure className={styles.figure()}>
              <figcaption className={styles.caption()}>
                <span className={styles.totalLabel()}>
                  {t("trend.total", { client: clientLabel, metric: t(`trend.metrics.${metric}`) })}
                </span>
                <span className={styles.totalValue()}>{number(total)}</span>
              </figcaption>
              <div
                role="img"
                aria-label={t("trend.chartLabel", {
                  client: clientLabel,
                  from: date.format(firstDay.start_ms),
                  to: date.format(lastDay.start_ms),
                  metric: t(`trend.metrics.${metric}`),
                })}
              >
                <div aria-hidden="true" className={styles.chart()}>
                  <div className={styles.scale()}>
                    <span>{number(maximum)}</span>
                    <span>{number(0)}</span>
                  </div>
                  <div className={styles.bars()}>
                    {days.map((day) => (
                      <div
                        key={day.start_ms}
                        className={styles.barCell()}
                        title={`${date.format(day.start_ms)} · ${t(`trend.metrics.${metric}`)} ${number(day[metric])}`}
                      >
                        <div
                          className={observationTrendBarVariants({ metric })}
                          style={{ height: `${(day[metric] / maximum) * 100}%` }}
                        />
                      </div>
                    ))}
                  </div>
                  <div />
                  <div className={styles.axis()}>
                    <span>{shortDate.format(firstDay.start_ms)}</span>
                    <span>{t("trend.today")}</span>
                  </div>
                </div>
              </div>
            </figure>
            {metric === "hook_output_emitted" && (
              <p className={styles.help()}>{t("trend.hookHelp")}</p>
            )}
            <p className={styles.help()}>{t("trend.zeroHelp")}</p>
            {metric === "errors" && <p className={styles.errorsHelp()}>{t("trend.errorsHelp")}</p>}
            <details className={styles.details()}>
              <summary className={styles.summary()}>
                {t("trend.details", { client: clientLabel })}
              </summary>
              <div className={styles.tableWrap()}>
                <table
                  aria-label={t("trend.details", { client: clientLabel })}
                  className={styles.table()}
                >
                  <thead className={styles.tableHead()}>
                    <tr>
                      <th scope="col" className={styles.dateHeading()}>
                        {t("trend.date")}
                      </th>
                      {metrics.map((column) => (
                        <th key={column} scope="col" className={styles.metricHeading()}>
                          {t(`trend.metrics.${column}`)}
                        </th>
                      ))}
                    </tr>
                  </thead>
                  <tbody className={styles.tableBody()}>
                    {days.map((day) => (
                      <tr key={day.start_ms}>
                        <th scope="row" className={styles.dateCell()}>
                          {date.format(day.start_ms)}
                        </th>
                        {metrics.map((column) => (
                          <td key={column} className={styles.metricCell()}>
                            {number(day[column])}
                          </td>
                        ))}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </details>
          </>
        )}
        <p className={styles.disclosure()}>{t("observation.disclosure")}</p>
      </div>
    </section>
  );
}
