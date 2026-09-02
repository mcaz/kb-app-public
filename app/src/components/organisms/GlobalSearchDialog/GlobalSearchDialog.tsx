import { Clock3, Search, Star } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/atoms/ui/command";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/atoms/ui/dialog";
import { DegradedBanner } from "@/components/molecules/DegradedBanner";
import { NotePreview } from "@/components/organisms/NotePreview";
import { useDebouncedValue } from "@/hooks/useDebouncedValue";
import { useMediaQuery } from "@/hooks/useMediaQuery";
import { formatDateTime } from "@/lib/format";
import { matchesFilter, sortHits } from "@/lib/hits";
import { useFavorites, useHomeState, useNoteSearch } from "@/lib/queries";
import { effectiveSearchPane, resolveSearchSelection, type SearchPane } from "@/lib/searchDialog";
import { useSession } from "@/lib/stores/session";

import { SearchFilterBar } from "./SearchFilterBar";

export interface GlobalSearchDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/** どの画面からでも開ける、結果と本文プレビューを一体にした検索。 */
export function GlobalSearchDialog({ open, onOpenChange }: GlobalSearchDialogProps) {
  const { t, i18n } = useTranslation(["notes", "common"]);
  const session = useSession();
  const compact = useMediaQuery("(max-width: 759px)");
  const [compactPane, setCompactPane] = useState<SearchPane>("results");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const debouncedQuery = useDebouncedValue(session.query.trim(), 250);
  const search = useNoteSearch(debouncedQuery);
  const { data: home } = useHomeState();
  const { data: favorites = [] } = useFavorites();
  const searching = debouncedQuery.length > 0;

  const hits = useMemo(() => {
    const source = (searching ? search.data?.hits : home?.notes) ?? [];
    return sortHits(
      source.filter((hit) =>
        matchesFilter(hit, { tags: session.selectedTags, period: session.period }),
      ),
      session.sort,
      i18n.language,
    ).slice(0, 30);
  }, [
    home?.notes,
    i18n.language,
    search.data?.hits,
    searching,
    session.period,
    session.selectedTags,
    session.sort,
  ]);

  const effectiveSelectedId = resolveSearchSelection(
    selectedId,
    hits.map((hit) => hit.id),
  );
  const effectiveCompactPane = effectiveSearchPane(compact, compactPane);

  const openNote = (id: string) => {
    session.openNote(id);
    onOpenChange(false);
    setCompactPane("results");
  };

  const select = (id: string) => {
    setSelectedId(id);
    if (compact) setCompactPane("preview");
    else openNote(id);
  };

  const showResults = !compact || effectiveCompactPane === "results";
  const showPreview = !compact || effectiveCompactPane === "preview";
  const resultHeading = searching ? t("notes:search.results") : t("notes:search.recent");

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) setCompactPane("results");
        onOpenChange(next);
      }}
    >
      <DialogContent
        showCloseButton={false}
        className="h-[80vh] max-h-[760px] w-[calc(100%-1rem)] max-w-[1180px] gap-0 overflow-hidden p-0 sm:max-w-[1180px]"
        onKeyDown={(event) => {
          if (!compact || effectiveCompactPane !== "preview" || event.key !== "Enter") return;
          if (event.nativeEvent.isComposing || !effectiveSelectedId) return;
          event.preventDefault();
          openNote(effectiveSelectedId);
        }}
      >
        <DialogTitle className="sr-only">{t("notes:search.dialogTitle")}</DialogTitle>
        <DialogDescription className="sr-only">
          {t("notes:search.dialogDescription")}
        </DialogDescription>

        <Command
          shouldFilter={false}
          value={effectiveSelectedId ?? ""}
          onValueChange={(value) => setSelectedId(value || null)}
          className="bg-ground rounded-none"
        >
          {showResults && (
            <>
              <CommandInput
                value={session.query}
                onValueChange={(value) => {
                  session.setQuery(value);
                  setCompactPane("results");
                }}
                placeholder={t("notes:search.placeholder")}
                className="text-base"
              />
              <SearchFilterBar
                query={session.query}
                allTags={(home?.tags ?? []).map(([tag]) => tag)}
                tags={session.selectedTags}
                period={session.period}
                sort={session.sort}
                onAddTag={session.addTag}
                onRemoveTag={session.removeTag}
                onClearTags={session.clearTags}
                onPeriodChange={session.setPeriod}
                onSortChange={session.setSort}
              />
            </>
          )}

          <div className="grid min-h-0 flex-1 grid-cols-[minmax(320px,42%)_minmax(0,1fr)] max-[759px]:grid-cols-1">
            {showResults && (
              <section className="border-line flex min-h-0 min-w-0 flex-col border-r max-[759px]:border-r-0">
                {session.query.trim() === "" && favorites.length > 0 && (
                  <div className="border-line border-b px-3 py-2.5">
                    <div className="text-muted mb-1.5 flex items-center gap-1.5 text-[11px] font-semibold tracking-wide">
                      <Star className="size-3.5" />
                      {t("notes:search.savedSearches")}
                    </div>
                    <div className="flex flex-wrap gap-1.5">
                      {favorites.map((favorite) => (
                        <button
                          key={favorite.name}
                          type="button"
                          className="border-line bg-chip hover:border-grow cursor-pointer rounded-md border px-2.5 py-1 text-xs"
                          onClick={() => session.applyFavorite(favorite)}
                        >
                          {favorite.name}
                        </button>
                      ))}
                    </div>
                  </div>
                )}

                {searching && (
                  <DegradedBanner items={search.data?.degraded ?? []} variant="inline" />
                )}

                <div className="text-muted flex items-center gap-1.5 px-3 pt-2.5 pb-1 text-[11px] font-semibold tracking-wide">
                  {searching ? <Search className="size-3.5" /> : <Clock3 className="size-3.5" />}
                  <span>{resultHeading}</span>
                  <span>({hits.length})</span>
                  {search.isFetching && (
                    <span className="ml-auto">{t("common:state.loading")}</span>
                  )}
                </div>

                <CommandList className="max-h-none min-h-0 flex-1 px-2 pb-2">
                  <CommandEmpty>{t("notes:list.notFound")}</CommandEmpty>
                  <CommandGroup>
                    {hits.map((hit) => (
                      <CommandItem
                        key={hit.id}
                        value={hit.id}
                        onMouseMove={() => setSelectedId(hit.id)}
                        onSelect={() => select(hit.id)}
                        className="data-[selected=true]:border-line flex-col items-stretch gap-1 border border-transparent px-3 py-2.5"
                      >
                        <div className="flex min-w-0 items-start gap-2">
                          <span className="line-clamp-2 min-w-0 flex-1 font-semibold">
                            {hit.title ?? hit.id}
                          </span>
                          {hit.updated && (
                            <span className="text-muted shrink-0 text-[10px]">
                              {formatDateTime(hit.updated, i18n.language, t("common:date.unknown"))}
                            </span>
                          )}
                        </div>
                        <div className="text-muted line-clamp-2 text-xs">{hit.snippet}</div>
                        {hit.tags.length > 0 && (
                          <div className="text-muted truncate text-[10.5px]">
                            {hit.tags.join(" · ")}
                          </div>
                        )}
                      </CommandItem>
                    ))}
                  </CommandGroup>
                </CommandList>
              </section>
            )}

            {showPreview && (
              <section className="bg-panel min-h-0 min-w-0">
                <NotePreview
                  noteId={effectiveSelectedId}
                  compact={compact}
                  emptyLabel={t("notes:search.previewEmpty")}
                  backLabel={t("notes:search.backToResults")}
                  openLabel={t("notes:search.openNote")}
                  onBack={() => setCompactPane("results")}
                  onOpen={openNote}
                />
              </section>
            )}
          </div>

          <footer className="border-line text-muted flex h-10 flex-none items-center gap-4 border-t px-3 text-[11px] max-[560px]:hidden">
            {showResults && <span>{t("notes:search.keyMove")}</span>}
            <span>{t("notes:search.keyOpen")}</span>
            <span>{t("notes:search.keyClose")}</span>
          </footer>
        </Command>
      </DialogContent>
    </Dialog>
  );
}
