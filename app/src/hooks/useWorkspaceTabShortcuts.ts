import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { useErrorText } from "@/hooks/useErrorText";
import { api, IN_TAURI } from "@/lib/api";
import { events } from "@/lib/bindings";
import { useSession } from "@/lib/stores/session";

type TabAction = "new" | "close" | "next" | "previous";
type ShortcutEvent = Pick<
  KeyboardEvent,
  "key" | "altKey" | "ctrlKey" | "metaKey" | "shiftKey" | "isComposing" | "repeat"
>;

export function workspaceTabAction(
  event: ShortcutEvent,
  isMac: boolean,
  nativeMenu: boolean,
): TabAction | null {
  if (event.isComposing || event.altKey) return null;
  if (event.key === "Tab" && event.ctrlKey && !event.metaKey) {
    return event.shiftKey ? "previous" : "next";
  }
  if (event.shiftKey || event.repeat) return null;
  const modifierMatches = isMac ? event.metaKey && !event.ctrlKey : event.ctrlKey && !event.metaKey;
  // macOSのメニューがCmd+N/Wを所有する。DOMにも届いても一操作を二度実行しない。
  if (!modifierMatches || (isMac && nativeMenu)) return null;
  if (event.key.toLowerCase() === "n") return "new";
  if (event.key.toLowerCase() === "w") return "close";
  return null;
}

/** タブの状態は既存storeに集約し、最後の1枚だけ常駐ウィンドウの操作へ渡す。 */
export function useWorkspaceTabShortcuts(enabled = true) {
  const { t, i18n } = useTranslation("common");
  const errorText = useErrorText();

  useEffect(() => {
    if (!enabled) return;
    const isMac = navigator.platform.startsWith("Mac");
    const nativeMenu = IN_TAURI && isMac;
    let composing = false;
    let cancelled = false;
    let dispose: (() => void) | undefined;
    const reportError = (error: unknown) => toast.error(errorText(error));
    const blocked = () =>
      composing || document.querySelector('[role="dialog"], [role="alertdialog"]') !== null;

    const run = (action: TabAction) => {
      if (cancelled || blocked()) return;
      const session = useSession.getState();
      if (action === "new") session.openTab();
      else if (action === "close") {
        if (session.tabs.length > 1) session.closeTab(session.activeTabId);
        else void api.windowHide().catch(reportError);
      } else {
        const index = session.tabs.findIndex((tab) => tab.id === session.activeTabId);
        if (index < 0 || session.tabs.length < 2) return;
        const offset = action === "next" ? 1 : -1;
        const next = session.tabs[(index + offset + session.tabs.length) % session.tabs.length];
        if (next) session.switchTab(next.id);
      }
    };
    const onKeyDown = (event: KeyboardEvent) => {
      const action = workspaceTabAction(event, isMac, nativeMenu);
      if (!action || event.defaultPrevented || blocked()) return;
      event.preventDefault();
      run(action);
    };
    const onCompositionStart = () => {
      composing = true;
    };
    const onCompositionEnd = () => {
      composing = false;
    };
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("compositionstart", onCompositionStart);
    window.addEventListener("compositionend", onCompositionEnd);

    if (nativeMenu) {
      void events.workspaceTabShortcut
        .listen((event) => run(event.payload))
        .then(async (unlisten) => {
          if (cancelled) {
            unlisten();
            return;
          }
          dispose = unlisten;
          await api.workspaceTabShortcutsConfigure(
            true,
            t("nav.tabMenu"),
            t("nav.newTab"),
            t("nav.closeCurrentTab"),
          );
        })
        .catch(reportError);
    }

    return () => {
      cancelled = true;
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("compositionstart", onCompositionStart);
      window.removeEventListener("compositionend", onCompositionEnd);
      dispose?.();
      if (nativeMenu) {
        void api
          .workspaceTabShortcutsConfigure(
            false,
            t("nav.tabMenu"),
            t("nav.newTab"),
            t("nav.closeCurrentTab"),
          )
          .catch(reportError);
      }
    };
  }, [enabled, errorText, t, i18n.language]);
}
