import { useEffect } from "react";

type SettingsShortcutEvent = Pick<
  KeyboardEvent,
  "altKey" | "ctrlKey" | "key" | "metaKey" | "shiftKey"
>;

export function matchesSettingsShortcut(event: SettingsShortcutEvent): boolean {
  if (event.altKey || event.shiftKey) return false;
  return (event.metaKey || event.ctrlKey) && event.key === ",";
}

/** macOS の Cmd+, / Windows・Linux の Ctrl+, で設定 Modal を開く。 */
export function useSettingsShortcut(onOpen: () => void, enabled = true) {
  useEffect(() => {
    if (!enabled) return;

    const onKeyDown = (event: KeyboardEvent) => {
      if (!matchesSettingsShortcut(event)) return;
      event.preventDefault();
      onOpen();
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [enabled, onOpen]);
}
