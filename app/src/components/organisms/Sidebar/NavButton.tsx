import type { LucideIcon } from "lucide-react";

import { Icon } from "@/components/atoms/Icon";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/atoms/ui/tooltip";

export interface NavButtonProps {
  icon: LucideIcon;
  label: string;
  collapsed: boolean;
  active: boolean;
  onClick: () => void;
}

/** Sidebar 専用の項目(私物なのでこのディレクトリに置く)。 */
export function NavButton({ icon, label, collapsed, active, onClick }: NavButtonProps) {
  const button = (
    <button
      type="button"
      onClick={onClick}
      aria-current={active ? "page" : undefined}
      aria-label={collapsed ? label : undefined}
      className={`flex cursor-pointer items-center gap-2 rounded-md border-none bg-transparent px-2.5 py-1.5 text-left text-[13px] whitespace-nowrap ${
        active ? "bg-sel text-ink" : "text-muted hover:text-ink"
      } ${collapsed ? "justify-center px-1" : ""}`}
    >
      <Icon as={icon} />
      {!collapsed && <span className="min-w-0 flex-1 truncate">{label}</span>}
    </button>
  );

  if (!collapsed) return button;
  return (
    <Tooltip>
      <TooltipTrigger asChild>{button}</TooltipTrigger>
      <TooltipContent side="right">{label}</TooltipContent>
    </Tooltip>
  );
}
