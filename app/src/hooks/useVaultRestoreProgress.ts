import { useEffect, useState } from "react";

import { IN_TAURI } from "@/lib/api";
import { events, type VaultRestoreProgress } from "@/lib/bindings";

/** 別端末のVaultを検査し、Full Artifactまで復元する進み具合。 */
export function useVaultRestoreProgress(): VaultRestoreProgress | null {
  const [progress, setProgress] = useState<VaultRestoreProgress | null>(null);

  useEffect(() => {
    if (!IN_TAURI) return;
    let dispose: (() => void) | undefined;
    let cancelled = false;

    void events.vaultRestoreProgress
      .listen((event) => setProgress(event.payload))
      .then((unlisten) => {
        if (cancelled) unlisten();
        else dispose = unlisten;
      });

    return () => {
      cancelled = true;
      dispose?.();
    };
  }, []);

  return progress;
}
