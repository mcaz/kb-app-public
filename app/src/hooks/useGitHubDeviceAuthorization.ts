import { useCallback, useEffect, useState } from "react";

import { IN_TAURI } from "@/lib/api";
import { events, type GitHubDeviceAuthorization } from "@/lib/bindings";

/** OAuth device flow で入力する短い code。token/device_code はイベントに含まれない。 */
export function useGitHubDeviceAuthorization() {
  const [authorization, setAuthorization] = useState<GitHubDeviceAuthorization | null>(null);

  useEffect(() => {
    if (!IN_TAURI) return;
    let dispose: (() => void) | undefined;
    let cancelled = false;
    void events.gitHubDeviceAuthorization
      .listen((event) => setAuthorization(event.payload))
      .then((unlisten) => {
        if (cancelled) unlisten();
        else dispose = unlisten;
      });
    return () => {
      cancelled = true;
      dispose?.();
    };
  }, []);

  return {
    authorization,
    clear: useCallback(() => setAuthorization(null), []),
  };
}
