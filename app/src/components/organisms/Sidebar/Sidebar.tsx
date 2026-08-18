import {
  ChevronLeft,
  ChevronRight,
  House,
  NotebookText,
  Paperclip,
  Search,
  Settings,
  Sprout,
  Waypoints,
} from "lucide-react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/atoms/ui/tooltip";
import { useMediaQuery } from "@/hooks/useMediaQuery";
import { usePrefs } from "@/lib/stores/prefs";
import { useSession } from "@/lib/stores/session";

import { NavButton } from "./NavButton";
import { CategoryAccordion } from "./CategoryAccordion";

import type { NoteCategory } from "@/lib/api";

export interface SidebarProps {
  vaultName: string;
  categories: NoteCategory[];
  onOpenSearch: () => void;
  settingsOpen: boolean;
  onOpenSettings: () => void;
}

/** 左のナビ。畳むとアイコンだけになり、ホバーでラベルを吹き出す。 */
export function Sidebar({
  vaultName,
  categories,
  onOpenSearch,
  settingsOpen,
  onOpenSettings,
}: SidebarProps) {
  const { t } = useTranslation();
  const view = useSession((s) => s.view);
  const go = useSession((s) => s.go);
  const selectedCategory = useSession((s) => s.selectedCategory);
  const selectCategory = useSession((s) => s.selectCategory);
  const focusGraph = useSession((s) => s.focusGraph);
  const collapsed = usePrefs((s) => s.sideCollapsed);
  const setPrefs = usePrefs((s) => s.set);
  const forcedCompact = useMediaQuery("(max-width: 719px)");
  const visuallyCollapsed = collapsed || forcedCompact;

  return (
    <nav
      className={`border-line bg-panel-2 flex flex-none flex-col gap-0.5 border-r text-[13px] ${
        visuallyCollapsed
          ? "w-[52px] px-1.5 py-3.5"
          : "w-[176px] px-2.5 py-3.5 max-[1040px]:w-[156px]"
      }`}
    >
      <div
        className={`flex items-center gap-2 px-2.5 py-1.5 font-bold ${
          visuallyCollapsed ? "justify-center px-0" : ""
        }`}
      >
        <Icon as={Sprout} className="text-grow" />
        {!visuallyCollapsed && <span className="truncate">{vaultName}</span>}
      </div>

      <NavButton
        icon={Search}
        label={t("nav.search")}
        collapsed={visuallyCollapsed}
        active={false}
        onClick={onOpenSearch}
      />
      <NavButton
        icon={House}
        label={t("nav.home")}
        collapsed={visuallyCollapsed}
        active={view === "home"}
        onClick={() => go("home")}
      />

      {visuallyCollapsed ? (
        <NavButton
          icon={NotebookText}
          label={t("nav.notes")}
          collapsed
          active={view === "notes" && !settingsOpen}
          onClick={() => {
            go("notes");
            if (!forcedCompact) setPrefs({ sideCollapsed: false });
          }}
        />
      ) : (
        <CategoryAccordion
          categories={categories}
          active={view === "notes" && !settingsOpen}
          selectedCategory={selectedCategory}
          onActivate={() => go("notes")}
          onSelectCategory={selectCategory}
        />
      )}

      <NavButton
        icon={Paperclip}
        label={t("nav.files")}
        collapsed={visuallyCollapsed}
        active={view === "files"}
        onClick={() => go("files")}
      />

      <NavButton
        icon={Waypoints}
        label={t("nav.graph")}
        collapsed={visuallyCollapsed}
        active={view === "graph"}
        // ナビからは全体表示(局所グラフの中心を外す)
        onClick={() => focusGraph(null)}
      />
      <NavButton
        icon={Settings}
        label={t("nav.settings")}
        collapsed={visuallyCollapsed}
        active={settingsOpen}
        onClick={onOpenSettings}
      />
      {!forcedCompact && (
        <Tooltip>
          <TooltipTrigger asChild>
            <button
              type="button"
              className="text-muted hover:text-ink mt-auto flex cursor-pointer items-center gap-2 rounded-md border-none bg-transparent px-2.5 py-1.5 text-left text-[13px]"
              onClick={() => setPrefs({ sideCollapsed: !collapsed })}
              aria-label={collapsed ? t("nav.expand") : t("nav.collapse")}
            >
              <Icon as={collapsed ? ChevronRight : ChevronLeft} />
              {!collapsed && <span className="truncate">{t("nav.collapse")}</span>}
            </button>
          </TooltipTrigger>
          {collapsed && <TooltipContent side="right">{t("nav.expand")}</TooltipContent>}
        </Tooltip>
      )}
    </nav>
  );
}
