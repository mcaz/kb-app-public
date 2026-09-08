import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ObservationTrendPanel } from "./ObservationTrendPanel";

import type { ObservationTrend, ObservationTrendFilter } from "@/lib/api";

const queries = vi.hoisted(() => {
  const createQuery = () => ({
    data: undefined as ObservationTrend | undefined,
    isPending: false,
    isError: false,
    isFetching: false,
    refetch: vi.fn(),
  });
  return { all: createQuery(), claude: createQuery(), gpt: createQuery() };
});
const query = queries.all;

vi.mock("@/lib/queries", () => ({
  useObservationTrend: (client: ObservationTrendFilter) => queries[client],
}));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, values?: Record<string, unknown>) =>
      values ? `${key}:${Object.values(values).map(String).join(":")}` : key,
    i18n: { resolvedLanguage: "en" },
  }),
}));

const fixture = (): ObservationTrend => ({
  status: "available",
  days: Array.from({ length: 14 }, (_, index) => ({
    start_ms: new Date(2026, 8, index + 1).getTime(),
    end_ms: new Date(2026, 8, index + 2).getTime(),
    hook_output_emitted: index === 0 ? 3 : index === 1 ? 7 : 0,
    propose_successes: index === 0 ? 5 : 0,
    update_successes: index === 1 ? 1 : 0,
    errors: index === 2 ? 2 : 0,
  })),
});

