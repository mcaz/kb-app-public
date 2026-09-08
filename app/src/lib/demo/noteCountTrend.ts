import type { NoteCountTrend } from "@/lib/api/types";

export function demoNoteCountTrend(localToday: string, scenario: string | null): NoteCountTrend {
  if (scenario === "unavailable") return { status: "unavailable", days: [] };
  const today = new Date(`${localToday}T00:00:00Z`);
  const counts = [null, null, 28, 29, null, 33, 34, 32, 35, null, 39, 40, 40, 42];
  const days = counts.map((value, index) => {
    const date = new Date(today);
    date.setUTCDate(today.getUTCDate() - 13 + index);
    const count =
      scenario === "empty" || (scenario === "single" && index < 13)
        ? null
        : scenario === "zero"
          ? 0
          : scenario === "stale" && index > 11
            ? null
            : value;
    return {
      local_date: date.toISOString().slice(0, 10),
      count,
      observed_at_ms: count === null ? null : Math.min(date.getTime() + 12 * 3_600_000, Date.now()),
    };
  });
  return { status: scenario === "empty" ? "no_observations" : "available", days };
}
