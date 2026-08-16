import { FolderOpen } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";
import { Button } from "@/components/atoms/ui/button";
import { NoteSummaryCard } from "@/components/molecules/NoteSummaryCard";
import { useCategoryNotes } from "@/lib/queries";

export interface CategoryNoteListProps {
  category: string;
  selectedId: string | null;
  onOpenNote: (id: string) => void;
}

/** 選択カテゴリだけをcursor pageで読み、主領域全体に表示するノート一覧。 */
export function CategoryNoteList({ category, selectedId, onOpenNote }: CategoryNoteListProps) {
  const { t, i18n } = useTranslation(["notes", "common"]);
  const query = useCategoryNotes(category);
  const notes = query.data?.pages.flatMap((page) => page.notes) ?? [];
  const total = query.data?.pages[0]?.total ?? 0;
  const name = category.split("/").filter(Boolean).at(-1) ?? t("browse.root");
  const formattedTotal = new Intl.NumberFormat(i18n.language).format(total);

  return (
    <section
      className="bg-panel flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden"
      aria-label={t("browse.panelLabel", { name })}
    >
      <header className="border-line flex min-h-[64px] flex-none items-center gap-3 border-b px-5 py-3 max-[800px]:px-3.5">
        <Icon as={FolderOpen} className="text-grow flex-none" />
        <div className="min-w-0 flex-1">
          <h2 className="m-0 truncate text-base font-bold">{name}</h2>
          <p className="text-muted m-0 text-xs tabular-nums">
            {t("browse.count", { count: formattedTotal })}
          </p>
        </div>
      </header>

      <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-5 py-4 max-[800px]:px-3 max-[800px]:py-3">
        {query.isPending ? (
          <p className="text-muted m-0 px-2 py-3 text-xs" role="status">
            {t("browse.loading")}
          </p>
        ) : query.isError ? (
          <div className="px-2 py-3 text-xs" role="alert">
            <p className="text-muted m-0 mb-2">{t("browse.loadError")}</p>
            <Button size="sm" onClick={() => void query.refetch()}>
              {t("browse.retry")}
            </Button>
          </div>
        ) : notes.length === 0 ? (
          <p className="text-muted m-0 px-2 py-3 text-xs">{t("browse.empty")}</p>
        ) : (
          notes.map((note) => {
            const selected = selectedId === note.id;
            const fallback = note.id.split("/").at(-1) ?? note.id;
            return (
              <NoteSummaryCard
                key={note.id}
                title={note.title?.trim() || fallback}
                description={note.description}
                tags={note.tags}
                createdAt={note.created}
                updatedAt={note.updated}
                selected={selected}
                onOpen={() => onOpenNote(note.id)}
              />
            );
          })
        )}

        {query.hasNextPage && (
          <div className="flex justify-center px-2 py-3">
            <Button
              size="sm"
              disabled={query.isFetchingNextPage}
              onClick={() => void query.fetchNextPage()}
            >
              {query.isFetchingNextPage ? t("browse.loading") : t("browse.loadMore")}
            </Button>
          </div>
        )}
      </div>
    </section>
  );
}
