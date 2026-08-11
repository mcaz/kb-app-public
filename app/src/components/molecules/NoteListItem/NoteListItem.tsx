import { Wrench } from "lucide-react";

import { Icon } from "@/components/atoms/Icon";
import type { Hit } from "@/lib/api";

import { noteListItemVariants } from "./variants";

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
  const s = noteListItemVariants({ selected });
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
