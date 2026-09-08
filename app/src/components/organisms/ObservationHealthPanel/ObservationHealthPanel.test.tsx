import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { demoObservationHealth } from "@/lib/demo/observationHealth";

import { ObservationHealthPanel } from "./ObservationHealthPanel";

import type { ObservationHealth } from "@/lib/api";

const query = vi.hoisted(() => ({
  data: undefined as ObservationHealth | undefined,
  isPending: false,
  isError: false,
  isFetching: false,
  refetch: vi.fn(),
}));

vi.mock("@/lib/queries", () => ({ useObservationHealth: () => query }));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, values?: Record<string, unknown>) =>
      values ? `${key}:${Object.values(values).map(String).join(":")}` : key,
    i18n: { resolvedLanguage: "en" },
  }),
}));

const fixture = (phase: string | null = null) => demoObservationHealth(phase, {});

describe("ObservationHealthPanel", () => {
  beforeEach(() => {
    query.data = fixture();
    query.isPending = false;
    query.isError = false;
    query.isFetching = false;
    query.refetch.mockReset();
  });
  afterEach(cleanup);

  /** 2026-09-05: 取得失敗後もQueryに残る過去の実績を、現在の正常値として描かない。 */
  it("再取得失敗時はキャッシュに残る数値を隠して不明と示す", () => {
    const { rerender } = render(<ObservationHealthPanel />);
    expect(screen.getAllByRole("article", { hidden: true })).toHaveLength(3);
    expect(screen.queryByRole("button", { name: "observation.retry" })).not.toBeInTheDocument();

    query.isError = true;
    rerender(<ObservationHealthPanel />);

    expect(screen.getByRole("status")).toHaveTextContent("observation.unavailable");
    expect(screen.getByRole("button", { name: "observation.retry" })).toBeEnabled();
    expect(screen.queryByRole("article", { hidden: true })).not.toBeInTheDocument();
    expect(screen.queryByText("observation.noObservations")).not.toBeInTheDocument();
  });

  /** 2026-09-05: OFFや台帳読取不可の状態は、古いcountsがあっても優先する。 */
  it.each(["disabled", "unavailable"] as const)("%sでは古いカードを表示しない", (status) => {
    const { rerender } = render(<ObservationHealthPanel />);
    query.data = { ...fixture(), status };
    rerender(<ObservationHealthPanel />);

    expect(screen.getByRole("status")).toHaveTextContent(`observation.${status}`);
    expect(screen.queryByRole("article", { hidden: true })).not.toBeInTheDocument();
    expect(screen.queryByText("observation.unassignedHead")).not.toBeInTheDocument();
  });

  it("初回取得中を観測なしへ置き換えない", () => {
    query.data = undefined;
    query.isPending = true;
    query.isFetching = true;
    render(<ObservationHealthPanel />);

    expect(screen.getByRole("status")).toHaveTextContent("observation.loading");
    expect(screen.queryByRole("button", { name: "observation.retry" })).not.toBeInTheDocument();
    expect(screen.queryByText("observation.noObservations")).not.toBeInTheDocument();
    expect(screen.queryByRole("article", { hidden: true })).not.toBeInTheDocument();
  });

  it("観測なしではゼロ件の実績カードを作らない", () => {
    query.data = fixture("empty");
    render(<ObservationHealthPanel />);

    expect(screen.getByText("observation.noObservations")).toBeInTheDocument();
    expect(screen.queryByRole("article", { hidden: true })).not.toBeInTheDocument();
    expect(screen.queryByText("observation.unavailable")).not.toBeInTheDocument();
  });

  it("出力完了と準備残留を分け、応答のエラーを成功や未分類へ混ぜない", () => {
    render(<ObservationHealthPanel />);
    const selected = screen.getAllByRole("article")[0]!;
    const card = within(selected);

    expect(card.getByText("observation.hookEmitted").nextElementSibling).toHaveTextContent("18");
    expect(card.getByText("observation.hookPending").nextElementSibling).toHaveTextContent("1");
    expect(card.getByText("observation.hookFailed").nextElementSibling).toHaveTextContent("2");
    expect(card.getByText("observation.trimmedDocuments").nextElementSibling).toHaveTextContent(
      "6",
    );
    expect(card.getByText("observation.writeCounts:4:2")).toBeInTheDocument();
    expect(card.getByText("observation.writeCounts:7:1")).toBeInTheDocument();

    const rejection = card.getByText("observation.rejections.tag_vocabulary").closest("tr")!;
    const unclassified = card.getByText("observation.unclassified").closest("tr")!;
    expect(
      within(rejection)
        .getAllByRole("cell", { hidden: true })
        .map((cell) => cell.textContent),
    ).toEqual(["1", "0"]);
    expect(
      within(unclassified)
        .getAllByRole("cell", { hidden: true })
        .map((cell) => cell.textContent),
    ).toEqual(["1", "0"]);
    expect(screen.getByText("observation.disclosure")).toBeInTheDocument();
  });

  it("未帰属だけの記録を選択中Vaultの実績にしない", () => {
    query.data = fixture("unassigned");
    render(<ObservationHealthPanel />);

    expect(screen.getByText("observation.noObservations")).toBeInTheDocument();
    const details = screen.getByText("observation.unassignedHead").closest("details")!;
    expect(within(details).getByText("observation.unassignedHelp")).toBeInTheDocument();
    const unassigned = screen.getByRole("article", { hidden: true });
    expect(details).toContainElement(unassigned);
    expect(details.open).toBe(false);
  });

  it("直近の観測がなくても保持期間内の最後の起票応答を消さない", () => {
    query.data = fixture("historical");
    render(<ObservationHealthPanel />);

    expect(screen.getByText("observation.period:14")).toBeInTheDocument();
    expect(screen.getByText("observation.daysAgo:30")).toBeInTheDocument();
    expect(screen.getByText("observation.noHookObservations")).toBeInTheDocument();
    expect(screen.getAllByText("observation.notObserved")).toHaveLength(2);
    expect(screen.queryByText("observation.hookEmitted")).not.toBeInTheDocument();
  });

  it("片方だけOFFならそのクライアントの過去記録と明示する", () => {
    query.data = fixture("partial-off");
    render(<ObservationHealthPanel />);

    const off = screen.getByText("observation.currentlyOff").closest("article")!;
    expect(within(off).getByText("observation.clients.claude_code")).toBeInTheDocument();
    expect(within(off).getByText("observation.writeCounts:1:0")).toBeInTheDocument();
    expect(screen.queryByText("observation.disabled")).not.toBeInTheDocument();
  });
});
