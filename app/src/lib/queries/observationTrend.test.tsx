import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { api } from "@/lib/api";

import { queryKeys, useObservationTrend } from "./index";

import type { ReactNode } from "react";
import type { ObservationTrend, ObservationTrendFilter } from "@/lib/api";

const boundaries = vi.hoisted(() => Array.from({ length: 15 }, (_, day) => day * 86_400_000));
vi.mock("@/hooks/useLocalDayBoundaries", () => ({ useLocalDayBoundaries: () => boundaries }));
vi.mock("@/lib/api", () => ({ api: { homeObservationTrend: vi.fn(), settingsGet: vi.fn() } }));

beforeEach(() => {
  vi.mocked(api.settingsGet).mockResolvedValue({});
});

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

describe("useObservationTrend", () => {
  it("切替前の遅い応答を選択中の集計へ混ぜず、対象ごとのキャッシュを保持する", async () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const pending = new Map<ObservationTrendFilter, (data: ObservationTrend) => void>();
    vi.mocked(api.homeObservationTrend).mockImplementation(
      (_, filter) => new Promise((resolve) => pending.set(filter, resolve)),
    );
    const wrapper = ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={client}>{children}</QueryClientProvider>
    );
    const { result, rerender } = renderHook(({ filter }) => useObservationTrend(filter), {
      initialProps: { filter: "all" as ObservationTrendFilter },
      wrapper,
    });
    const all: ObservationTrend = { status: "available", days: [] };
    const claude: ObservationTrend = { status: "no_observations", days: [] };

    await waitFor(() => expect(pending.has("all")).toBe(true));
    rerender({ filter: "claude" });
    await waitFor(() => expect(pending.has("claude")).toBe(true));
    await act(async () => {
      pending.get("all")!(all);
      await Promise.resolve();
    });
    expect(result.current.isPending).toBe(true);
    expect(result.current.data).toBeUndefined();
    await act(async () => {
      pending.get("claude")!(claude);
      await Promise.resolve();
    });
    await waitFor(() => expect(result.current.data).toEqual(claude));
    expect(client.getQueryData(queryKeys.observationTrend(boundaries, "all"))).toEqual(all);
    expect(client.getQueryData(queryKeys.observationTrend(boundaries, "claude"))).toEqual(claude);
    expect(client.getQueryData(queryKeys.observationTrend(boundaries, "gpt"))).toBeUndefined();

    // 設定変更時の既存prefix無効化は、非表示になった対象も再取得の対象に残す。
    await client.invalidateQueries({ queryKey: queryKeys.observationHealth, refetchType: "none" });
    expect(client.getQueryState(queryKeys.observationTrend(boundaries, "all"))?.isInvalidated).toBe(
      true,
    );
    expect(
      client.getQueryState(queryKeys.observationTrend(boundaries, "claude"))?.isInvalidated,
    ).toBe(true);
    client.clear();
  });

  it.each([
    [{ ai_kb_enabled: false }, "disabled"],
    [{ claude_kb_enabled: false, gpt_kb_enabled: false }, "disabled"],
    [{ claude_kb_enabled: false, gpt_kb_enabled: true }, "available"],
    [{ claude_kb_enabled: true, gpt_kb_enabled: false }, "available"],
  ] as const)(
    "OFFの設定%jを接続元切替にも反映し、片側OFFの履歴は保つ",
    async (settings, status) => {
      const client = new QueryClient({
        defaultOptions: { queries: { retry: false, staleTime: Infinity } },
      });
      const snapshot: ObservationTrend = {
        status: "available",
        days: [
          {
            start_ms: 0,
            end_ms: 1,
            hook_output_emitted: 3,
            propose_successes: 1,
            update_successes: 0,
            errors: 0,
          },
        ],
      };
      client.setQueryData(queryKeys.settings, {});
      for (const filter of ["all", "claude", "gpt"] as const) {
        client.setQueryData(queryKeys.observationTrend(boundaries, filter), snapshot);
      }
      vi.mocked(api.homeObservationTrend).mockResolvedValue(snapshot);
      const wrapper = ({ children }: { children: ReactNode }) => (
        <QueryClientProvider client={client}>{children}</QueryClientProvider>
      );
      const { result, rerender } = renderHook(({ filter }) => useObservationTrend(filter), {
        initialProps: { filter: "claude" as ObservationTrendFilter },
        wrapper,
      });
      expect(result.current.data?.status).toBe("available");
      // 2026-09-06: invalidateだけでは非表示の対象にON時の成功データが残っていた。
      await act(async () => {
        client.setQueryData(queryKeys.settings, settings);
        await client.invalidateQueries({
          queryKey: queryKeys.observationHealth,
          refetchType: "none",
        });
      });
      await waitFor(() => expect(result.current.data?.status).toBe(status));
      for (const filter of ["all", "gpt", "claude"] as const) {
        rerender({ filter });
        if (status === "disabled") expect(result.current.data?.status).not.toBe("available");
        await waitFor(() => expect(result.current.data?.status).toBe(status));
      }
      if (status === "disabled") expect(api.homeObservationTrend).not.toHaveBeenCalled();
      client.clear();
    },
  );
});
