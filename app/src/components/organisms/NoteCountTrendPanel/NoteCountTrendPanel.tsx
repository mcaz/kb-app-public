import { RefreshCw } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/atoms/ui/button";
import { useNoteCountTrend } from "@/lib/queries";

import { formatNoteCountDate, noteCountChart } from "./chart";
import { noteCountTrendVariants } from "./variants";

export function NoteCountTrendPanel() {
  const { t, i18n } = useTranslation("home");
  const { data: trend, isPending, isError, isFetching, refetch } = useNoteCountTrend();
  const unavailable = isError || trend?.status === "unavailable";
  // 読み直せなかった履歴を、以前の成功値で覆い隠さない。
  const days = !isPending && !isError && trend?.status === "available" ? trend.days : [];
  const chart = noteCountChart(days);
  const latest = chart.latest;
  const firstDay = days[0];
  const lastDay = days.at(-1);
  const styles = noteCountTrendVariants({ unavailable });
  const number = (value: number) => value.toLocaleString(i18n.resolvedLanguage);
  const date = (value: string, short = false) =>
    formatNoteCountDate(value, i18n.resolvedLanguage, short);

  return (
    <section className={styles.section()} aria-labelledby="note-count-trend-heading">
      <div className={styles.header()}>
        <h2 id="note-count-trend-heading" className={styles.heading()}>
          {t("noteCountTrend.head")}
        </h2>
        {unavailable && (
          <Button variant="quiet" size="sm" disabled={isFetching} onClick={() => void refetch()}>
            <RefreshCw className={styles.refreshIcon()} />
            {t("noteCountTrend.retry")}
          </Button>
        )}
      </div>
      <div className={styles.card()}>
        {unavailable ? (
          <p role="status" className={styles.status()}>
            {t("noteCountTrend.unavailable")}
          </p>
        ) : isPending || !trend ? (
          <p role="status" className={styles.status()}>
            {t("noteCountTrend.loading")}
          </p>
        ) : !latest ? (
          <p role="status" className={styles.status()}>
            {t("noteCountTrend.noObservations")}
          </p>
        ) : null}

        {latest && firstDay && lastDay && (
          <>
            <p className={styles.period()}>{t("noteCountTrend.period", { days: days.length })}</p>
            <figure className={styles.figure()}>
              <figcaption className={styles.caption()}>
                <span className={styles.latestLabel()}>{t("noteCountTrend.latest")}</span>
                <span className={styles.latestValue()}>{number(latest.count)}</span>
                <span className={styles.latestDate()}>
                  {t("noteCountTrend.observedOn", { date: date(latest.localDate) })}
                </span>
              </figcaption>
              <div
                role="img"
                aria-label={t("noteCountTrend.chartLabel", {
                  from: date(firstDay.local_date),
                  to: date(lastDay.local_date),
                })}
              >
                <div aria-hidden="true" className={styles.chart()}>
                  <div className={styles.scale()}>
                    <span>{number(chart.maximum)}</span>
                    <span>{number(0)}</span>
                  </div>
                  <div className={styles.plot()}>
                    <div className={styles.drawing()}>
                      <svg
                        className={styles.svg()}
                        viewBox="0 0 100 100"
                        preserveAspectRatio="none"
                        focusable="false"
                      >
                        {chart.segments.map((segment) => (
                          <polyline
                            key={segment[0]?.localDate}
                            className={styles.line()}
                            points={segment.map((point) => `${point.x},${point.y}`).join(" ")}
                            strokeWidth="2"
                            strokeLinejoin="round"
                            vectorEffect="non-scaling-stroke"
                          />
                        ))}
                      </svg>
                      {chart.points.map((point) => (
                        <span
                          key={point.localDate}
                          className={styles.point()}
                          style={{ left: `${point.x}%`, top: `${point.y}%` }}
                          title={`${date(point.localDate)} · ${number(point.count)}`}
                        />
                      ))}
                    </div>
                  </div>
                  <div />
                  <div className={styles.axis()}>
                    <span>{date(firstDay.local_date, true)}</span>
                    <span>{date(lastDay.local_date, true)}</span>
                  </div>
                </div>
              </div>
            </figure>
            <details className={styles.details()}>
              <summary className={styles.summary()}>{t("noteCountTrend.details")}</summary>
              <div className={styles.tableWrap()}>
                <table className={styles.table()}>
                  <thead className={styles.tableHead()}>
                    <tr>
                      <th scope="col" className={styles.dateColumn()}>
                        {t("trend.date")}
                      </th>
                      <th scope="col" className={styles.countColumn()}>
                        {t("noteCountTrend.count")}
                      </th>
                    </tr>
                  </thead>
                  <tbody className={styles.tableBody()}>
                    {days.map((day) => (
                      <tr key={day.local_date}>
                        <th scope="row" className={styles.dateColumn()}>
                          {date(day.local_date)}
                        </th>
                        <td className={styles.countColumn()}>
                          {day.count === null ? t("noteCountTrend.notRecorded") : number(day.count)}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </details>
          </>
        )}
        <p className={styles.help()}>{t("noteCountTrend.help")}</p>
      </div>
    </section>
  );
}
