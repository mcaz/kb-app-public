import { useEffect } from "react";

import { useSession } from "@/lib/stores/session";

type NavigationShortcutEvent = Pick<
  KeyboardEvent,
  "altKey" | "ctrlKey" | "key" | "metaKey" | "shiftKey"
>;

export function navigationDirection(
  event: NavigationShortcutEvent,
  isMac: boolean,
): "back" | "forward" | null {
  const modifierMatches = isMac
    ? event.metaKey && !event.altKey && !event.ctrlKey && !event.shiftKey
    : event.altKey && !event.metaKey && !event.ctrlKey && !event.shiftKey;
  if (!modifierMatches) return null;
  if (event.key === "ArrowLeft") return "back";
  if (event.key === "ArrowRight") return "forward";
  return null;
}

const isEditable = (target: EventTarget | null) =>
  target instanceof HTMLElement &&
  (target.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName));

/** WebView の履歴ではなく、セッション中に開いた画面とノートを辿る。 */
export function useNavigationHistoryShortcut(enabled = true) {
  useEffect(() => {
    if (!enabled) return;
    const isMac = navigator.platform.startsWith("Mac");

    const onKeyDown = (event: KeyboardEvent) => {
      const direction = navigationDirection(event, isMac);
      if (!direction || isEditable(event.target) || document.querySelector('[role="dialog"]'))
        return;
      event.preventDefault();
      const session = useSession.getState();
      if (direction === "back") session.goBack();
      else session.goForward();
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [enabled]);
}
