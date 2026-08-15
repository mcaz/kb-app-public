import { FileText, FolderOpen } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";
import { Button } from "@/components/atoms/ui/button";
import { formatDay } from "@/lib/format";
import { useCategoryNotes } from "@/lib/queries";

export interface CategoryNoteListProps {
  category: string;
  selectedId: string | null;
  compact?: boolean;
  onOpenNote: (id: string) => void;
}

/** 選択カテゴリだけをcursor pageで読む、中間のノート一覧パネル。 */
export function CategoryNoteList({
  category,
  selectedId,
  compact = false,
  onOpenNote,
}: CategoryNoteListProps) {
  const { t, i18n } = useTranslation("notes");
  const query = useCategoryNotes(category);
  const notes = query.data?.pages.flatMap((page) => page.notes) ?? [];
  const total = query.data?.pages[0]?.total ?? 0;
  const name = category.split("/").filter(Boolean).at(-1) ?? t("browse.root");

  return (
    <section
      className={`bg-panel flex min-h-0 flex-none flex-col overflow-hidden ${
        compact ? "min-w-0 flex-1" : "border-line w-[280px] border-r max-[1120px]:w-[240px]"
      }`}
      aria-label={t("browse.panelLabel", { name })}
    >
      <header className="border-line flex min-h-[58px] flex-none items-center gap-2 border-b px-3.5 py-2.5">
        <Icon as={FolderOpen} className="text-grow flex-none" />
        <div className="min-w-0 flex-1">
          <h2 className="m-0 truncate text-[13px] font-bold">{name}</h2>
          <p className="text-muted m-0 text-[11px] tabular-nums">
            {t("browse.count", { count: total })}
          </p>
        </div>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto p-2">
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
              <button
                key={note.id}
                type="button"
                aria-current={selected ? "page" : undefined}
                onClick={() => onOpenNote(note.id)}
                className={`mb-1 flex w-full cursor-pointer flex-col items-stretch gap-1 rounded-lg border px-3 py-2.5 text-left ${
                  selected
                    ? "border-grow bg-sel text-ink"
                    : "hover:border-line hover:bg-sel/50 border-transparent bg-transparent"
                }`}
              >
                <span className="flex min-w-0 items-start gap-2">
                  <Icon as={FileText} size="sm" className="text-muted mt-0.5 flex-none" />
                  <span className="line-clamp-2 min-w-0 flex-1 text-[12.5px] font-semibold">
                    {note.title?.trim() || fallback}
                  </span>
                  {note.updated && (
                    <span className="text-muted flex-none text-[10px]">
                      {formatDay(note.updated, i18n.language)}
                    </span>
                  )}
                </span>
                {note.description && (
                  <span className="text-muted line-clamp-2 pl-5.5 text-[11px] leading-relaxed">
                    {note.description}
                  </span>
                )}
                {note.tags.length > 0 && (
                  <span className="text-muted truncate pl-5.5 text-[10px]">
                    {note.tags.join(" · ")}
                  </span>
                )}
              </button>
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
