import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { setupI18n } from "@/i18n";
import { api, type Hit, type HomeState } from "@/lib/api";
import { queryKeys } from "@/lib/queries";
import { useSession } from "@/lib/stores/session";

import { GlobalSearchDialog } from "./GlobalSearchDialog";

vi.mock("@/components/organisms/NotePreview", () => ({
  NotePreview: ({ noteId }: { noteId: string | null }) => <div data-testid="preview">{noteId}</div>,
}));

const hit = (id: string): Hit => ({
  id,
  title: id,
  snippet: "",
  tags: [],
  origin: "agent",
  status: "stable",
  via: "browse",
  distance: null,
  note_uid: null,
  namespace: null,
  authority_role: null,
  authority_status: null,
  authority_scope: null,
  created: null,
  updated: null,
});
const home: HomeState = {
  note_count: 105,
  stats: {
    total: 110,
    deprecated: 1,
    memos: 0,
    agent_notes: 110,
    links: 0,
    embed_enabled: false,
    embedded: 0,
  },
  notes: [hit("recent/old")],
  care: [],
  tags: [],
  degraded: [],
};
let client: QueryClient;
beforeEach(() => {
  setupI18n("ja");
  useSession.setState(useSession.getInitialState(), true);
  useSession.getState().openNote("previous/detail");
  useSession.getState().go("home");
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  vi.stubGlobal(
    "matchMedia",
    vi.fn(() => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() })),
  );
  Element.prototype.scrollIntoView = vi.fn();
  vi.spyOn(api, "homeState").mockResolvedValue(home);
  vi.spyOn(api, "favoritesList").mockResolvedValue([]);
  vi.spyOn(api, "noteSearch").mockResolvedValue({
    hits: [hit("search/old")],
    related: [],
    degraded: [],
  });
});
afterEach(() => {
  cleanup();
  client.clear();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
const show = (mode: "recent" | "all" = "all", onOpenChange = vi.fn()) =>
  render(
    <QueryClientProvider client={client}>
      <GlobalSearchDialog mode={mode} open onOpenChange={onOpenChange} />
    </QueryClientProvider>,
  );

// 2026-09-08: Home件数の遷移先がカテゴリ/詳細に依存し、検索へ移しても30件で欠落する事故を防ぐ。
describe("all-note modal", () => {
  it("最近の30件に切らずページを追加し、選んだノートを開く", async () => {
    const browse = vi
      .spyOn(api, "noteBrowse")
      .mockResolvedValueOnce({
        hits: Array.from({ length: 100 }, (_, i) => hit(`all/n${i}`)),
        total: 105,
        next_cursor: "page-2",
        degraded: [],
      })
      .mockResolvedValueOnce({
        hits: Array.from({ length: 5 }, (_, i) => hit(`second/n${i}`)),
        total: 105,
        next_cursor: null,
        degraded: [],
      });
    const close = vi.fn();
    show("all", close);
    expect(await screen.findByText("105件中 100件を表示")).toBeInTheDocument();
    expect(screen.getAllByRole("option")).toHaveLength(100);
    expect(screen.queryByText("recent/old")).not.toBeInTheDocument();
    expect(screen.getByTestId("preview")).toHaveTextContent("all/n0");
    expect(useSession.getState().view).toBe("home");
    fireEvent.keyDown(screen.getByRole("button", { name: "さらに読み込む" }), { key: "Enter" });
    expect(useSession.getState().view).toBe("home");
    fireEvent.click(screen.getByRole("button", { name: "さらに読み込む" }));
    expect(await screen.findByText("105件中 105件を表示")).toBeInTheDocument();
    expect(browse).toHaveBeenLastCalledWith([], "all", "updated", "page-2", 100);
    expect(screen.getAllByRole("option")).toHaveLength(105);
    fireEvent.click(screen.getByRole("option", { name: "second/n4" }));
    expect(useSession.getState().selectedId).toBe("second/n4");
    expect(useSession.getState().view).toBe("notes");
    expect(close).toHaveBeenCalledWith(false);
  });

  it("条件変更は全件APIの先頭から取得し、古いdebounce結果を空検索へ混ぜない", async () => {
    const browse = vi.spyOn(api, "noteBrowse").mockResolvedValue({
      hits: [hit("all/start")],
      total: 1,
      next_cursor: "old-next",
      degraded: [],
    });
    show();
    await screen.findByRole("option", { name: "all/start" });
    browse.mockResolvedValue({
      hits: [hit("late/filtered")],
      total: 1,
      next_cursor: null,
      degraded: [],
    });
    act(() => {
      useSession.getState().setSearchTags(["tag"]);
      useSession.getState().setPeriod("7");
      useSession.getState().setSort("title");
    });
    await screen.findByRole("option", { name: "late/filtered" });
    expect(browse).toHaveBeenLastCalledWith(["tag"], "7", "title", null, 100);
    expect(screen.queryByText("all/start")).not.toBeInTheDocument();
    fireEvent.change(screen.getByRole("combobox"), { target: { value: "old query" } });
    await waitFor(() => expect(api.noteSearch).toHaveBeenCalledWith("old query"));
    fireEvent.change(screen.getByRole("combobox"), { target: { value: "" } });
    expect(screen.getByText("すべてのノート")).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "late/filtered" })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: "search/old" })).not.toBeInTheDocument();
    expect(useSession.getState().view).toBe("home");
  });

  it("初回失敗は0件とせず再試行でき、追加ページ失敗でも既読ページを保つ", async () => {
    const browse = vi.spyOn(api, "noteBrowse").mockRejectedValueOnce(new Error("unavailable"));
    show();
    expect(await screen.findByRole("alert")).toHaveTextContent("ノート一覧を読み込めませんでした");
    expect(screen.queryByText("見つかりませんでした")).not.toBeInTheDocument();
    expect(screen.queryByText("0件中 0件を表示")).not.toBeInTheDocument();
    browse.mockResolvedValueOnce({
      hits: [hit("all/first")],
      total: 2,
      next_cursor: "next",
      degraded: [],
    });
    fireEvent.click(screen.getByRole("button", { name: "もう一度試す" }));
    await screen.findByRole("option", { name: "all/first" });
    browse.mockRejectedValueOnce(new Error("next failed"));
    fireEvent.click(screen.getByRole("button", { name: "さらに読み込む" }));
    await screen.findByRole("alert");
    expect(screen.getByRole("option", { name: "all/first" })).toBeInTheDocument();
    browse.mockResolvedValueOnce({
      hits: [hit("all/second")],
      total: 2,
      next_cursor: null,
      degraded: [],
    });
    fireEvent.click(screen.getByRole("button", { name: "もう一度試す" }));
    await screen.findByText("2件中 2件を表示");
    expect(browse).toHaveBeenLastCalledWith([], "all", "updated", "next", 100);
  });

  it("取得中と空一覧を区別し、Escでもホームと以前の本文は変更しない", async () => {
    vi.spyOn(api, "noteBrowse").mockReturnValue(new Promise(() => {}));
    const close = vi.fn();
    show("all", close);
    expect(screen.queryByText("見つかりませんでした")).not.toBeInTheDocument();
    act(() => {
      client.setQueryData(queryKeys.noteBrowse([], "all", "updated"), {
        pages: [{ hits: [], total: 0, next_cursor: null, degraded: [] }],
        pageParams: [null],
      });
    });
    expect(await screen.findByText("見つかりませんでした")).toBeInTheDocument();
    expect(screen.getByText("0件中 0件を表示")).toBeInTheDocument();
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(close).toHaveBeenCalledWith(false);
    expect(useSession.getState()).toMatchObject({ view: "home", selectedId: "previous/detail" });
  });

  it("通常の検索入口は最近のノートを表示し、全件APIを呼ばない", async () => {
    const browse = vi.spyOn(api, "noteBrowse");
    show("recent");
    await screen.findByRole("option", { name: "recent/old" });
    expect(screen.getByText("最近のノート")).toBeInTheDocument();
    expect(browse).not.toHaveBeenCalled();
  });
});
