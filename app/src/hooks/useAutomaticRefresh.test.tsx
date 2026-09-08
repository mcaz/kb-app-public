import { QueryClient, QueryClientProvider, QueryObserver } from "@tanstack/react-query";
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { queryKeys } from "@/lib/queries/keys";
import { api } from "@/lib/api";
import { liveQueryOptions } from "@/lib/queries/refreshPolicy";

import { useAutomaticRefresh } from "./useAutomaticRefresh";

import type { ReactNode } from "react";

const native = vi.hoisted(() => ({ listen: vi.fn(), isFocused: vi.fn() }));
vi.mock("@/lib/api", () => ({ IN_TAURI: true, api: { noteRevision: vi.fn() } }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ onFocusChanged: native.listen, isFocused: native.isFocused }),
}));

let client: QueryClient;
let focus: (event: { payload: boolean }) => void;
let unlisten: () => void;
let subscriptions: (() => void)[];

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-09-06T12:00:00Z"));
  vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
  client = new QueryClient({
    defaultOptions: { queries: { retry: false, refetchOnWindowFocus: false } },
  });
  subscriptions = [];
  unlisten = vi.fn();
  native.listen.mockImplementation((callback: typeof focus) => {
    focus = callback;
    return Promise.resolve(unlisten);
  });
  native.isFocused.mockResolvedValue(true);
  vi.mocked(api.noteRevision).mockImplementation(() => new Promise<string>(() => {}));
});

afterEach(() => {
  cleanup();
  subscriptions.forEach((unsubscribe) => unsubscribe());
  client.clear();
  vi.restoreAllMocks();
  vi.resetAllMocks();
  vi.useRealTimers();
});

function observe(
  queryKey: readonly string[],
  { live = true, enabled = true, staleTime = Infinity } = {},
) {
  let value = 0;
  const fetch = vi.fn(() => Promise.resolve(++value));
  const observer = new QueryObserver(client, {
    queryKey,
    queryFn: fetch,
    initialData: 0,
    staleTime,
    enabled,
    meta: { refreshOnResume: live },
  });
  subscriptions.push(observer.subscribe(() => {}));
  return fetch;
}

const wrapper = ({ children }: { children: ReactNode }) => (
  <QueryClientProvider client={client}>{children}</QueryClientProvider>
);

const flush = () => act(() => vi.advanceTimersByTimeAsync(50));

