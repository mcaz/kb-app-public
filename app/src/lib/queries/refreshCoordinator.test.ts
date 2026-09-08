import { QueryClient, QueryObserver } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createRefreshCoordinator } from "./refreshCoordinator";

let client: QueryClient;
let coordinator: ReturnType<typeof createRefreshCoordinator>;
let subscriptions: (() => void)[];

beforeEach(() => {
  vi.useFakeTimers();
  client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, staleTime: Infinity, refetchOnWindowFocus: false },
    },
  });
  coordinator = createRefreshCoordinator(client);
  subscriptions = [];
});

afterEach(() => {
  coordinator.dispose();
  subscriptions.forEach((unsubscribe) => unsubscribe());
  client.clear();
  vi.useRealTimers();
});

function deferred() {
  let resolve: (value: number) => void = () => {};
  let reject: (error: Error) => void = () => {};
  const promise = new Promise<number>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

function observe(key: string, fetch: () => Promise<number>) {
  const options = {
    queryKey: [key],
    queryFn: fetch,
    initialData: 0,
    meta: { refreshOnResume: true },
  };
  const observer = new QueryObserver(client, options);
  const unsubscribe = observer.subscribe(() => {});
  subscriptions.push(unsubscribe);
  return { observer, options, unsubscribe };
}

const refresh = () =>
  coordinator.refresh({
    type: "active",
    predicate: (query) => query.meta?.refreshOnResume === true,
  });
const flush = () => vi.advanceTimersByTimeAsync(0);

describe("refresh coordinator", () => {
  // 2026-09-08: 外部保存より前の取得へ復帰通知が吸収され、次の15秒周期まで古い値が残った。
  it("変更前から進行中の取得を待ち、複数要求を完了直後の1回にまとめる", async () => {
    const old = deferred();
    const fetch = vi.fn(() => Promise.resolve(2)).mockReturnValueOnce(old.promise);
    observe("home", fetch);
    const existing = client.refetchQueries({ queryKey: ["home"] });
    refresh();
    refresh();
    await flush();
    expect(fetch).toHaveBeenCalledTimes(1);
    old.resolve(1);
    await existing;
    await flush();
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(client.getQueryData(["home"])).toBe(2);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it("自身の取得中に届いた新しい要求も中断せず次の1回へまとめる", async () => {
    const first = deferred();
    const next = deferred();
    const fetch = vi.fn().mockReturnValueOnce(first.promise).mockReturnValueOnce(next.promise);
    observe("home", fetch);
    refresh();
    refresh();
    refresh();
    first.resolve(1);
    await flush();
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(client.getQueryData(["home"])).toBe(1);
    next.resolve(2);
    await flush();
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(client.getQueryData(["home"])).toBe(2);
  });

  it("遅いqueryが別のqueryの更新を待たせない", async () => {
    const slow = deferred();
    const home = vi.fn(() => Promise.resolve(2)).mockReturnValueOnce(slow.promise);
    const categories = vi.fn(() => Promise.resolve(2));
    observe("home", home);
    observe("categories", categories);
    const existing = client.refetchQueries({ queryKey: ["home"] });
    refresh();
    await flush();
    expect(home).toHaveBeenCalledTimes(1);
    expect(categories).toHaveBeenCalledTimes(1);
    expect(client.getQueryData(["categories"])).toBe(2);
    slow.resolve(1);
    await existing;
    await flush();
    expect(home).toHaveBeenCalledTimes(2);
  });

  it.each(["disabled", "inactive", "disposed", "removed"])(
    "待機中に%sになったqueryを再取得しない",
    async (state) => {
      const old = deferred();
      const fetch = vi.fn(() => Promise.resolve(2)).mockReturnValueOnce(old.promise);
      const { observer, options, unsubscribe } = observe("home", fetch);
      const existing = client.refetchQueries({ queryKey: ["home"] });
      refresh();
      if (state === "disabled") observer.setOptions({ ...options, enabled: false });
      if (state === "inactive") unsubscribe();
      if (state === "disposed") coordinator.dispose();
      if (state === "removed") client.removeQueries({ queryKey: ["home"] });
      old.resolve(1);
      await existing;
      await flush();
      expect(fetch).toHaveBeenCalledTimes(1);
    },
  );

  it("待っていた取得が失敗しても後続を1回実行し、最後の成功値を保つ", async () => {
    const old = deferred();
    const next = deferred();
    const fetch = vi.fn().mockReturnValueOnce(old.promise).mockReturnValueOnce(next.promise);
    observe("home", fetch);
    const existing = client.refetchQueries({ queryKey: ["home"] });
    refresh();
    old.reject(new Error("old read failed"));
    await existing;
    await flush();
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(client.getQueryData(["home"])).toBe(0);
    next.resolve(2);
    await flush();
    expect(client.getQueryData(["home"])).toBe(2);
  });

  it("失敗だけで再取得を繰り返さず、次の要求では再試行できる", async () => {
    const fetch = vi.fn(() => Promise.resolve(2)).mockRejectedValueOnce(new Error("read failed"));
    observe("home", fetch);
    refresh();
    await flush();
    expect(client.getQueryData(["home"])).toBe(0);
    expect(client.getQueryState(["home"])?.status).toBe("error");
    await vi.advanceTimersByTimeAsync(60_000);
    expect(fetch).toHaveBeenCalledTimes(1);
    refresh();
    await flush();
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(client.getQueryData(["home"])).toBe(2);
  });

  // 2026-09-08: 保守完了のinvalidateは古いPromiseを破棄し、新しい取得へ置き換える。
  it("無効化が先行取得を置き換えても、その最中に重複取得せず後続要求を保つ", async () => {
    const old = deferred();
    const replacement = deferred();
    const fetch = vi
      .fn(() => Promise.resolve(3))
      .mockReturnValueOnce(old.promise)
      .mockReturnValueOnce(replacement.promise);
    observe("home", fetch);
    const existing = client.refetchQueries({ queryKey: ["home"] });
    refresh();
    const invalidation = client.invalidateQueries({ queryKey: ["home"] });
    await flush();
    expect(fetch).toHaveBeenCalledTimes(2);
    old.resolve(1);
    await flush();
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(client.getQueryData(["home"])).toBe(0);
    replacement.resolve(2);
    await Promise.all([existing, invalidation]);
    await flush();
    expect(fetch).toHaveBeenCalledTimes(3);
    expect(client.getQueryData(["home"])).toBe(3);
  });
});
