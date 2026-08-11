import type { LucideIcon } from "lucide-react";

import { Icon } from "@/components/atoms/Icon";
import { StatusPill } from "@/components/atoms/StatusPill";

export interface ConnectCardProps {
  name: string;
  icon: LucideIcon;
  description: string;
  state: { label: string; ok: boolean };
  /** 補足行(未送信の件数、エラーなど)。 */
  notes?: React.ReactNode;
  children?: React.ReactNode;
}

/** 「繋ぐ」画面のカード1枚(見た目だけ。操作は organisms が持つ)。 */
export function ConnectCard({ name, icon, description, state, notes, children }: ConnectCardProps) {
  return (
    <div className="border-line bg-panel rounded-xl border px-4 py-3.5">
      <div className="flex items-center gap-2 font-bold">
        <Icon as={icon} />
        {name}
      </div>
      <div className="text-muted mb-3 text-[12.5px]">{description}</div>
      <StatusPill tone={state.ok ? "grow" : "muted"} className="mb-2.5 inline-block">
        {state.label}
      </StatusPill>
      {notes}
      <div className="mt-1">{children}</div>
    </div>
  );
}
