import type { LucideIcon } from "lucide-react";

import { Icon } from "@/components/atoms/Icon";

export interface RelatedEntry {
  id: string;
  title: string;
  /** 近いノートのみ。コサイン距離を右端に出す。 */
  distance?: number | null;
}

export interface RelatedListProps {
  head: string;
  headIcon: LucideIcon;
  entries: RelatedEntry[];
  emptyLabel: string;
  /** 「近いノート」= まだ関連づいていない候補(琥珀系)。 */
  tone: "linked" | "similar";
  openId: string | null;
  onOpen: (id: string) => void;
}

export function RelatedList({
  head,
  headIcon,
  entries,
  emptyLabel,
  tone,
  openId,
  onOpen,
}: RelatedListProps) {
  const base =
    tone === "linked"
      ? "border-grow-soft bg-grow-soft text-grow hover:border-grow"
      : "border-prop-soft bg-prop-soft text-prop hover:border-prop";

  return (
    <>
      <div className="text-muted mb-1.5 flex items-center gap-1.5 text-[11.5px] tracking-[0.08em]">
        <Icon as={headIcon} size="sm" />
        {head}
      </div>
      {entries.length === 0 ? (
        <div className="text-muted text-xs">{emptyLabel}</div>
      ) : (
        entries.map((entry) => (
          <button
            key={entry.id}
            type="button"
            onClick={() => onOpen(entry.id)}
            className={`flex w-full cursor-pointer justify-between gap-2 rounded-md border px-2.5 py-1.5 text-left text-xs ${base} ${
              openId === entry.id ? "border-ink font-semibold" : ""
            }`}
          >
            <span className="min-w-0 truncate">{entry.title}</span>
            {entry.distance != null && (
              <span className="text-[10.5px] opacity-75">{entry.distance.toFixed(2)}</span>
            )}
          </button>
        ))
      )}
    </>
  );
}
