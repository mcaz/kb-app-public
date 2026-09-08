import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { NoteCountTrendPanel } from "./NoteCountTrendPanel";

import type { NoteCountTrend } from "@/lib/api";

const query = vi.hoisted(() => ({
  data: undefined as NoteCountTrend | undefined,
  isPending: false,
  isError: false,
  isFetching: false,
  refetch: vi.fn(),
}));
vi.mock("@/lib/queries", () => ({ useNoteCountTrend: () => query }));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, values?: Record<string, unknown>) =>
      values ? `${key}:${Object.values(values).map(String).join(":")}` : key,
    i18n: { resolvedLanguage: "en-US" },
  }),
}));

describe("NoteCountTrendPanel", () => {
  beforeEach(() => {
    query.data = {
      status: "available",
      days: [
        { local_date: "2026-09-01", count: 0, observed_at_ms: Date.UTC(2026, 8, 1) },
        { local_date: "2026-09-02", count: null, observed_at_ms: null },
      ],
    };
    query.isPending = false;
    query.isError = false;
    query.isFetching = false;
    query.refetch.mockReset();
  });
  afterEach(cleanup);

  it("最新の実測0とその日付を示し、日別表では未記録と区別する", () => {
    render(<NoteCountTrendPanel />);
    expect(screen.getByText("noteCountTrend.latest").nextElementSibling).toHaveTextContent("0");
    expect(screen.getByText("noteCountTrend.observedOn:9/1/2026")).toBeInTheDocument();
    const chart = screen.getByRole("img");
    expect(chart.querySelectorAll("[title]")).toHaveLength(1);
    expect(chart.querySelectorAll("polyline")).toHaveLength(0);
    const table = screen.getByRole("table", { hidden: true });
    expect(
      within(table)
        .getAllByRole("cell", { hidden: true })
        .map((cell) => cell.textContent),
    ).toEqual(["0", "noteCountTrend.notRecorded"]);
  });

  it("再取得失敗は古い値とグラフを隠し、再試行できる", () => {
    const { rerender } = render(<NoteCountTrendPanel />);
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
    query.isError = true;
    rerender(<NoteCountTrendPanel />);
    expect(screen.getByRole("status")).toHaveTextContent("noteCountTrend.unavailable");
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
    expect(screen.queryByRole("table", { hidden: true })).not.toBeInTheDocument();
    expect(screen.queryByText("noteCountTrend.latest")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "noteCountTrend.retry" }));
    expect(query.refetch).toHaveBeenCalledOnce();
  });

  it.each(["unavailable", "no_observations", "pending"] as const)(
    "%sのときに以前の件数を表示しない",
    (state) => {
      if (state === "pending") {
        query.isPending = true;
        query.isFetching = true;
      } else if (query.data) {
        query.data.status = state;
      }
      render(<NoteCountTrendPanel />);
      expect(screen.getByRole("status")).toHaveTextContent(
        state === "pending"
          ? "noteCountTrend.loading"
          : state === "no_observations"
            ? "noteCountTrend.noObservations"
            : "noteCountTrend.unavailable",
      );
      expect(screen.queryByRole("img")).not.toBeInTheDocument();
      expect(screen.queryByRole("table", { hidden: true })).not.toBeInTheDocument();
      if (state === "pending") expect(screen.queryByRole("button")).not.toBeInTheDocument();
    },
  );
});
