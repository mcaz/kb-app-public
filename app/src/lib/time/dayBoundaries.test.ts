import { afterEach, describe, expect, it, vi } from "vitest";

import { localDayBoundaries } from "./dayBoundaries";

afterEach(() => vi.unstubAllEnvs());

describe("localDayBoundaries", () => {
  it("年をまたぐ14日と翌日0時を、端末の暦日で区切る", () => {
    vi.stubEnv("TZ", "Asia/Tokyo");
    const boundaries = localDayBoundaries(new Date(2026, 0, 3, 12));
    expect(boundaries).toHaveLength(15);
    expect(new Date(boundaries[0]!).toISOString()).toBe("2025-12-20T15:00:00.000Z");
    expect(new Date(boundaries[14]!).toISOString()).toBe("2026-01-03T15:00:00.000Z");
    expect(boundaries.every((value) => new Date(value).getHours() === 0)).toBe(true);
  });

  it.each([
    [2, 10, 23],
    [10, 3, 25],
  ])("夏時間の切り替えを含む月=%s・日=%sでは%s時間の日を保つ", (month, day, hours) => {
    vi.stubEnv("TZ", "America/New_York");
    const boundaries = localDayBoundaries(new Date(2026, month, day, 12));
    const durations = boundaries
      .slice(1)
      .map((end, index) => (end - boundaries[index]!) / 3_600_000);
    expect(durations).toContain(hours);
    expect(durations.filter((value) => value === 24)).toHaveLength(13);
    expect(boundaries.every((value) => new Date(value).getHours() === 0)).toBe(true);
  });
});