describe("useAutomaticRefresh", () => {
  // 2026-09-08: 本文を毎秒再取得せず、MCP保存のDB revisionだけで表示を更新する。
  it("初回と変更時だけlive queryを更新し、同じtokenや保守へ毎秒波及させない", async () => {
    vi.mocked(api.noteRevision).mockResolvedValue("vault-a:1");
    const home = observe(queryKeys.home);
    const maintenance = observe(queryKeys.maintenance, { live: false, staleTime: 60_000 });
    renderHook(() => useAutomaticRefresh(true), { wrapper });
    await flush();
    await flush();
    await flush();
    expect(home).toHaveBeenCalledTimes(1);
    const probes = vi.mocked(api.noteRevision).mock.calls.length;
    await act(() => vi.advanceTimersByTimeAsync(3_000));
    expect(api.noteRevision).toHaveBeenCalledTimes(probes + 3);
    expect(home).toHaveBeenCalledTimes(1);
    expect(maintenance).not.toHaveBeenCalled();

    vi.mocked(api.noteRevision).mockResolvedValue("vault-a:2");
    await act(() => vi.advanceTimersByTimeAsync(1_000));
    await flush();
    await flush();
    expect(home).toHaveBeenCalledTimes(2);
    expect(maintenance).not.toHaveBeenCalled();

    vi.mocked(api.noteRevision).mockResolvedValue("vault-b:2");
    await client.invalidateQueries({ queryKey: queryKeys.noteRevision });
    await flush();
    await flush();
    expect(home).toHaveBeenCalledTimes(3);
  });

  it("初回probeが先行Home取得の後の保存を見ても、旧応答で更新要求を失わない", async () => {
    let finish: (value: number) => void = () => {};
    const fetch = vi
      .fn(() => Promise.resolve(2))
      .mockImplementationOnce(() => new Promise<number>((resolve) => (finish = resolve)));
    const observer = new QueryObserver(client, {
      queryKey: queryKeys.home,
      queryFn: fetch,
      initialData: 0,
      staleTime: Infinity,
      meta: { refreshOnResume: true },
    });
    subscriptions.push(observer.subscribe(() => {}));
    const previous = client.refetchQueries({ queryKey: queryKeys.home });
    vi.mocked(api.noteRevision).mockResolvedValue("vault:2");
    renderHook(() => useAutomaticRefresh(true), { wrapper });
    await flush();
    await flush();
    await flush();
    expect(fetch).toHaveBeenCalledTimes(1);
    act(() => finish(1));
    await previous;
    await flush();
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(client.getQueryData(queryKeys.home)).toBe(2);
  });

  it("閉じたqueryは取得せず古い扱いにし、直後に再表示したとき最新値を読む", async () => {
    vi.mocked(api.noteRevision).mockResolvedValue("vault:1");
    const graph = vi.fn(() => Promise.resolve(2));
    client.setQueryDefaults(queryKeys.graph, {
      queryFn: graph,
      staleTime: Infinity,
      meta: { refreshOnResume: true },
    });
    client.setQueryData(queryKeys.graph, 0);
    renderHook(() => useAutomaticRefresh(true), { wrapper });
    await flush();
    await flush();
    await flush();
    client.setQueryData(queryKeys.graph, 0);
    expect(client.getQueryState(queryKeys.graph)?.isInvalidated).toBe(false);
    vi.mocked(api.noteRevision).mockResolvedValue("vault:2");
    await act(() => vi.advanceTimersByTimeAsync(1_000));
    await flush();
    expect(client.getQueryState(queryKeys.graph)?.isInvalidated).toBe(true);
    expect(graph).not.toHaveBeenCalled();
    const observer = new QueryObserver(client, { queryKey: queryKeys.graph });
    subscriptions.push(observer.subscribe(() => {}));
    await flush();
    expect(graph).toHaveBeenCalledTimes(1);
    expect(client.getQueryData(queryKeys.graph)).toBe(2);
  });

  it("native非focusでは短周期probeを止め、復帰・手動更新でprobeとliveを読む", async () => {
    native.isFocused.mockResolvedValue(false);
    vi.mocked(api.noteRevision).mockResolvedValue("vault:1");
    const home = observe(queryKeys.home);
    const { result } = renderHook(() => useAutomaticRefresh(true), { wrapper });
    await act(() => vi.advanceTimersByTimeAsync(3_000));
    expect(api.noteRevision).not.toHaveBeenCalled();
    act(() => focus({ payload: true }));
    await flush();
    await flush();
    await flush();
    expect(api.noteRevision).toHaveBeenCalled();
    expect(home).toHaveBeenCalled();
    act(() => focus({ payload: false }));
    const probes = vi.mocked(api.noteRevision).mock.calls.length;
    await act(() => vi.advanceTimersByTimeAsync(3_000));
    expect(api.noteRevision).toHaveBeenCalledTimes(probes);
    act(() => focus({ payload: true }));
    await flush();
    await flush();
    const readsBeforeRetry = home.mock.calls.length;
    const probesBeforeRetry = vi.mocked(api.noteRevision).mock.calls.length;
    act(() => result.current.retry());
    await flush();
    expect(home).toHaveBeenCalledTimes(readsBeforeRetry + 1);
    expect(api.noteRevision).toHaveBeenCalledTimes(probesBeforeRetry + 1);
  });

  it("復帰時に変更前のprobeが残っていても、その完了後にtokenを取り直す", async () => {
    let finish: (value: string) => void = () => {};
    vi.mocked(api.noteRevision)
      .mockResolvedValue("vault:2")
      .mockImplementationOnce(() => new Promise<string>((resolve) => (finish = resolve)));
    const home = observe(queryKeys.home);
    renderHook(() => useAutomaticRefresh(true), { wrapper });
    await flush();
    act(() => focus({ payload: false }));
    await flush();
    act(() => focus({ payload: true }));
    await flush();
    expect(api.noteRevision).toHaveBeenCalledTimes(1);
    act(() => finish("vault:1"));
    await flush();
    await flush();
    expect(api.noteRevision).toHaveBeenCalledTimes(2);
    expect(client.getQueryData(queryKeys.noteRevision)).toBe("vault:2");
    expect(home).toHaveBeenCalled();
  });

  it("backgroundへ移った後は、待機中だったlive再取得も始めない", async () => {
    let finish: (value: number) => void = () => {};
    const fetch = vi
      .fn(() => Promise.resolve(2))
      .mockImplementationOnce(() => new Promise<number>((resolve) => (finish = resolve)));
    const observer = new QueryObserver(client, {
      queryKey: queryKeys.home,
      queryFn: fetch,
      initialData: 0,
      staleTime: Infinity,
      meta: { refreshOnResume: true },
    });
    subscriptions.push(observer.subscribe(() => {}));
    renderHook(() => useAutomaticRefresh(true), { wrapper });
    act(() => focus({ payload: true }));
    await flush();
    act(() => focus({ payload: true }));
    await flush();
    act(() => focus({ payload: false }));
    act(() => finish(1));
    await flush();
    expect(fetch).toHaveBeenCalledTimes(1);
    act(() => focus({ payload: true }));
    await flush();
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it("probe失敗を返し、失敗中も15秒fallbackで表示を取得できる", async () => {
    const failure = new Error("revision unavailable");
    vi.mocked(api.noteRevision).mockRejectedValue(failure);
    const home = vi.fn(() => Promise.resolve(2));
    const observer = new QueryObserver(client, {
      queryKey: queryKeys.home,
      queryFn: home,
      initialData: 0,
      staleTime: Infinity,
      ...liveQueryOptions,
    });
    subscriptions.push(observer.subscribe(() => {}));
    const { result } = renderHook(() => useAutomaticRefresh(true), { wrapper });
    await flush();
    await flush();
    expect(result.current.error).toBe(failure);
    expect(home).not.toHaveBeenCalled();
    await act(() => vi.advanceTimersByTimeAsync(15_000));
    expect(home).toHaveBeenCalledTimes(1);
    expect(client.getQueryData(queryKeys.home)).toBe(2);
    vi.mocked(api.noteRevision).mockResolvedValue("vault:1");
    act(() => result.current.retry());
    await flush();
    await flush();
    expect(result.current.error).toBeNull();
  });

  it("初期focus応答が遅れても後から届いた非表示通知を上書きしない", async () => {
    let initialFocus: (value: boolean) => void = () => {};
    native.isFocused.mockReturnValue(new Promise<boolean>((resolve) => (initialFocus = resolve)));
    vi.mocked(api.noteRevision).mockResolvedValue("vault:1");
    renderHook(() => useAutomaticRefresh(true), { wrapper });
    vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    act(() => initialFocus(true));
    await act(() => vi.advanceTimersByTimeAsync(3_000));
    expect(api.noteRevision).not.toHaveBeenCalled();
  });

  it("ready解除後のprobe応答を表示更新へ流さず、再開時には初回から確認する", async () => {
    let finish: (value: string) => void = () => {};
    vi.mocked(api.noteRevision).mockImplementationOnce(
      () => new Promise<string>((resolve) => (finish = resolve)),
    );
    const home = observe(queryKeys.home);
    const { rerender } = renderHook(({ ready }) => useAutomaticRefresh(ready), {
      initialProps: { ready: true },
      wrapper,
    });
    await flush();
    expect(api.noteRevision).toHaveBeenCalledTimes(1);
    rerender({ ready: false });
    act(() => finish("vault:1"));
    await act(() => vi.advanceTimersByTimeAsync(3_000));
    expect(home).not.toHaveBeenCalled();
    expect(api.noteRevision).toHaveBeenCalledTimes(1);
    vi.mocked(api.noteRevision).mockResolvedValue("vault:1");
    rerender({ ready: true });
    await flush();
    await flush();
    expect(api.noteRevision).toHaveBeenCalledTimes(2);
    expect(home).toHaveBeenCalledTimes(1);
  });

  // 2026-09-06: native復帰はvisibilitychangeだけでは届かず、手動更新が必要だった。
  it("native復帰とDOM通知をまとめ、購読中の読み取りだけを再取得する", async () => {
    const home = observe(queryKeys.home);
    const proposals = observe(queryKeys.proposalList);
    const disabled = observe(queryKeys.notes, { enabled: false });
    const auth = observe(queryKeys.githubAuth, { live: false });
    const maintenance = observe(queryKeys.maintenance, { live: false, staleTime: 60_000 });
    const inactive = vi.fn(() => Promise.resolve(1));
    client.setQueryDefaults(queryKeys.graph, {
      queryFn: inactive,
      meta: { refreshOnResume: true },
    });
    client.setQueryData(queryKeys.graph, 0);
    renderHook(() => useAutomaticRefresh(true), { wrapper });
    act(() => focus({ payload: false }));
    await flush();
    expect(home).not.toHaveBeenCalled();

    act(() => {
      focus({ payload: true });
      window.dispatchEvent(new Event("focus"));
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await flush();
    expect(home).toHaveBeenCalledTimes(1);
    expect(proposals).toHaveBeenCalledTimes(1);
    expect(client.getQueryData(queryKeys.home)).toBe(1);
    for (const fetch of [disabled, auth, maintenance, inactive]) {
      expect(fetch).not.toHaveBeenCalled();
    }
  });

  it("保守は60秒経過後だけ再開し、短いアプリ切替で繰り返さない", async () => {
    const maintenance = observe(queryKeys.maintenance, { live: false, staleTime: 60_000 });
    renderHook(() => useAutomaticRefresh(true), { wrapper });
    await act(() => vi.advanceTimersByTimeAsync(60_001));
    act(() => focus({ payload: true }));
    await flush();
    expect(maintenance).toHaveBeenCalledTimes(1);
    act(() => focus({ payload: true }));
    await flush();
    expect(maintenance).toHaveBeenCalledTimes(1);
  });

  // 2026-09-08: 復帰要求が変更前の応答へ吸収され、完了後さらに15秒待っていた。
  it("進行中の取得を中断せず、重なった復帰の後に1回だけ最新値を取得する", async () => {
    let complete: (value: number) => void = () => {};
    const fetch = vi
      .fn(() => Promise.resolve(2))
      .mockImplementationOnce(() => new Promise<number>((resolve) => (complete = resolve)));
    const observer = new QueryObserver(client, {
      queryKey: queryKeys.home,
      queryFn: fetch,
      initialData: 0,
      staleTime: Infinity,
      meta: { refreshOnResume: true },
    });
    subscriptions.push(observer.subscribe(() => {}));
    renderHook(() => useAutomaticRefresh(true), { wrapper });
    act(() => focus({ payload: true }));
    await flush();
    act(() => focus({ payload: true }));
    await flush();
    act(() => focus({ payload: true }));
    await flush();
    expect(fetch).toHaveBeenCalledTimes(1);
    act(() => complete(1));
    await flush();
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(client.getQueryData(queryKeys.home)).toBe(2);
  });

  it("オンボーディング中は開始せず、停止後の通知と予約を破棄する", async () => {
    const home = observe(queryKeys.home);
    const { rerender, unmount } = renderHook(({ ready }) => useAutomaticRefresh(ready), {
      initialProps: { ready: false },
      wrapper,
    });
    window.dispatchEvent(new Event("focus"));
    await flush();
    expect(home).not.toHaveBeenCalled();
    expect(native.listen).not.toHaveBeenCalled();
    rerender({ ready: true });
    act(() => focus({ payload: true }));
    unmount();
    focus({ payload: true });
    window.dispatchEvent(new Event("focus"));
    await flush();
    expect(home).not.toHaveBeenCalled();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it("停止後にnative購読が完了してもlistenerを残さない", async () => {
    let registered: (dispose: () => void) => void = () => {};
    native.listen.mockReturnValue(new Promise<() => void>((resolve) => (registered = resolve)));
    const { unmount } = renderHook(() => useAutomaticRefresh(true), { wrapper });
    unmount();
    act(() => registered(unlisten));
    await flush();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it("native購読が失敗してもDOM復帰で読み取りを更新できる", async () => {
    const home = observe(queryKeys.home);
    native.listen.mockRejectedValue(new Error("native listener unavailable"));
    const warning = vi.spyOn(console, "warn").mockImplementation(() => {});
    renderHook(() => useAutomaticRefresh(true), { wrapper });
    await act(() => window.dispatchEvent(new Event("focus")));
    await flush();
    expect(warning).toHaveBeenCalledTimes(1);
    expect(home).toHaveBeenCalledTimes(1);
  });

  it("非表示への遷移では再取得せず、表示復帰時に更新する", async () => {
    const home = observe(queryKeys.home);
    renderHook(() => useAutomaticRefresh(true), { wrapper });
    vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    document.dispatchEvent(new Event("visibilitychange"));
    await flush();
    expect(home).not.toHaveBeenCalled();
    vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
    document.dispatchEvent(new Event("visibilitychange"));
    await flush();
    expect(home).toHaveBeenCalledTimes(1);
  });
});
