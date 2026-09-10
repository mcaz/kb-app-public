import type { SetupState, UpdatePhase, UpdateStatus } from "@/lib/api";

export function isAppUpdateBusy(phase: UpdatePhase | undefined): boolean {
  return (
    phase === "checking" ||
    phase === "downloading" ||
    phase === "verifying" ||
    phase === "installing"
  );
}

/** readyはnativeの検証結果だけを使い、受信率や版の存在から補わない。 */
export function appUpdateActions(status: UpdateStatus | undefined) {
  if (!status || status.phase === "unavailable" || isAppUpdateBusy(status.phase)) {
    return { check: false, download: false, install: false };
  }
  const versionAvailable = Boolean(status.available_version);
  return {
    check: true,
    download:
      status.phase === "available" &&
      versionAvailable &&
      status.failure !== "incompatible" &&
      status.failure !== "restart_failed",
    install:
      status.phase === "ready" &&
      versionAvailable &&
      (status.failure === null ||
        status.failure === "install_failed" ||
        status.failure === "storage"),
  };
}

export function appUpdateProgress(status: UpdateStatus): number | null {
  const { downloaded_bytes: downloaded, total_bytes: total } = status;
  if (total === null || !Number.isFinite(total) || total <= 0 || !Number.isFinite(downloaded)) {
    return null;
  }
  return Math.min(100, Math.max(0, Math.round((downloaded / total) * 100)));
}

/** 初期画面のデータ取得の受領条件。古いcacheに値があっても取得失敗を成功扱いしない。 */
export function appUpdateBootReadyAllowed({
  setup,
  setupFailed,
  homeLoaded,
  homeFailed,
  categoriesLoaded,
  categoriesFailed,
}: {
  setup: SetupState | undefined;
  setupFailed: boolean;
  homeLoaded: boolean;
  homeFailed: boolean;
  categoriesLoaded: boolean;
  categoriesFailed: boolean;
}): boolean {
  return (
    setup !== undefined &&
    !setupFailed &&
    !homeFailed &&
    !categoriesFailed &&
    (setup.needs_onboarding || (homeLoaded && categoriesLoaded))
  );
}
