import { onlineManager, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, render, renderHook, screen, waitFor } from "@testing-library/react";
import { StrictMode, type ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { TooltipProvider } from "@/components/atoms/ui/tooltip";
import { NotePane } from "@/components/organisms/NotePane";
import { setupI18n } from "@/i18n";
import { api, type Degradation, type NoteView } from "@/lib/api";
import { KbError } from "@/lib/api/error";
import { useLaunchAi } from "@/lib/queries";
import { queryKeys } from "@/lib/queries/keys";
import { createQueryClient } from "@/lib/queryClient";

import { useCurrentNote } from "./useCurrentNote";

vi.mock("@/lib/api", async () => {
  const { isKbError } = await import("@/lib/api/error");
  return {
    IN_TAURI: false,
    isKbError,
    api: {
      noteSetCurrent: vi.fn(),
      noteGet: vi.fn(),
      homeState: vi.fn(),
      launchAi: vi.fn(),
    },
  };
});
vi.mock("@/components/organisms/FilePanel", () => ({ FilePanel: () => null }));

let client: ReturnType<typeof createQueryClient>;

const wrapper = ({ children }: { children: ReactNode }) => (
  <StrictMode>
    <QueryClientProvider client={client}>
      <TooltipProvider>{children}</TooltipProvider>
    </QueryClientProvider>
  </StrictMode>
);

const warning: Degradation = { code: "current_note_context", detail: "synthetic warning" };

function deferred<T>() {
  let finish!: (value: T) => void;
  const promise = new Promise<T>((resolve) => (finish = resolve));
  return { promise, finish };
}

beforeEach(() => {
  setupI18n("ja");
  client = createQueryClient();
  onlineManager.setOnline(true);
  vi.mocked(api.noteSetCurrent).mockResolvedValue([]);
  vi.mocked(api.launchAi).mockResolvedValue(null);
});

afterEach(() => {
  cleanup();
  client.clear();
  onlineManager.setOnline(true);
  vi.resetAllMocks();
});

describe("useCurrentNote", () => {
  // 2026-09-06: note_getの定期取得が「現在ノート」を上書きするため、選択時だけへ分離した。
  it("本文取得前は記録せず、同じIDの再取得とStrictModeでは重複記録しない", async () => {
    const view = renderHook(({ id }) => useCurrentNote(id), {
      initialProps: { id: null as string | null },
      wrapper,
    });
    expect(api.noteSetCurrent).not.toHaveBeenCalled();
    view.rerender({ id: "notes/a" });
    await waitFor(() => expect(api.noteSetCurrent).toHaveBeenCalledExactlyOnceWith("notes/a"));
    view.rerender({ id: "notes/a" });
    expect(api.noteSetCurrent).toHaveBeenCalledTimes(1);
    view.rerender({ id: null });
    expect(view.result.current).toEqual({ degraded: [], error: null });
    view.rerender({ id: "notes/a" });
    await waitFor(() => expect(api.noteSetCurrent).toHaveBeenCalledTimes(2));
    view.unmount();
    renderHook(() => useCurrentNote("notes/a"), { wrapper });
    await waitFor(() => expect(api.noteSetCurrent).toHaveBeenCalledTimes(3));
  });

  it("速いA→B選択は別mountでも直列化し、離れたAの遅い診断をBへ表示しない", async () => {
    const first = deferred<Degradation[]>();
    const second = deferred<Degradation[]>();
    vi.mocked(api.noteSetCurrent).mockImplementation((id) =>
      id === "notes/a" ? first.promise : second.promise,
    );
    const a = renderHook(() => useCurrentNote("notes/a"), { wrapper });
    await waitFor(() => expect(api.noteSetCurrent).toHaveBeenCalledExactlyOnceWith("notes/a"));
    a.unmount();
    const b = renderHook(() => useCurrentNote("notes/b"), { wrapper });
    expect(api.noteSetCurrent).toHaveBeenCalledTimes(1);
    await act(async () => {
      first.finish([warning]);
      await first.promise;
    });
    await waitFor(() => expect(api.noteSetCurrent).toHaveBeenNthCalledWith(2, "notes/b"));
    expect(b.result.current).toEqual({ degraded: [], error: null });
    await act(async () => {
      second.finish([]);
      await second.promise;
    });
    expect(b.result.current).toEqual({ degraded: [], error: null });
  });

  it("オフラインでも文脈記録を進め、AI起動をその完了より先に実行しない", async () => {
    onlineManager.setOnline(false);
    const pending = deferred<Degradation[]>();
    vi.mocked(api.noteSetCurrent).mockReturnValue(pending.promise);
    renderHook(() => useCurrentNote("notes/a"), { wrapper });
    await waitFor(() => expect(api.noteSetCurrent).toHaveBeenCalledExactlyOnceWith("notes/a"));
    const launch = renderHook(() => useLaunchAi(), { wrapper });
    act(() => launch.result.current.mutate("notes/b"));
    expect(api.launchAi).not.toHaveBeenCalled();
    await act(async () => {
      pending.finish([]);
      await pending.promise;
    });
    await waitFor(() => expect(api.launchAi).toHaveBeenCalledExactlyOnceWith("notes/b"));
  });

  it.each(["degraded", "typed_error"] as const)(
    "文脈記録が%sでも診断と本文を表示し、本文の更新で再記録しない",
    async (failure) => {
      const note: NoteView = {
        id: "notes/a",
        title: "Synthetic note",
        description: null,
        body: "読み続けられる本文",
        status: "stable",
        origin: "agent",
        tags: [],
        note_uid: null,
        authority: null,
        relations: [],
        created_at: null,
        generated_at: null,
        related: [],
        similar: [],
        degraded: [],
        vault_root: "",
      };
      vi.mocked(api.noteGet).mockResolvedValue(note);
      vi.mocked(api.homeState).mockResolvedValue({
        note_count: 1,
        stats: {
          total: 1,
          deprecated: 0,
          memos: 0,
          agent_notes: 1,
          links: 0,
          embed_enabled: false,
          embedded: 0,
        },
        notes: [],
        care: [],
        tags: [],
        degraded: [],
      });
      if (failure === "degraded") vi.mocked(api.noteSetCurrent).mockResolvedValue([warning]);
      else
        vi.mocked(api.noteSetCurrent).mockRejectedValue(
          new KbError({ code: "note_not_found", id: "notes/a" }),
        );
      render(<NotePane key={note.id} noteId={note.id} />, { wrapper });
      await waitFor(() => {
        expect(screen.getByText("読み続けられる本文")).toBeInTheDocument();
        if (failure === "degraded")
          expect(
            screen.getByText(/AIへ現在のノートを引き渡す文脈を記録できませんでした/),
          ).toBeInTheDocument();
        else expect(screen.getByRole("alert")).toHaveTextContent("そのノートはまだありません");
      });
      act(() => {
        client.setQueryData(queryKeys.note(note.id), { ...note, body: "更新された本文" });
      });
      await waitFor(() => expect(screen.getByText("更新された本文")).toBeInTheDocument());
      expect(api.noteSetCurrent).toHaveBeenCalledExactlyOnceWith(note.id);
    },
  );
});
