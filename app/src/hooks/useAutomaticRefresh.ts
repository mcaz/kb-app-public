import { useQueryClient } from "@tanstack/react-query";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useCallback, useEffect, useRef, useState } from "react";

import { IN_TAURI } from "@/lib/api";
import { useNoteRevision } from "@/lib/queries";
import { queryKeys } from "@/lib/queries/keys";
import { createRefreshCoordinator } from "@/lib/queries/refreshCoordinator";

/** Webviewのvisibilityが変わらないアプリ切替も、nativeのfocus通知で拾う。 */
export function useAutomaticRefresh(enabled: boolean) {
  const client = useQueryClient();
  const [foreground, setForeground] = useState(
    () => !IN_TAURI && document.visibilityState === "visible",
  );
  const revision = useNoteRevision(enabled && foreground);
  const lastRevision = useRef<string | undefined>(undefined);
  const changed = useRef<(() => void) | undefined>(undefined);
  const retryRefresh = useRef<(() => void) | undefined>(undefined);
  const retry = useCallback(() => retryRefresh.current?.(), []);

  useEffect(() => {
    if (!enabled) return;
    let disposed = false;
    let pending: ReturnType<typeof setTimeout> | undefined;
    let unlisten: (() => void) | undefined;
    let nativeFocused: boolean | undefined;
    let focusObserved = false;
    let active = !IN_TAURI && document.visibilityState === "visible";
    let probeRequested = false;
    let maintenanceRequested = false;
    const reads = createRefreshCoordinator(client);

    const refresh = (probe: boolean, maintenance = false) => {
      if (disposed) return;
      probeRequested ||= probe;
      maintenanceRequested ||= maintenance;
      if (pending !== undefined) return;
      // 同時に届く変更検知・native・DOM通知をまとめ、保守は復帰時だけに限る。
      pending = setTimeout(() => {
        pending = undefined;
        const checkRevision = probeRequested;
        const runMaintenance = maintenanceRequested;
        probeRequested = false;
        maintenanceRequested = false;
        if (!active) return;
        reads.refresh({
          type: "active",
          predicate: (query) => active && query.meta?.refreshOnResume === true,
        });
        if (checkRevision) reads.refresh({ queryKey: queryKeys.noteRevision, type: "active" });
        // 同期・export等は読み取りと別の間隔を守り、短いアプリ切替で連続起動しない。
        if (runMaintenance) {
          void client.refetchQueries(
            { queryKey: queryKeys.maintenance, type: "active", stale: true },
            { cancelRefetch: false },
          );
        }
      }, 50);
    };
    changed.current = () => {
      // 閉じた画面も古い扱いにし、5秒のfresh期間内に戻っても変更前のcacheへ戻さない。
      void client.invalidateQueries({
        predicate: (query) => query.meta?.refreshOnResume === true,
        refetchType: "none",
      });
      refresh(false);
    };
    retryRefresh.current = () => refresh(true);
    const updateForeground = (focused: boolean) => {
      active = focused;
      setForeground(focused);
    };
    const onVisible = () => {
      focusObserved = true;
      const visible = document.visibilityState === "visible" && nativeFocused !== false;
      updateForeground(visible);
      if (visible) refresh(true, true);
    };
    window.addEventListener("focus", onVisible);
    document.addEventListener("visibilitychange", onVisible);
    if (IN_TAURI) {
      const currentWindow = getCurrentWindow();
      void currentWindow
        .onFocusChanged(({ payload: focused }) => {
          if (disposed) return;
          focusObserved = true;
          nativeFocused = focused;
          updateForeground(focused);
          if (focused) refresh(true, true);
        })
        .then((dispose) => {
          if (disposed) dispose();
          else unlisten = dispose;
        })
        .catch((error: unknown) => {
          // native購読に失敗しても、定期取得とDOMによる復帰確認は続けられる。
          console.warn("アプリ復帰の通知を購読できませんでした", error);
        });
      void currentWindow
        .isFocused()
        .then((focused) => {
          if (!disposed && !focusObserved) updateForeground(focused);
        })
        .catch((error: unknown) => {
          if (disposed || focusObserved) return;
          console.warn("アプリの表示状態を確認できませんでした", error);
          updateForeground(document.visibilityState === "visible");
        });
    } else {
      // オンボーディング中に表示状態が変わっていても、再開時の状態から始める。
      void Promise.resolve().then(() => {
        if (!disposed && !focusObserved) {
          updateForeground(document.visibilityState === "visible");
        }
      });
    }

    return () => {
      disposed = true;
      changed.current = undefined;
      retryRefresh.current = undefined;
      lastRevision.current = undefined;
      reads.dispose();
      if (pending !== undefined) clearTimeout(pending);
      unlisten?.();
      window.removeEventListener("focus", onVisible);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [client, enabled]);

  useEffect(() => {
    if (!enabled || !foreground || !revision.isSuccess || revision.data === undefined) return;
    // 初回も再取得し、起動時に並行して読んだ一覧との間の保存を取りこぼさない。
    if (lastRevision.current !== revision.data) {
      lastRevision.current = revision.data;
      changed.current?.();
    }
  }, [enabled, foreground, revision.data, revision.isSuccess]);

  return { error: enabled ? revision.error : null, isFetching: revision.isFetching, retry };
}
