import { ChevronDown, ChevronRight } from "lucide-react";
import { useMemo } from "react";
import { useTranslation } from "react-i18next";

import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/atoms/ui/select";
import { NoteListItem } from "@/components/molecules/NoteListItem";
import { Pager } from "@/components/molecules/Pager";
import { SelectionStrip } from "@/components/molecules/SelectionStrip";
import { FilterPanel } from "@/components/organisms/FilterPanel";
import type { Hit } from "@/lib/api";
import { formatDateTime } from "@/lib/format";
import { activeFilterCount, matchesFilter, paginate, sortHits } from "@/lib/hits";
import { useHomeState, useNote, useNoteSearch } from "@/lib/queries";
import { useDebouncedValue } from "@/hooks/useDebouncedValue";
import { PAGE_SIZES, usePrefs } from "@/lib/stores/prefs";
import { useSession } from "@/lib/stores/session";

export interface NoteListPanelProps {
  width: number;
}

/** ノート画面の左カラム(絞り込み・一覧・ページャ)。 */
export function NoteListPanel({ width }: NoteListPanelProps) {
  const { t, i18n } = useTranslation(["notes", "common"]);
  const session = useSession();
  const prefs = usePrefs();
  const { data: home } = useHomeState();

  const debouncedQuery = useDebouncedValue(session.query.trim(), 250);
  const search = useNoteSearch(debouncedQuery);
  const searching = debouncedQuery.length > 0;

  const source: Hit[] = (searching ? search.data?.hits : home?.notes) ?? [];
  const filter = { tags: session.selectedTags, period: session.period };

  const page = useMemo(() => {
    const hits = sortHits(
      source.filter((h) => matchesFilter(h, filter)),
      session.sort,
      i18n.language,
    );
    return paginate(hits, prefs.pageSize, session.listPage);
    // filter はオブジェクトリテラルなので中身で依存を張る
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    source,
    session.selectedTags,
    session.period,
    session.sort,
    prefs.pageSize,
    session.listPage,
    i18n.language,
  ]);

  const careIds = useMemo(() => {
    const ids = new Set<string>();
    for (const c of home?.care ?? []) {
      ids.add(c.a);
      ids.add(c.b);
    }
    return ids;
  }, [home?.care]);

  const { data: primary } = useNote(session.selectedId);
  const { data: secondary } = useNote(session.secondaryId);
  const entries = [
    primary && { id: primary.id, title: primary.title, secondary: false },
    secondary && { id: secondary.id, title: secondary.title, secondary: true },
  ].filter((e): e is { id: string; title: string; secondary: boolean } => Boolean(e));

  const filterCount = activeFilterCount(filter);

  return (
    <div
      style={{ width }}
      className="border-line flex min-h-0 min-w-[200px] flex-none flex-col border-r py-2.5 text-[13px]"
    >
      <button
        type="button"
        aria-expanded={prefs.filterOpen}
        onClick={() => prefs.set({ filterOpen: !prefs.filterOpen })}
        className="border-line text-muted hover:border-grow hover:text-ink mx-2.5 mb-1.5 flex cursor-pointer items-center justify-between gap-1.5 rounded-md border bg-transparent px-2.5 py-0.5 text-[11.5px]"
      >
        <span className="flex items-center gap-1">
          {prefs.filterOpen ? (
            <ChevronDown className="size-3" />
          ) : (
            <ChevronRight className="size-3" />
          )}
          {t("filter.title")}
        </span>
        {filterCount > 0 && (
          <span className="bg-grow text-on-accent rounded-full px-1.5 text-[10px] leading-4">
            {filterCount}
          </span>
        )}
      </button>

      {prefs.filterOpen && <FilterPanel allTags={(home?.tags ?? []).map(([tag]) => tag)} />}

      <SelectionStrip
        entries={entries}
        labels={{
          head: t("selection.head"),
          primary: t("selection.primary"),
          secondary: t("selection.secondary"),
          close: t("common:action.close"),
        }}
        onFocus={(entry) => (entry.secondary ? session.promoteSecondary() : undefined)}
        onClose={session.closeSecondary}
      />

      <div className="flex-1 overflow-y-auto">
        {page.items.length === 0 ? (
          <div className="text-muted px-3.5 py-2 text-[12.5px]">
            {searching ? t("list.notFound") : t("list.empty")}
          </div>
        ) : (
          page.items.map((hit) => (
            <NoteListItem
              key={hit.id}
              hit={hit}
              selected={session.selectedId === hit.id}
              needsCare={careIds.has(hit.id)}
              createdLabel={t("common:date.created", {
                value: formatDateTime(hit.created, i18n.language, t("common:date.unknown")),
              })}
              updatedLabel={t("common:date.updated", {
                value: formatDateTime(hit.updated, i18n.language, t("common:date.unknown")),
              })}
              onOpen={() => session.openNote(hit.id)}
            />
          ))
        )}
      </div>

      <div className="flex flex-none items-center justify-between gap-2 px-2.5 pt-0.5 pb-2">
        <Select
          value={String(prefs.pageSize)}
          onValueChange={(v) => {
            prefs.set({ pageSize: Number(v) });
            session.setListPage(0);
          }}
        >
          <SelectTrigger size="sm" className="w-[72px]">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {PAGE_SIZES.map((n) => (
              <SelectItem key={n} value={String(n)}>
                {n}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Pager
          page={page.page}
          pageCount={page.pageCount}
          from={page.from}
          to={page.to}
          total={page.total}
          onChange={session.setListPage}
          labels={{ previous: "‹", next: "›" }}
        />
      </div>
    </div>
  );
}
