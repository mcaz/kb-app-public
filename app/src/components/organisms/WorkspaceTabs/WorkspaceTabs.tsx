import {
  ChevronLeft,
  ChevronRight,
  House,
  NotebookText,
  Paperclip,
  Plus,
  Waypoints,
  X,
} from "lucide-react";
import { useEffect, useRef, type KeyboardEvent } from "react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";
import { useMediaQuery } from "@/hooks/useMediaQuery";
import { usePrefs } from "@/lib/stores/prefs";
import { useSession, type View, type WorkspaceTab } from "@/lib/stores/session";

const viewIcons = {
  home: House,
  notes: NotebookText,
  files: Paperclip,
  graph: Waypoints,
} satisfies Record<View, typeof House>;

/** 開いている作業文脈を切り替え、各タブのページ履歴を独立して保つ。 */
export function WorkspaceTabs() {
  const { t } = useTranslation();
  const tabs = useSession((state) => state.tabs);
  const activeTabId = useSession((state) => state.activeTabId);
  const openTab = useSession((state) => state.openTab);
  const closeTab = useSession((state) => state.closeTab);
  const switchTab = useSession((state) => state.switchTab);
  const goBack = useSession((state) => state.goBack);
  const goForward = useSession((state) => state.goForward);
  const canGoBack = useSession((state) => state.backStack.length > 0);
  const canGoForward = useSession((state) => state.forwardStack.length > 0);
  const collapsed = usePrefs((state) => state.sideCollapsed);
  const forcedCompact = useMediaQuery("(max-width: 719px)");
  const tabListRef = useRef<HTMLDivElement | null>(null);
  const tabRefs = useRef<Array<HTMLButtonElement | null>>([]);

  useEffect(() => {
    const tabList = tabListRef.current;
    if (!tabList) return;

    const scrollHorizontally = (event: WheelEvent) => {
      const rawDelta =
        Math.abs(event.deltaX) > Math.abs(event.deltaY) ? event.deltaX : event.deltaY;
      if (rawDelta === 0) return;

      const scale = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? tabList.clientWidth : 1;
      const maxScrollLeft = tabList.scrollWidth - tabList.clientWidth;
      const nextScrollLeft = Math.max(
        0,
        Math.min(maxScrollLeft, tabList.scrollLeft + rawDelta * scale),
      );
      if (nextScrollLeft === tabList.scrollLeft) return;

      tabList.scrollLeft = nextScrollLeft;
      event.preventDefault();
    };

    tabList.addEventListener("wheel", scrollHorizontally, { passive: false });
    return () => tabList.removeEventListener("wheel", scrollHorizontally);
  }, []);

  useEffect(() => {
    const activeIndex = tabs.findIndex((tab) => tab.id === activeTabId);
    tabRefs.current[activeIndex]?.scrollIntoView({
      behavior: "smooth",
      block: "nearest",
      inline: "nearest",
    });
  }, [activeTabId, tabs]);

  const onKeyDown = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    const nextIndex =
      event.key === "Home"
        ? 0
        : event.key === "End"
          ? tabs.length - 1
          : (index + (event.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length;
    const next = tabs[nextIndex];
    if (!next) return;
    switchTab(next.id);
    tabRefs.current[nextIndex]?.focus();
  };

  return (
    <header className="bg-ground flex h-[52px] flex-none items-end">
      <div
        className={`flex h-full flex-none items-center gap-1.5 px-3 ${
          collapsed || forcedCompact
            ? "w-[52px] justify-center"
            : "w-[207px] max-[1040px]:w-[187px]"
        }`}
      >
        <span
          className="border-line bg-panel text-ink grid size-7 flex-none place-items-center rounded-lg border text-[11px] font-black tracking-[-0.04em]"
          aria-hidden="true"
        >
          kb
        </span>
        {!collapsed && !forcedCompact && (
          <>
            <strong className="mr-auto truncate text-[13px] tracking-[-0.01em]">kb-app</strong>
            <button
              type="button"
              className="text-muted hover:bg-panel-2 hover:text-ink grid size-7 cursor-pointer place-items-center rounded-md border-0 bg-transparent disabled:cursor-default disabled:opacity-25"
              aria-label={t("nav.back")}
              disabled={!canGoBack}
              onClick={goBack}
            >
              <Icon as={ChevronLeft} size="sm" />
            </button>
            <button
              type="button"
              className="text-muted hover:bg-panel-2 hover:text-ink grid size-7 cursor-pointer place-items-center rounded-md border-0 bg-transparent disabled:cursor-default disabled:opacity-25"
              aria-label={t("nav.forward")}
              disabled={!canGoForward}
              onClick={goForward}
            >
              <Icon as={ChevronRight} size="sm" />
            </button>
          </>
        )}
      </div>

      <div
        ref={tabListRef}
        className="workspace-tabs-scroll flex min-w-0 flex-1 items-end gap-1 overflow-x-auto"
        role="tablist"
        aria-label={t("nav.workspaces")}
      >
        {tabs.map((tab, index) => {
          const active = tab.id === activeTabId;
          const TabIcon = viewIcons[tab.view];
          return (
            <div
              key={tab.id}
              className={`group flex h-[42px] max-w-[210px] min-w-[128px] items-center rounded-t-xl border transition-colors max-[720px]:min-w-[112px] ${
                active
                  ? "border-line bg-panel text-ink border-b-0"
                  : "text-muted hover:bg-panel-2/70 hover:text-ink border-transparent"
              }`}
            >
              <button
                ref={(node) => {
                  tabRefs.current[index] = node;
                }}
                type="button"
                role="tab"
                aria-selected={active}
                aria-controls="workspace-panel"
                tabIndex={active ? 0 : -1}
                onClick={() => switchTab(tab.id)}
                onKeyDown={(event) => onKeyDown(event, index)}
                className="flex h-full min-w-0 flex-1 cursor-pointer items-center gap-2 border-0 bg-transparent py-0 pr-1 pl-3 text-inherit"
              >
                <Icon as={TabIcon} size="sm" className="flex-none" />
                <span className="min-w-0 flex-1 truncate text-left text-[12px] font-medium">
                  {tabTitle(tab, t)}
                </span>
              </button>
              {tabs.length > 1 && (
                <button
                  type="button"
                  className="hover:bg-sel mr-1 grid size-7 flex-none cursor-pointer place-items-center rounded-md border-0 bg-transparent text-inherit opacity-60 hover:opacity-100"
                  aria-label={t("nav.closeTab", { name: tabTitle(tab, t) })}
                  onClick={() => closeTab(tab.id)}
                >
                  <Icon as={X} size="sm" />
                </button>
              )}
            </div>
          );
        })}
      </div>
      <div className="flex h-full flex-none items-end pr-3 pl-1">
        <button
          type="button"
          className="text-muted hover:bg-panel-2 hover:text-ink mb-1 grid size-8 flex-none cursor-pointer place-items-center rounded-lg border-0 bg-transparent"
          aria-label={t("nav.newTab")}
          onClick={openTab}
        >
          <Icon as={Plus} />
        </button>
      </div>
    </header>
  );
}

function tabTitle(tab: WorkspaceTab, t: ReturnType<typeof useTranslation>["t"]) {
  if (tab.view === "notes") {
    const path = tab.selectedId ?? tab.selectedCategory;
    const name = path?.split("/").filter(Boolean).at(-1);
    return name || t("nav.notes");
  }
  return t(`nav.${tab.view}`);
}
