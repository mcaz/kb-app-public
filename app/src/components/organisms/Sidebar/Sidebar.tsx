import {
  ChevronLeft,
  ChevronRight,
  House,
  NotebookText,
  Plug,
  Search,
  Settings,
  Sprout,
  Waypoints,
  type LucideIcon,
} from "lucide-react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/atoms/ui/tooltip";
import { useMediaQuery } from "@/hooks/useMediaQuery";
import { usePrefs } from "@/lib/stores/prefs";
import { useSession, type View } from "@/lib/stores/session";

import { NavButton } from "./NavButton";

export interface SidebarProps {
  vaultName: string;
  onOpenSearch: () => void;
}

/** 左のナビ。畳むとアイコンだけになり、ホバーでラベルを吹き出す。 */
export function Sidebar({ vaultName, onOpenSearch }: SidebarProps) {
  const { t } = useTranslation();
  const view = useSession((s) => s.view);
  const go = useSession((s) => s.go);
  const resetNotes = useSession((s) => s.resetNotes);
  const focusGraph = useSession((s) => s.focusGraph);
  const collapsed = usePrefs((s) => s.sideCollapsed);
  const setPrefs = usePrefs((s) => s.set);
  const forcedCompact = useMediaQuery("(max-width: 719px)");
  const visuallyCollapsed = collapsed || forcedCompact;

  const items: { view: View; icon: LucideIcon; label: string; onClick: () => void }[] = [
    { view: "home", icon: House, label: t("nav.home"), onClick: () => go("home") },
    // ナビの「ノート」は検索条件と選択を解除した本文画面へ戻す
    { view: "notes", icon: NotebookText, label: t("nav.notes"), onClick: resetNotes },
    // ナビからは全体表示(局所グラフの中心を外す)
    { view: "graph", icon: Waypoints, label: t("nav.graph"), onClick: () => focusGraph(null) },
    // 「繋ぐ」= 外部アプリとの接続。ノート間の「つながり」(Link)とは別物なので
    // 記号も分ける(絵文字ではどちらも 🔗 で区別が付かなかった)
    { view: "connect", icon: Plug, label: t("nav.connect"), onClick: () => go("connect") },
    { view: "settings", icon: Settings, label: t("nav.settings"), onClick: () => go("settings") },
  ];

  return (
    <nav
      className={`border-line bg-panel-2 flex flex-none flex-col gap-0.5 border-r text-[13px] ${
        visuallyCollapsed
          ? "w-[52px] px-1.5 py-3.5"
          : "w-[176px] px-2.5 py-3.5 max-[1040px]:w-[132px]"
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

      {items.map((item) => (
        <NavButton
          key={item.view}
          icon={item.icon}
          label={item.label}
          collapsed={visuallyCollapsed}
          active={view === item.view}
          onClick={item.onClick}
        />
      ))}
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
