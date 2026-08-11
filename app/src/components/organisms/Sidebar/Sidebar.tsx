import {
  ChevronLeft,
  ChevronRight,
  House,
  NotebookText,
  Plug,
  Settings,
  Sprout,
  Star,
  Waypoints,
  type LucideIcon,
} from "lucide-react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/atoms/ui/tooltip";
import { usePrefs } from "@/lib/stores/prefs";
import { useSession, type View } from "@/lib/stores/session";

import { NavButton } from "./NavButton";

export interface SidebarProps {
  vaultName: string;
  onOpenFavorites: () => void;
}

/** 左のナビ。畳むとアイコンだけになり、ホバーでラベルを吹き出す。 */
export function Sidebar({ vaultName, onOpenFavorites }: SidebarProps) {
  const { t } = useTranslation();
  const view = useSession((s) => s.view);
  const go = useSession((s) => s.go);
  const resetNotes = useSession((s) => s.resetNotes);
  const focusGraph = useSession((s) => s.focusGraph);
  const collapsed = usePrefs((s) => s.sideCollapsed);
  const setPrefs = usePrefs((s) => s.set);

  const items: { view: View; icon: LucideIcon; label: string; onClick: () => void }[] = [
    { view: "home", icon: House, label: t("nav.home"), onClick: () => go("home") },
    // ナビの「ノート」はまっさらな一覧に戻す(絞り込み・検索・選択を解除)
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
        collapsed ? "w-[52px] px-1.5 py-3.5" : "w-[176px] px-2.5 py-3.5 max-[1040px]:w-[132px]"
      }`}
    >
      <div
        className={`flex items-center gap-2 px-2.5 py-1.5 font-bold ${
          collapsed ? "justify-center px-0" : ""
        }`}
      >
        <Icon as={Sprout} className="text-grow" />
        {!collapsed && <span className="truncate">{vaultName}</span>}
      </div>

      {items.map((item) => (
        <NavButton
          key={item.view}
          icon={item.icon}
          label={item.label}
          collapsed={collapsed}
          active={view === item.view}
          onClick={item.onClick}
        />
      ))}
      <NavButton
        icon={Star}
        label={t("nav.favorites")}
        collapsed={collapsed}
        active={false}
        onClick={onOpenFavorites}
      />

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
    </nav>
  );
}
