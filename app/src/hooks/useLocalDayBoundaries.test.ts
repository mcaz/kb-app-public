import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useLocalDayBoundaries } from "./useLocalDayBoundaries";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("useLocalDayBoundaries", () => {
  it("開いたまま0時を越えても前日の取得期間を使い続けない", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2026, 8, 6, 23, 59, 59));
    const { result } = renderHook(useLocalDayBoundaries);
    const previous = result.current;
    await act(() => vi.advanceTimersByTime(1_000));
    expect(result.current[0]).toBe(previous[1]);
    expect(new Date(result.current[13]!).getDate()).toBe(7);
  });

  it("スリープ後のフォーカスで日付変更を拾う", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2026, 8, 6, 12));
    const { result } = renderHook(useLocalDayBoundaries);
    vi.setSystemTime(new Date(2026, 8, 8, 12));
    await act(() => window.dispatchEvent(new Event("focus")));
    expect(new Date(result.current[13]!).getDate()).toBe(8);
  });
});
