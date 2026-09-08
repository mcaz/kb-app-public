import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { setupI18n } from "@/i18n";
import { api, type HomeState } from "@/lib/api";
import { demoProposalTickets } from "@/lib/demo/proposals";
import { queryKeys, useHomeState } from "@/lib/queries";
import type * as Queries from "@/lib/queries";
import { useSession } from "@/lib/stores/session";
import { ProposalsPage } from "@/pages/ProposalsPage";

import { HomePage } from "./HomePage";

vi.mock("@/lib/queries", async (importOriginal) => ({
  ...(await importOriginal<typeof Queries>()),
  useConnectState: () => ({ data: undefined }),
}));
vi.mock("@/components/organisms/NoteCountTrendPanel", () => ({ NoteCountTrendPanel: () => null }));
vi.mock("@/components/organisms/ObservationTrendPanel", () => ({
  ObservationTrendPanel: () => null,
}));
vi.mock("@/components/organisms/ObservationHealthPanel", () => ({
  ObservationHealthPanel: () => null,
}));

const refresh = {
  updatedAt: 0,
  isFetching: false,
  isError: false,
  detectionFailed: false,
  retry: () => {},
};

const home: HomeState = {
  note_count: 160,
  stats: {
    total: 165,
    deprecated: 0,
    memos: 0,
    agent_notes: 165,
    links: 1077,
    embed_enabled: true,
    embedded: 165,
  },
  notes: [],
  care: Array.from({ length: 5 }, (_, index) => ({
    key: `broken-${index}`,
    kind: "broken",
    a: `notes/source-${index}`,
    b: `notes/missing-${index}`,
    detail: "",
  })),
  tags: [],
  degraded: [],
};

let client: QueryClient;
beforeEach(() => {
  setupI18n("ja");
  useSession.setState(useSession.getInitialState(), true);
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
});
afterEach(() => {
  cleanup();
  client.clear();
  vi.restoreAllMocks();
});

const show = (withList = false) =>
  render(
    <QueryClientProvider client={client}>
      <HomePage refresh={refresh} home={home} onOpenAllNotes={() => {}} />
      {withList && <ProposalsPage />}
    </QueryClientProvider>,
  );

// 2026-09-06: ホームが手入れ候補5件を「提案」と表示し、提案一覧の全1件と食い違った。
describe("home proposal count", () => {
  it("採用済みも含む一覧と同じ件数を表示し、同じ取得結果で更新する", async () => {
    const tickets = demoProposalTickets();
    const approved = tickets.find((ticket) => ticket.status === "approved")!;
    const list = vi
      .spyOn(api, "proposalList")
      .mockResolvedValue({ tickets: [approved], degraded: [] });
    show(true);
    expect(await screen.findByRole("button", { name: "1 提案" })).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: /^すべて\s*1$/ })).toBeInTheDocument();
    expect(list).toHaveBeenCalledTimes(1);

    list.mockResolvedValue({ tickets, degraded: [] });
    await act(() => client.invalidateQueries({ queryKey: queryKeys.proposalList }));
    expect(
      await screen.findByRole("button", { name: `${tickets.length} 提案` }),
    ).toBeInTheDocument();
    expect(
      await screen.findByRole("button", { name: new RegExp(`^すべて\\s*${tickets.length}$`) }),
    ).toBeInTheDocument();
    expect(list).toHaveBeenCalledTimes(2);
  });

  it("0件も正しく表示し、以前の詳細選択を解除して提案一覧を開く", async () => {
    vi.spyOn(api, "proposalList").mockResolvedValue({ tickets: [], degraded: [] });
    useSession.getState().openProposal("notes/previous-proposal");
    useSession.getState().go("home");
    show();
    fireEvent.click(await screen.findByRole("button", { name: "0 提案" }));
    expect(useSession.getState().view).toBe("proposals");
    expect(useSession.getState().selectedId).toBeNull();
  });

  it("取得中は0件や手入れ候補数を表示しない", () => {
    vi.spyOn(api, "proposalList").mockReturnValue(new Promise(() => {}));
    show();
    expect(screen.getByRole("button", { name: "… 提案" })).toBeInTheDocument();
  });

  it.each([false, true])("取得失敗は不明と表示する（以前の値あり=%s）", async (cached) => {
    if (cached) {
      client.setQueryData(queryKeys.proposalList, { tickets: demoProposalTickets(), degraded: [] });
    }
    vi.spyOn(api, "proposalList").mockRejectedValue(new Error("unavailable"));
    show();
    expect(await screen.findByRole("alert")).toHaveTextContent("提案件数を取得できませんでした");
    expect(screen.getByRole("button", { name: "— 提案" })).toBeInTheDocument();
  });
});

// 2026-09-08: 件数を押して以前のカテゴリ・詳細が再表示される事故の再現。
describe("home all-note entry", () => {
  it.each(["list", "note"] as const)(
    "以前の%sへ遷移せず、通常参照できる件数で全件検索を開く",
    async (pane) => {
      vi.spyOn(api, "proposalList").mockResolvedValue({ tickets: [], degraded: [] });
      useSession.getState().selectCategory("research");
      if (pane === "note") useSession.getState().openListedNote("research/previous");
      useSession.getState().go("home");
      const before = useSession.getState();
      const openAll = vi.fn();
      render(
        <QueryClientProvider client={client}>
          <HomePage refresh={refresh} home={home} onOpenAllNotes={openAll} />
        </QueryClientProvider>,
      );
      fireEvent.click(screen.getByRole("button", { name: "160 ノート" }));
      expect(openAll).toHaveBeenCalledOnce();
      expect(useSession.getState()).toBe(before);
      await screen.findByRole("button", { name: "0 提案" });
    },
  );
});

// 2026-09-08: ホームの再取得失敗が隠れ、古い件数だけが表示され続けていた。
describe("home refresh status", () => {
  it.each([false, true])(
    "取得失敗を表示して更新から復帰できる（キャッシュあり=%s）",
    async (cached) => {
      vi.spyOn(api, "proposalList").mockResolvedValue({ tickets: [], degraded: [] });
      if (cached) client.setQueryData(queryKeys.home, home);
      const read = vi.spyOn(api, "homeState").mockRejectedValueOnce(new Error("read failed"));
      read.mockResolvedValue({ ...home, note_count: 161 });
      function Home() {
        const query = useHomeState();
        return (
          <HomePage
            home={query.data}
            onOpenAllNotes={() => {}}
            refresh={{
              ...refresh,
              updatedAt: query.dataUpdatedAt,
              isFetching: query.isFetching,
              isError: query.isError,
              retry: () => void query.refetch({ cancelRefetch: false }),
            }}
          />
        );
      }
      render(
        <QueryClientProvider client={client}>
          <Home />
        </QueryClientProvider>,
      );
      expect(await screen.findByRole("alert")).toHaveTextContent(
        cached ? "前回取得した件数を表示しています" : "ホームを取得できませんでした",
      );
      if (cached) expect(screen.getByRole("button", { name: "160 ノート" })).toBeInTheDocument();
      else expect(screen.queryByRole("button", { name: /ノート$/ })).not.toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "更新" }));
      expect(await screen.findByRole("button", { name: "161 ノート" })).toBeInTheDocument();
      expect(screen.queryByRole("alert")).not.toBeInTheDocument();
      expect(screen.getByText(/^最終取得 /)).toBeInTheDocument();
    },
  );
});
