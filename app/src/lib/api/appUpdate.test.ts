import { describe, expect, it, vi } from "vitest";

import { api, IN_TAURI } from "./index";

vi.mock("@/lib/bindings", () => ({
  commands: {
    appUpdateStatus: vi.fn(),
    appUpdateCheck: vi.fn(),
    appUpdateDownload: vi.fn(),
    appUpdateInstall: vi.fn(),
    appUpdateBootReady: vi.fn(),
  },
}));

describe("ブラウザ表示の更新境界", () => {
  it.each(["appUpdateStatus", "appUpdateCheck", "appUpdateDownload", "appUpdateInstall"] as const)(
    "%sは成功を模擬せず利用不可を返す",
    async (operation) => {
      expect(IN_TAURI).toBe(false);
      expect(await api[operation]()).toEqual({
        current_version: "0.0.1",
        phase: "unavailable",
        available_version: null,
        downloaded_bytes: 0,
        total_bytes: null,
        failure: "unsupported_platform",
      });
    },
  );
});
