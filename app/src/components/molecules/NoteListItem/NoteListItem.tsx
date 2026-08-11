import { Wrench } from "lucide-react";
import { tv } from "tailwind-variants";

import { Icon } from "@/components/atoms/Icon";
import type { Hit } from "@/lib/api";

const item = tv({
  slots: {
    root: "w-full cursor-pointer border-l-2 px-3.5 py-2 text-left",
    title: "line-clamp-2 font-semibold",
    snippet: "line-clamp-2 text-[11.5px] text-muted",
    dates: "mt-0.5 text-[10.5px] leading-snug text-muted opacity-85",
  },
  variants: {
    selected: {
      true: { root: "border-l-grow bg-sel" },
      false: { root: "border-l-transparent hover:bg-sel/50" },
    },
  },
  defaultVariants: { selected: false },
});

export interface NoteListItemProps {
  hit: Hit;
  selected: boolean;
  /** お手入れ提案が付いているノート(旧実装の 🔧 印)。 */
  needsCare: boolean;
  createdLabel: string;
  updatedLabel: string;
  onOpen: () => void;
}

export function NoteListItem({
  hit,
  selected,
  needsCare,
  createdLabel,
  updatedLabel,
  onOpen,
}: NoteListItemProps) {
  const s = item({ selected });
  return (
    <button type="button" className={s.root()} onClick={onOpen} aria-current={selected}>
      <div className={s.title()}>
        {hit.title ?? hit.id}
        {/* お手入れの提案が付いているノート(琥珀 = 提案の色) */}
        {needsCare && (
          <Icon as={Wrench} size="sm" className="text-prop ml-1 inline-block align-text-bottom" />
        )}
      </div>
      <div className={s.snippet()}>{hit.snippet.slice(0, 80)}</div>
      <div className={s.dates()}>
        {createdLabel}
        <br />
        {updatedLabel}
      </div>
    </button>
  );
}
