import type { Hit } from "@/lib/api";

export interface RecentNoteRowProps {
  hit: Hit;
  datesLabel: string;
  onOpen: () => void;
}

/** ホームの「最近のノート」1行。 */
export function RecentNoteRow({ hit, datesLabel, onOpen }: RecentNoteRowProps) {
  return (
    <button
      type="button"
      onClick={onOpen}
      className="border-line bg-panel text-ink hover:bg-sel flex flex-col gap-px rounded-lg border px-3 py-2 text-left"
    >
      <span className="text-[13px] font-semibold">{hit.title ?? hit.id}</span>
      <span className="text-muted text-[11.5px]">{hit.snippet.slice(0, 60)}</span>
      <span className="text-muted text-[10.5px] opacity-85">{datesLabel}</span>
    </button>
  );
}
