import { describe, expect, it } from "vitest";

import type { UpdateFailureKind, UpdatePhase, UpdateStatus } from "@/lib/api";

import {
  appUpdateActions,
  appUpdateBootReadyAllowed,
  appUpdateProgress,
  isAppUpdateBusy,
} from "./appUpdatePolicy";

const status = (fields: Partial<UpdateStatus> = {}): UpdateStatus => ({
  current_version: "0.0.1",
  phase: "available",
  available_version: "0.0.2",
  downloaded_bytes: 100,
  total_bytes: 100,
  failure: null,
  ...fields,
});

describe("更新操作の表示条件", () => {
  it("受信100%でもnativeの検証完了前には適用を提示しない", () => {
    for (const phase of ["available", "downloading", "verifying"] as const) {
      const current = status({ phase });
      expect(appUpdateProgress(current)).toBe(100);
      expect(appUpdateActions(current).install).toBe(false);
    }
    expect(appUpdateActions(status({ phase: "ready" })).install).toBe(true);
    expect(appUpdateActions(status({ phase: "ready", available_version: null })).install).toBe(
      false,
    );
  });

  it("利用不可と検証失敗では適用を提示せず、対応した位置からだけ再試行する", () => {
    expect(appUpdateActions(undefined)).toEqual({ check: false, download: false, install: false });
    expect(appUpdateActions(status({ phase: "unavailable" }))).toEqual({
      check: false,
      download: false,
      install: false,
    });
    for (const failure of [
      "signature",
      "invalid_package",
      "incompatible",
      "not_configured",
      "unsupported_platform",
      "restart_failed",
    ] satisfies UpdateFailureKind[]) {
      expect(appUpdateActions(status({ phase: "ready", failure })).install).toBe(false);
    }
    expect(appUpdateActions(status({ failure: "network" })).download).toBe(true);
    expect(appUpdateActions(status({ failure: "incompatible" })).download).toBe(false);
    expect(appUpdateActions(status({ phase: "ready", failure: "storage" })).install).toBe(true);
    expect(appUpdateActions(status({ phase: "ready", failure: "install_failed" })).install).toBe(
      true,
    );
    expect(appUpdateActions(status({ phase: "ready", failure: "restart_failed" }))).toEqual({
      check: true,
      download: false,
      install: false,
    });
    expect(appUpdateActions(status({ failure: "restart_failed" })).download).toBe(false);
  });

  it("処理中は別の操作を提示せず、静止状態はpoll対象にしない", () => {
    const phases: UpdatePhase[] = [
      "unavailable",
      "idle",
      "checking",
      "up_to_date",
      "available",
      "downloading",
      "verifying",
      "ready",
      "installing",
    ];
    const busy = ["checking", "downloading", "verifying", "installing"];
    for (const phase of phases) {
      expect(isAppUpdateBusy(phase)).toBe(busy.includes(phase));
      if (isAppUpdateBusy(phase)) {
        expect(appUpdateActions(status({ phase }))).toEqual({
          check: false,
          download: false,
          install: false,
        });
      }
    }
  });

  it("総量不明を100%と扱わず、表示値を0〜100%へ収める", () => {
    for (const total_bytes of [null, 0, -1, Number.NaN, Infinity]) {
      expect(appUpdateProgress(status({ total_bytes }))).toBeNull();
    }
    expect(appUpdateProgress(status({ downloaded_bytes: Number.NaN }))).toBeNull();
    expect(appUpdateProgress(status({ downloaded_bytes: -5 }))).toBe(0);
    expect(appUpdateProgress(status({ downloaded_bytes: 150 }))).toBe(100);
    expect(appUpdateProgress(status({ downloaded_bytes: 33 }))).toBe(33);
  });
});

describe("更新後の初期画面受領条件", () => {
  const loaded = {
    setup: { needs_onboarding: false, vault_name: "fixture", vault_path: "/fixture" },
    setupFailed: false,
    homeLoaded: true,
    homeFailed: false,
    categoriesLoaded: true,
    categoriesFailed: false,
  };

  it("既存KBは初期データが全て揃った時だけ通知する", () => {
    expect(appUpdateBootReadyAllowed(loaded)).toBe(true);
    expect(appUpdateBootReadyAllowed({ ...loaded, setup: undefined })).toBe(false);
    expect(appUpdateBootReadyAllowed({ ...loaded, homeLoaded: false })).toBe(false);
    expect(appUpdateBootReadyAllowed({ ...loaded, categoriesLoaded: false })).toBe(false);
  });

  it("cacheに値が残っていても、いずれかの取得失敗中には通知しない", () => {
    for (const key of ["setupFailed", "homeFailed", "categoriesFailed"] as const) {
      expect(appUpdateBootReadyAllowed({ ...loaded, [key]: true })).toBe(false);
    }
  });

  it("初回画面はsetup成功だけで表示でき、未作成KBのqueryを要求しない", () => {
    expect(
      appUpdateBootReadyAllowed({
        ...loaded,
        setup: { needs_onboarding: true, vault_name: null, vault_path: null },
        homeLoaded: false,
        categoriesLoaded: false,
      }),
    ).toBe(true);
  });
});
