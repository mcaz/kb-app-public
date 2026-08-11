import type { LucideIcon } from "lucide-react";
import { tv } from "tailwind-variants";

import { Icon } from "@/components/atoms/Icon";

const tile = tv({
  slots: {
    root: "border-line bg-panel rounded-xl border px-4 py-3.5 text-left",
    num: "text-[22px] leading-tight font-bold",
    label: "text-muted mt-0.5 flex items-center gap-1.5 text-xs",
  },
  variants: {
    amber: { true: { root: "border-prop", num: "text-prop" } },
    clickable: { true: { root: "w-full cursor-pointer hover:bg-sel" } },
  },
});

export interface StatTileProps {
  value: React.ReactNode;
  label: string;
  icon: LucideIcon;
  amber?: boolean;
  onClick?: () => void;
}

/** ホームの数値タイル。 */
export function StatTile({ value, label, icon, amber, onClick }: StatTileProps) {
  const s = tile({ amber, clickable: Boolean(onClick) });
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
