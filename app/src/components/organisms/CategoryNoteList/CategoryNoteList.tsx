import { FileText, FolderOpen } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";
import { Button } from "@/components/atoms/ui/button";
import { formatDay } from "@/lib/format";
import { useCategoryNotes } from "@/lib/queries";

export interface CategoryNoteListProps {
  category: string;
  selectedId: string | null;
  onOpenNote: (id: string) => void;
}

/** 選択カテゴリだけをcursor pageで読み、主領域全体に表示するノート一覧。 */
export function CategoryNoteList({ category, selectedId, onOpenNote }: CategoryNoteListProps) {
  const { t, i18n } = useTranslation("notes");
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

      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-2 max-[800px]:px-2">
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
                className={`mb-1 flex w-full cursor-pointer items-start gap-4 rounded-lg border px-3.5 py-3 text-left max-[900px]:flex-wrap max-[900px]:gap-x-2 max-[900px]:gap-y-1 ${
                  selected
                    ? "border-grow bg-sel text-ink"
                    : "hover:border-line hover:bg-sel/50 border-transparent bg-transparent"
                }`}
              >
                <span className="flex min-w-[180px] flex-1 items-start gap-2 max-[900px]:min-w-0">
                  <Icon as={FileText} size="sm" className="text-muted mt-0.5 flex-none" />
                  <span className="min-w-0 flex-1">
                    <span className="line-clamp-2 block text-[13px] font-semibold">
                      {note.title?.trim() || fallback}
                    </span>
                    {note.tags.length > 0 && (
                      <span className="text-muted mt-1 block truncate text-[10.5px]">
                        {note.tags.join(" · ")}
                      </span>
                    )}
                  </span>
                </span>
                {note.description && (
                  <span className="text-muted line-clamp-2 min-w-[240px] flex-[1.5] text-xs leading-relaxed max-[900px]:order-3 max-[900px]:w-full max-[900px]:min-w-0 max-[900px]:pl-5.5">
                    {note.description}
                  </span>
                )}
                {note.updated && (
                  <span className="text-muted flex-none text-[11px] tabular-nums">
                    {formatDay(note.updated, i18n.language)}
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
