import { NoteSummaryCard } from "@/components/molecules/NoteSummaryCard";

import type { Hit } from "@/lib/api";

export interface RecentNoteRowProps {
  hit: Hit;
  onOpen: () => void;
}

/** ホームの「最近のノート」カード。 */
export function RecentNoteRow({ hit, onOpen }: RecentNoteRowProps) {
  return (
    <NoteSummaryCard
      title={hit.title?.trim() || hit.id.split("/").at(-1) || hit.id}
      description={hit.snippet}
      tags={hit.tags}
      createdAt={hit.created}
      updatedAt={hit.updated}
      onOpen={onOpen}
    />
  );
}
