import { SlidersHorizontal } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/atoms/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/atoms/ui/popover";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/atoms/ui/select";
import { TagChip } from "@/components/atoms/TagChip";
import { TagCombobox } from "@/components/molecules/TagCombobox";
import type { Period, SortKey } from "@/lib/api";

import { SavedSearchActions } from "./SavedSearchActions";

const PERIODS: Period[] = ["all", "7", "30", "90"];
const SORTS: SortKey[] = ["updated", "created", "title"];

interface SearchFilterBarProps {
  query: string;
  allTags: string[];
  tags: string[];
  period: Period;
  sort: SortKey;
  onAddTag: (tag: string) => void;
  onRemoveTag: (tag: string) => void;
  onClearTags: () => void;
  onPeriodChange: (period: Period) => void;
  onSortChange: (sort: SortKey) => void;
}

/** Modal 内では検索入力を主役にし、絞り込みは必要なときだけ開く。 */
export function SearchFilterBar({
  query,
  allTags,
  tags,
  period,
  sort,
  onAddTag,
  onRemoveTag,
  onClearTags,
  onPeriodChange,
  onSortChange,
}: SearchFilterBarProps) {
  const { t } = useTranslation(["notes", "common"]);
  const activeCount = tags.length + (period === "all" ? 0 : 1) + (sort === "updated" ? 0 : 1);

  return (
    <div className="border-line flex min-h-11 flex-wrap items-center gap-1.5 border-b px-3 py-2">
      <Popover>
        <PopoverTrigger asChild>
          <Button size="sm" variant={activeCount > 0 ? "default" : "quiet"}>
            <SlidersHorizontal className="size-3.5" />
            {t("notes:filter.title")}
            {activeCount > 0 && (
              <span className="bg-grow text-on-accent rounded-full px-1.5 text-[10px]">
                {activeCount}
              </span>
            )}
          </Button>
        </PopoverTrigger>
        <PopoverContent
          align="start"
          className="w-[320px] p-3"
          onOpenAutoFocus={(event) => event.preventDefault()}
        >
          <div className="mb-2 text-xs font-semibold">{t("notes:filter.tagPlaceholder")}</div>
          <TagCombobox
            className="border-line mx-0 mb-4 rounded-md border px-2 py-1"
            allTags={allTags}
            selected={tags}
            onAdd={onAddTag}
            onRemove={onRemoveTag}
            onClear={onClearTags}
            labels={{
              placeholder: t("notes:filter.tagPlaceholder"),
              clear: t("notes:filter.clearTags"),
              clearTitle: t("notes:filter.clearTagsTitle"),
              empty: t("notes:list.notFound"),
            }}
          />

          <label className="text-muted mb-3 flex items-center gap-3 text-xs">
            <span className="w-12">{t("notes:filter.period")}</span>
            <Select value={period} onValueChange={(v) => onPeriodChange(v as Period)}>
              <SelectTrigger size="sm" className="flex-1">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {PERIODS.map((value) => (
                  <SelectItem key={value} value={value}>
                    {value === "all"
                      ? t("notes:filter.periodAll")
                      : t("notes:filter.periodDays", { count: Number(value) })}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </label>

          <label className="text-muted flex items-center gap-3 text-xs">
            <span className="w-12">{t("notes:filter.sort")}</span>
            <Select value={sort} onValueChange={(v) => onSortChange(v as SortKey)}>
              <SelectTrigger size="sm" className="flex-1">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {SORTS.map((value) => (
                  <SelectItem key={value} value={value}>
                    {value === "updated"
                      ? t("notes:filter.sortUpdated")
                      : value === "created"
                        ? t("notes:filter.sortCreated")
                        : t("notes:filter.sortTitle")}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </label>

          {activeCount > 0 && (
            <Button
              variant="quiet"
              size="sm"
              className="mt-3"
              onClick={() => {
                onClearTags();
                onPeriodChange("all");
                onSortChange("updated");
              }}
            >
              {t("notes:search.clearFilters")}
            </Button>
          )}

          <SavedSearchActions query={query} tags={tags} period={period} sort={sort} />
        </PopoverContent>
      </Popover>

      {tags.map((tag) => (
        <TagChip
          key={tag}
          tag={tag}
          selected
          onRemove={() => onRemoveTag(tag)}
          removeLabel={t("notes:search.removeFilter", { tag })}
        />
      ))}

      {period !== "all" && (
        <button
          type="button"
          className="bg-grow-soft text-grow cursor-pointer rounded-full border-none px-2 py-0.5 text-[11px]"
          onClick={() => onPeriodChange("all")}
        >
          {t("notes:filter.periodDays", { count: Number(period) })} ×
        </button>
      )}
    </div>
  );
}
