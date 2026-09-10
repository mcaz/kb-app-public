import { focusManager, onlineManager, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { api, type UpdateStatus } from "@/lib/api";
import { createQueryClient } from "@/lib/queryClient";

import { useAppUpdateAction, useAppUpdateStatus } from "./appUpdate";
import { queryKeys } from "./keys";

import type { ReactNode } from "react";

vi.mock("@/lib/api", () => ({
  api: {
    appUpdateStatus: vi.fn(),
    appUpdateCheck: vi.fn(),
    appUpdateDownload: vi.fn(),
    appUpdateInstall: vi.fn(),
  },
}));

const status = (fields: Partial<UpdateStatus> = {}): UpdateStatus => ({
  current_version: "0.0.1",
  phase: "idle",
  available_version: null,
  downloaded_bytes: 0,
  total_bytes: null,
  failure: null,
  ...fields,
});

const clients: ReturnType<typeof createQueryClient>[] = [];
function setup() {
  const client = createQueryClient();
  clients.push(client);
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  );
  return { client, wrapper };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

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
  vi.mocked(api.appUpdateStatus).mockResolvedValue(status());
});

afterEach(() => {
  cleanup();
  for (const client of clients.splice(0)) client.clear();
  focusManager.setFocused(undefined);
  onlineManager.setOnline(true);
  vi.useRealTimers();
  vi.resetAllMocks();
});

describe("更新の状態取得", () => {
  it("静止中は通信による確認やpollを開始せず、nativeの処理中だけ進捗を取得する", async () => {
    const { client, wrapper } = setup();
    renderHook(useAppUpdateStatus, { wrapper });
    await advance();
    await advance(2_000);
    expect(api.appUpdateStatus).toHaveBeenCalledTimes(1);
    expect(api.appUpdateCheck).not.toHaveBeenCalled();
    const downloading = status({ phase: "downloading", available_version: "0.0.2" });
    vi.mocked(api.appUpdateStatus).mockResolvedValue(downloading);
    act(() => {
      client.setQueryData(queryKeys.appUpdate, downloading);
    });
    await advance();
    await advance(550);
    expect(api.appUpdateStatus).toHaveBeenCalledTimes(2);
    vi.mocked(api.appUpdateStatus).mockResolvedValue(
      status({ phase: "ready", available_version: "0.0.2" }),
    );
    await advance(550);
    const completed = vi.mocked(api.appUpdateStatus).mock.calls.length;
    await advance(2_000);
    expect(api.appUpdateStatus).toHaveBeenCalledTimes(completed);
    expect(client.getQueryData<UpdateStatus>(queryKeys.appUpdate)?.phase).toBe("ready");
  });

  it("長いdownload応答の待機中もpollし、古いpoll応答で検証済み結果を上書きしない", async () => {
    const { client, wrapper } = setup();
    const download = deferred<UpdateStatus>();
    const oldPoll = deferred<UpdateStatus>();
    vi.mocked(api.appUpdateDownload).mockReturnValue(download.promise);
    const { result } = renderHook(
      () => ({ query: useAppUpdateStatus(), action: useAppUpdateAction() }),
      { wrapper },
    );
    await advance();
    vi.mocked(api.appUpdateStatus).mockReturnValueOnce(oldPoll.promise);
    act(() => result.current.action.mutate("download"));
    await advance(550);
    expect(api.appUpdateStatus).toHaveBeenCalledTimes(2);
    const ready = status({ phase: "ready", available_version: "0.0.2" });
    vi.mocked(api.appUpdateStatus).mockResolvedValue(ready);
    await act(async () => {
      download.resolve(ready);
      await download.promise;
    });
    await advance();
    await act(async () => {
      oldPoll.resolve(status({ phase: "downloading" }));
      await oldPoll.promise;
    });
    await advance();
    expect(client.getQueryData(queryKeys.appUpdate)).toEqual(ready);
    expect(api.appUpdateInstall).not.toHaveBeenCalled();
  });

  it("install失敗を自動再試行せず、nativeの再試行位置と理由を保持する", async () => {
    const { client, wrapper } = setup();
    const failed = status({
      phase: "ready",
      available_version: "0.0.2",
      failure: "install_failed",
    });
    vi.mocked(api.appUpdateInstall).mockResolvedValue(failed);
    vi.mocked(api.appUpdateStatus).mockResolvedValue(failed);
    const { result } = renderHook(
      () => ({ query: useAppUpdateStatus(), action: useAppUpdateAction() }),
      { wrapper },
    );
    await advance();
    act(() => result.current.action.mutate("install"));
    await advance();
    await advance(2_000);
    expect(api.appUpdateInstall).toHaveBeenCalledTimes(1);
    expect(client.getQueryData(queryKeys.appUpdate)).toEqual(failed);
  });

  it("設定画面を閉じても操作結果を残し、再度開いた時にnative状態を照合する", async () => {
    const { client, wrapper } = setup();
    const download = deferred<UpdateStatus>();
    vi.mocked(api.appUpdateDownload).mockReturnValue(download.promise);
    const first = renderHook(
      () => ({ query: useAppUpdateStatus(), action: useAppUpdateAction() }),
      { wrapper },
    );
    await advance();
    act(() => first.result.current.action.mutate("download"));
    await advance();
    first.unmount();
    const ready = status({ phase: "ready", available_version: "0.0.2" });
    await act(async () => {
      download.resolve(ready);
      await download.promise;
    });
    await advance();
    expect(client.getQueryData(queryKeys.appUpdate)).toEqual(ready);
    const unavailable = status({ phase: "unavailable", failure: "not_configured" });
    vi.mocked(api.appUpdateStatus).mockResolvedValue(unavailable);
    const second = renderHook(useAppUpdateStatus, { wrapper });
    await advance();
    expect(second.result.current.data).toEqual(unavailable);
  });
});
