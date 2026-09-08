import { focusManager, onlineManager, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { api, type HomeState, type Settings, type TicketView } from "@/lib/api";
import { createQueryClient } from "@/lib/queryClient";

import {
  useHomeState,
  useMaintenanceRefresh,
  useNoteSearch,
  useObservationTrend,
  useProposal,
  useProposals,
} from "./index";

import type { ReactNode } from "react";

vi.mock("@/hooks/useLocalDayBoundaries", () => ({
  useLocalDayBoundaries: () => Array.from({ length: 15 }, (_, day) => day * 86_400_000),
}));
vi.mock("@/lib/api", () => ({
  api: {
    homeState: vi.fn(),
    proposalList: vi.fn(),
    proposalGet: vi.fn(),
    settingsGet: vi.fn(),
    homeObservationTrend: vi.fn(),
    maintenanceRefresh: vi.fn(),
    noteSearch: vi.fn(),
  },
}));

const clients: ReturnType<typeof createQueryClient>[] = [];
function wrapper() {
  const client = createQueryClient();
  clients.push(client);
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  );
}

const home = (total: number): HomeState => ({
  note_count: total,
  stats: {
    total,
    deprecated: 0,
    memos: 0,
    agent_notes: total,
    links: 0,
    embed_enabled: false,
    embedded: 0,
  },
  notes: [],
  care: [],
  tags: [],
  degraded: [],
});

const ticket: TicketView = {
  note_id: "notes/proposal-test",
  note_uid: "proposal-test",
  title: "Test proposal",
  status: "review_pending",
  etag: "etag-1",
  current_revision: 1,
  revisions: [],
  reviews: [],
  decisions: [],
};

async function advance(milliseconds = 0) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(milliseconds);
    await vi.advanceTimersByTimeAsync(1);
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  focusManager.setFocused(true);
  onlineManager.setOnline(true);
  vi.mocked(api.homeState).mockResolvedValue(home(1));
  vi.mocked(api.proposalList).mockResolvedValue({ tickets: [], degraded: [] });
  vi.mocked(api.proposalGet).mockResolvedValue({ ticket, degraded: [] });
  vi.mocked(api.settingsGet).mockResolvedValue({});
  vi.mocked(api.homeObservationTrend).mockResolvedValue({ status: "available", days: [] });
  vi.mocked(api.maintenanceRefresh).mockResolvedValue({ elapsed_ms: 1, degraded: [] });
  vi.mocked(api.noteSearch).mockResolvedValue({ hits: [], related: [], degraded: [] });
});

afterEach(() => {
  cleanup();
  for (const client of clients.splice(0)) client.clear();
  focusManager.setFocused(undefined);
  onlineManager.setOnline(true);
  vi.useRealTimers();
  vi.resetAllMocks();
});

