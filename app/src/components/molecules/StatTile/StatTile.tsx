import type { LucideIcon } from "lucide-react";

import { Icon } from "@/components/atoms/Icon";

import { statTileVariants } from "./variants";

export interface StatTileProps {
  value: React.ReactNode;
  label: string;
  icon: LucideIcon;
  amber?: boolean;
  onClick?: () => void;
}

/** ホームの数値タイル。 */
export function StatTile({ value, label, icon, amber, onClick }: StatTileProps) {
  const s = statTileVariants({ amber, clickable: Boolean(onClick) });
  const body = (
    <>
      <div className={s.num()}>{value}</div>
      <div className={s.label()}>
        <Icon as={icon} size="sm" />
        {label}
      </div>
    </>
  );
  return onClick ? (
    <button type="button" className={s.root()} onClick={onClick}>
      {body}
    </button>
  ) : (
    <div className={s.root()}>{body}</div>
  );
}
