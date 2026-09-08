import { QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, render, screen } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { setupI18n } from "@/i18n";
import { api, type AppBootMode } from "@/lib/api";
import { createQueryClient } from "@/lib/queryClient";

import { AppEntry } from "./AppEntry";

const normalApp = vi.hoisted(() => vi.fn());
vi.mock("@/App", () => ({
  App: () => {
    normalApp();
    return <div data-testid="normal-app" />;
  },
}));
vi.mock("@/pages/StorageRecoveryPage", () => ({
  StorageRecoveryPage: () => <div data-testid="recovery-app" />,
}));
vi.mock(import("@/lib/api"), async (original) => {
  const actual = await original();
  return { ...actual, api: { ...actual.api, appBootMode: vi.fn() } };
});

beforeEach(() => {
  setupI18n("ja");
  vi.clearAllMocks();
});
afterEach(cleanup);

function show() {
  return render(
    <StrictMode>
      <QueryClientProvider client={createQueryClient()}>
        <AppEntry />
      </QueryClientProvider>
    </StrictMode>,
  );
}

describe("startup mode isolation", () => {
  // 2026-09-07: 通常AppのmountだけでDB・保守が始まるため、mode未確認ではmountしない。
  it("起動モードの応答前と復旧モード確定後は通常Appを一度もmountしない", async () => {
    let resolve!: (mode: AppBootMode) => void;
    const pending = new Promise<AppBootMode>((done) => {
      resolve = done;
    });
    vi.mocked(api.appBootMode).mockReturnValue(pending);
    show();
    expect(screen.getByRole("status")).toHaveTextContent("起動方法を確認しています");
    expect(normalApp).not.toHaveBeenCalled();
    await act(async () => {
      resolve("storage_recovery");
      await pending;
    });
    expect(await screen.findByTestId("recovery-app")).toBeInTheDocument();
    expect(normalApp).not.toHaveBeenCalled();
    expect(api.appBootMode).toHaveBeenCalledOnce();
  });

  it("起動モード取得失敗で通常モードへfallbackしない", async () => {
    vi.mocked(api.appBootMode).mockRejectedValue(new Error("private backend detail"));
    show();
    expect(await screen.findByRole("alert")).toHaveTextContent("起動方法を確認できませんでした");
    expect(normalApp).not.toHaveBeenCalled();
    expect(screen.queryByText(/private/)).not.toBeInTheDocument();
    expect(api.appBootMode).toHaveBeenCalledOnce();
  });

  it("通常モードを確認できたときだけ通常Appをmountする", async () => {
    vi.mocked(api.appBootMode).mockResolvedValue("normal");
    show();
    expect(await screen.findByTestId("normal-app")).toBeInTheDocument();
    expect(screen.queryByTestId("recovery-app")).not.toBeInTheDocument();
  });
});