describe("live query refresh", () => {
  it("表示中のホームと提案を15秒で更新し、画面を閉じた後は取得しない", async () => {
    const { result, unmount } = renderHook(
      () => ({ home: useHomeState(), proposals: useProposals() }),
      { wrapper: wrapper() },
    );
    await advance();
    expect(result.current.home.data?.stats.total).toBe(1);
    expect(result.current.proposals.data?.tickets).toEqual([]);

    vi.mocked(api.homeState).mockResolvedValue(home(2));
    vi.mocked(api.proposalList).mockResolvedValue({ tickets: [ticket], degraded: [] });
    await advance(15_000);
    expect(result.current.home.data?.stats.total).toBe(2);
    expect(result.current.proposals.data?.tickets).toEqual([ticket]);
    expect(api.homeState).toHaveBeenCalledTimes(2);
    expect(api.proposalList).toHaveBeenCalledTimes(2);

    unmount();
    await advance(45_000);
    expect(api.homeState).toHaveBeenCalledTimes(2);
    expect(api.proposalList).toHaveBeenCalledTimes(2);
  });

  it("無効なホーム購読や未選択の提案を定期取得せず、選択解除後も止める", async () => {
    const { rerender } = renderHook(
      ({ enabled, note }) => ({ home: useHomeState(enabled), proposal: useProposal(note) }),
      { initialProps: { enabled: false, note: null as string | null }, wrapper: wrapper() },
    );
    await advance(30_000);
    expect(api.homeState).not.toHaveBeenCalled();
    expect(api.proposalGet).not.toHaveBeenCalled();

    rerender({ enabled: true, note: ticket.note_id });
    await advance();
    expect(api.homeState).toHaveBeenCalledTimes(1);
    expect(api.proposalGet).toHaveBeenCalledTimes(1);
    rerender({ enabled: false, note: null });
    await advance(30_000);
    expect(api.homeState).toHaveBeenCalledTimes(1);
    expect(api.proposalGet).toHaveBeenCalledTimes(1);
  });

  it("非表示では定期取得を止め、オフラインでも表示中のローカル情報は更新する", async () => {
    const { result } = renderHook(
      () => ({ home: useHomeState(), maintenance: useMaintenanceRefresh() }),
      { wrapper: wrapper() },
    );
    await advance();
    focusManager.setFocused(false);
    await advance(30_000);
    expect(api.homeState).toHaveBeenCalledTimes(1);
    expect(api.maintenanceRefresh).toHaveBeenCalledTimes(1);

    onlineManager.setOnline(false);
    focusManager.setFocused(true);
    vi.mocked(api.homeState).mockResolvedValue(home(3));
    await advance(15_000);
    expect(result.current.home.data?.stats.total).toBe(3);
    expect(api.homeState).toHaveBeenCalledTimes(2);
    expect(api.maintenanceRefresh).toHaveBeenCalledTimes(1);

    await advance(15_000);
    expect(api.maintenanceRefresh).toHaveBeenCalledTimes(2);
    expect(result.current.maintenance.fetchStatus).toBe("idle");
    await advance(60_000);
    expect(api.maintenanceRefresh).toHaveBeenCalledTimes(3);
  });

  it("閉じた検索の条件を保持していても取得せず、開いている間だけ定期更新する", async () => {
    const { rerender } = renderHook(({ query, open }) => useNoteSearch(query, open), {
      initialProps: { query: "search", open: false },
      wrapper: wrapper(),
    });
    await advance(30_000);
    expect(api.noteSearch).not.toHaveBeenCalled();

    rerender({ query: "search", open: true });
    await advance();
    expect(api.noteSearch).toHaveBeenCalledTimes(1);
    await advance(15_000);
    expect(api.noteSearch).toHaveBeenCalledTimes(2);

    rerender({ query: "search", open: false });
    await advance(30_000);
    expect(api.noteSearch).toHaveBeenCalledTimes(2);
    rerender({ query: "changed while closed", open: false });
    await advance(30_000);
    expect(api.noteSearch).toHaveBeenCalledTimes(2);

    rerender({ query: " ", open: true });
    await advance(30_000);
    expect(api.noteSearch).toHaveBeenCalledTimes(2);
    rerender({ query: "reopened", open: true });
    await advance();
    expect(api.noteSearch).toHaveBeenLastCalledWith("reopened");
    expect(api.noteSearch).toHaveBeenCalledTimes(3);
  });

  it("外部でKBがOFFになったら設定の次回取得で成功キャッシュを非表示にする", async () => {
    let settings: Settings = { ai_kb_enabled: true };
    vi.mocked(api.settingsGet).mockImplementation(() => Promise.resolve(settings));
    const { result } = renderHook(() => useObservationTrend("claude"), { wrapper: wrapper() });
    await advance();
    expect(result.current.data?.status).toBe("available");

    settings = { ai_kb_enabled: false };
    await advance(15_000);
    expect(result.current.data?.status).toBe("disabled");
    expect(api.settingsGet).toHaveBeenCalledTimes(2);
    const readsAfterOff = vi.mocked(api.homeObservationTrend).mock.calls.length;
    await advance(15_000);
    expect(result.current.data?.status).toBe("disabled");
    expect(api.homeObservationTrend).toHaveBeenCalledTimes(readsAfterOff);
  });

  it("通常表示と観測は15秒で更新し、保守は60秒ごとに限る", async () => {
    renderHook(
      () => ({
        home: useHomeState(),
        trend: useObservationTrend("all"),
        maintenance: useMaintenanceRefresh(),
      }),
      { wrapper: wrapper() },
    );
    await advance();
    await advance(15_000);
    expect(api.homeState).toHaveBeenCalledTimes(2);
    expect(api.homeObservationTrend).toHaveBeenCalledTimes(2);
    expect(api.maintenanceRefresh).toHaveBeenCalledTimes(1);
    await advance(15_000);
    expect(api.homeObservationTrend).toHaveBeenCalledTimes(3);
    expect(api.maintenanceRefresh).toHaveBeenCalledTimes(1);
    await advance(30_000);
    expect(api.maintenanceRefresh).toHaveBeenCalledTimes(2);
  });
});
