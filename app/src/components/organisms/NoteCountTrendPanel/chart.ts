import type { NoteCountTrend } from "@/lib/api";

export function noteCountChart(days: NoteCountTrend["days"]) {
  const maximum = Math.max(1, ...days.map((day) => day.count ?? 0));
  const points = days.map((day, index) =>
    day.count === null
      ? null
      : {
          localDate: day.local_date,
          count: day.count,
          observedAtMs: day.observed_at_ms,
          x: days.length === 1 ? 50 : (index / (days.length - 1)) * 100,
          y: 100 - (day.count / maximum) * 100,
        },
  );
  const segments: NonNullable<(typeof points)[number]>[][] = [];
  for (const point of points) {
    if (point === null) {
      segments.push([]);
    } else {
      if (segments.length === 0) segments.push([]);
      segments.at(-1)?.push(point);
    }
  }
  const observedPoints = points.filter((point) => point !== null);
  // 西への移動で観測日が戻っても、最新の観測は実際の時刻から選ぶ。
  const latest = observedPoints.reduce<(typeof observedPoints)[number] | undefined>(
    (current, point) => {
      if (point.observedAtMs === null) return current;
      if (
        !current ||
        current.observedAtMs === null ||
        point.observedAtMs > current.observedAtMs ||
        (point.observedAtMs === current.observedAtMs && point.localDate > current.localDate)
      ) {
        return point;
      }
      return current;
    },
    undefined,
  );
  return {
    maximum,
    latest,
    points: observedPoints,
    segments: segments.filter((segment) => segment.length > 1),
  };
}

/** 観測日の文字列は、閲覧時のタイムゾーンで別の日へ移さない。 */
export function formatNoteCountDate(localDate: string, language?: string, short = false) {
  return new Intl.DateTimeFormat(language, {
    timeZone: "UTC",
    ...(short ? {} : { year: "numeric" as const }),
    month: "numeric",
    day: "numeric",
  }).format(new Date(`${localDate}T00:00:00Z`));
}
