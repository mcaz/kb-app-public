import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { api } from "@/lib/api";
import { queryKeys, useOnboard, useOnboardExisting } from "./index";

afterEach(() => vi.restoreAllMocks());

// 2026-09-08: setupの更新で初回画面が外れても、次の接続画面への遷移を落とさない。
describe("onboarding handoff", () => {
  it.each(["create", "restore"] as const)(
    "%sの成功を再取得より前に渡し、作成済みのサーバ状態を残す",
    async (mode) => {
      const setup = { needs_onboarding: false, vault_name: "fixture", vault_path: "/fixture" };
      vi.spyOn(api, "onboard").mockResolvedValue(setup);
      vi.spyOn(api, "onboardExisting").mockResolvedValue(setup);
      const client = new QueryClient();
      const onReady = vi.fn();
      const { result, unmount } = renderHook(
        () => ({ create: useOnboard(onReady), restore: useOnboardExisting(onReady) }),
        {
          wrapper: ({ children }) => (
            <QueryClientProvider client={client}>{children}</QueryClientProvider>
          ),
        },
      );
      vi.spyOn(client, "invalidateQueries").mockImplementation(async () => {
        expect(onReady).toHaveBeenCalledOnce();
        expect(client.getQueryData(queryKeys.setup)).toEqual(setup);
        unmount();
        await Promise.resolve();
      });
      await act(async () => {
        if (mode === "create") await result.current.create.mutateAsync();
        else await result.current.restore.mutateAsync("https://github.com/example/fixture.git");
      });
      expect(onReady).toHaveBeenCalledOnce();
      client.clear();
    },
  );

  it.each(["create", "restore"] as const)("%sの失敗では接続へ進めない", async (mode) => {
    const failure = new Error("fixture failure");
    vi.spyOn(api, "onboard").mockRejectedValue(failure);
    vi.spyOn(api, "onboardExisting").mockRejectedValue(failure);
    const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
    const onReady = vi.fn();
    const { result, unmount } = renderHook(
      () => ({ create: useOnboard(onReady), restore: useOnboardExisting(onReady) }),
      {
        wrapper: ({ children }) => (
          <QueryClientProvider client={client}>{children}</QueryClientProvider>
        ),
      },
    );
    await act(async () => {
      const operation =
        mode === "create"
          ? result.current.create.mutateAsync()
          : result.current.restore.mutateAsync("https://github.com/example/fixture.git");
      await expect(operation).rejects.toBe(failure);
    });
    expect(onReady).not.toHaveBeenCalled();
    expect(client.getQueryData(queryKeys.setup)).toBeUndefined();
    unmount();
    client.clear();
  });
});
