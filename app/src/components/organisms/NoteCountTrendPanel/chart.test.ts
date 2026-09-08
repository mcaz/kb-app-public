import { describe, expect, it } from "vitest";

import { formatNoteCountDate, noteCountChart } from "./chart";

const days = (counts: (number | null)[]) =>
  counts.map((count, index) => ({
    local_date: `2026-09-${String(index + 1).padStart(2, "0")}`,
    count,
    observed_at_ms: count === null ? null : Date.UTC(2026, 8, index + 1),
  }));

describe("noteCountChart", () => {
  it("欠測を越えて線を結ばず、欠測の日にも横軸の位置を残す", () => {
    const chart = noteCountChart(days([4, 5, null, 7, 9]));
    expect(chart.segments.map((segment) => segment.map((point) => point.count))).toEqual([
      [4, 5],
      [7, 9],
    ]);
    expect(chart.points.map((point) => point.x)).toEqual([0, 25, 75, 100]);
  });

  it("実測0は点として残し、前後の未記録を補完しない", () => {
    const chart = noteCountChart(days([null, 0, null]));
    expect(chart.maximum).toBe(1);
    expect(chart.segments).toEqual([]);
    expect(chart.points).toEqual([
      { localDate: "2026-09-02", count: 0, observedAtMs: Date.UTC(2026, 8, 2), x: 50, y: 100 },
    ]);
  });

  it("1日だけでも点が有限の位置に収まる", () => {
    expect(noteCountChart(days([5])).points[0]).toMatchObject({ x: 50, y: 0 });
    expect(noteCountChart([]).points).toEqual([]);
  });

  it("観測日の表示は閲覧時のタイムゾーンに依存しない", () => {
    expect(formatNoteCountDate("2026-09-01", "en-US")).toBe("9/1/2026");
    expect(formatNoteCountDate("2024-02-29", "ja-JP", true)).toBe("2/29");
  });

  it("西への移動で日付が戻っても最新値は観測時刻から選び、グラフの日付順を保つ", () => {
    const observations = days([12, 20]).map((day, index) => ({
      ...day,
      observed_at_ms: Date.UTC(2026, 8, 2, index === 0 ? 6 : 0),
    }));
    const chart = noteCountChart(observations);
    expect(chart.latest).toMatchObject({ localDate: "2026-09-01", count: 12 });
    expect(chart.points.map((point) => point.localDate)).toEqual(["2026-09-01", "2026-09-02"]);
    expect(
      noteCountChart(observations.map((day) => ({ ...day, observed_at_ms: Date.UTC(2026, 8, 2) })))
        .latest,
    ).toMatchObject({ localDate: "2026-09-02", count: 20 });
  });
});