describe("ObservationTrendPanel", () => {
  beforeEach(() => {
    for (const result of Object.values(queries)) {
      result.data = fixture();
      result.isPending = false;
      result.isError = false;
      result.isFetching = false;
      result.refetch.mockReset();
    }
  });
  afterEach(cleanup);

  it("再取得が失敗したら過去のグラフと日別表を隠す", () => {
    const { rerender } = render(<ObservationTrendPanel />);
    expect(screen.getByRole("img")).toBeInTheDocument();
    query.isError = true;
    rerender(<ObservationTrendPanel />);

    expect(screen.getByRole("status")).toHaveTextContent("observation.unavailable");
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
    expect(screen.queryByRole("table", { hidden: true })).not.toBeInTheDocument();
    expect(screen.queryByText(/^trend.noObservations/)).not.toBeInTheDocument();
  });

  it.each(["disabled", "unavailable", "no_observations"] as const)(
    "%sでは以前の数値が残っていても表示しない",
    (status) => {
      query.data = { ...fixture(), status };
      render(<ObservationTrendPanel />);

      expect(screen.getByRole("status")).toHaveTextContent(
        status === "no_observations" ? "trend.noObservations" : `observation.${status}`,
      );
      expect(screen.queryByRole("img")).not.toBeInTheDocument();
      expect(screen.queryByRole("table", { hidden: true })).not.toBeInTheDocument();
    },
  );

  it("初回読取中を観測なしや0件のグラフに置き換えない", () => {
    query.isPending = true;
    query.isFetching = true;
    render(<ObservationTrendPanel />);

    expect(screen.getByRole("status")).toHaveTextContent("observation.loading");
    expect(screen.queryByRole("button", { name: "observation.retry" })).not.toBeInTheDocument();
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
  });

  it("選んだ指標の合計と日別数値を表示し、エラーには集計範囲を示す", () => {
    render(<ObservationTrendPanel />);
    expect(
      screen.getByText("trend.total:trend.clients.all:trend.metrics.hook_output_emitted")
        .nextElementSibling,
    ).toHaveTextContent("10");
    expect(
      screen.getByRole("button", { name: "trend.metrics.hook_output_emitted" }),
    ).toHaveAttribute("aria-pressed", "true");

    fireEvent.click(screen.getByRole("button", { name: "trend.metrics.errors" }));

    expect(
      screen.getByText("trend.total:trend.clients.all:trend.metrics.errors").nextElementSibling,
    ).toHaveTextContent("2");
    expect(screen.getByRole("button", { name: "trend.metrics.errors" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(screen.getByText("trend.errorsHelp")).toBeInTheDocument();
    expect(screen.getByRole("img")).toHaveAccessibleName(/trend.metrics.errors$/);

    const table = screen.getByRole("table", { hidden: true });
    const rows = within(table).getAllByRole("row", { hidden: true });
    expect(rows).toHaveLength(15);
    expect(
      within(rows[1]!)
        .getAllByRole("cell", { hidden: true })
        .map((cell) => cell.textContent),
    ).toEqual(["3", "5", "0", "0"]);
    expect(screen.getByText("observation.disclosure")).toBeInTheDocument();
  });

  it("0件の日に棒の高さを作らず、全日0件の指標も選択できる", () => {
    const data = fixture();
    query.data = {
      ...data,
      days: data.days.map((day) => ({ ...day, hook_output_emitted: 0 })),
    };
    render(<ObservationTrendPanel />);

    const bars = screen.getByRole("img").querySelectorAll<HTMLElement>("[style]");
    expect(bars).toHaveLength(14);
    expect([...bars].every((bar) => bar.style.height === "0%")).toBe(true);
    expect(
      screen.getByText("trend.total:trend.clients.all:trend.metrics.hook_output_emitted")
        .nextElementSibling,
    ).toHaveTextContent("0");
    expect(screen.getByText("trend.zeroHelp")).toBeInTheDocument();
    expect(screen.queryByText(/^trend.noObservations/)).not.toBeInTheDocument();
  });

  it("接続元を切り替えても指標を保持し、取得中に別の接続元の実績を残さない", () => {
    queries.claude.data = {
      ...fixture(),
      days: fixture().days.map((day, index) => ({ ...day, errors: index === 0 ? 9 : 0 })),
    };
    queries.gpt.data = undefined;
    queries.gpt.isPending = true;
    queries.gpt.isFetching = true;
    const { rerender } = render(<ObservationTrendPanel />);
    expect(screen.getByRole("button", { name: "trend.clients.all" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    fireEvent.click(screen.getByRole("button", { name: "trend.metrics.errors" }));
    fireEvent.click(screen.getByRole("button", { name: "trend.clients.claude" }));
    expect(
      screen.getByText("trend.total:trend.clients.claude:trend.metrics.errors").nextElementSibling,
    ).toHaveTextContent("9");
    expect(screen.getByRole("img")).toHaveAccessibleName(/trend.clients.claude/);
    expect(screen.getByRole("table", { hidden: true })).toHaveAccessibleName(
      "trend.details:trend.clients.claude",
    );

    fireEvent.click(screen.getByRole("button", { name: "trend.clients.gpt" }));
    expect(screen.getByRole("status")).toHaveTextContent("observation.loading");
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
    expect(screen.queryByRole("table", { hidden: true })).not.toBeInTheDocument();
    expect(screen.queryByText(/^trend.total/)).not.toBeInTheDocument();

    queries.gpt.data = {
      ...fixture(),
      days: fixture().days.map((day, index) => ({ ...day, errors: index === 0 ? 4 : 0 })),
    };
    queries.gpt.isPending = false;
    queries.gpt.isFetching = false;
    rerender(<ObservationTrendPanel />);
    expect(
      screen.getByText("trend.total:trend.clients.gpt:trend.metrics.errors").nextElementSibling,
    ).toHaveTextContent("4");

    fireEvent.click(screen.getByRole("button", { name: "trend.clients.all" }));
    expect(
      screen.getByText("trend.total:trend.clients.all:trend.metrics.errors").nextElementSibling,
    ).toHaveTextContent("2");
    expect(screen.getByRole("button", { name: "trend.metrics.errors" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
  });

  it.each(["disabled", "unavailable", "no_observations", "error", "loading"] as const)(
    "%sの接続元から別の接続元へ戻れる",
    (status) => {
      queries.claude.data = {
        ...fixture(),
        status: status === "error" || status === "loading" ? "available" : status,
      };
      queries.claude.isError = status === "error";
      queries.claude.isPending = status === "loading";
      render(<ObservationTrendPanel />);
      fireEvent.click(screen.getByRole("button", { name: "trend.clients.claude" }));
      expect(screen.queryByRole("img")).not.toBeInTheDocument();
      expect(screen.queryByRole("table", { hidden: true })).not.toBeInTheDocument();
      expect(screen.getByRole("button", { name: "trend.clients.claude" })).toHaveAttribute(
        "aria-pressed",
        "true",
      );

      fireEvent.click(screen.getByRole("button", { name: "trend.clients.all" }));
      expect(screen.getByRole("img")).toHaveAccessibleName(/trend.clients.all/);
      expect(
        screen.getByText("trend.total:trend.clients.all:trend.metrics.hook_output_emitted")
          .nextElementSibling,
      ).toHaveTextContent("10");
    },
  );
});
