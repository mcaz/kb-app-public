import { Star } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Icon } from "@/components/atoms/Icon";
import { Button } from "@/components/atoms/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/atoms/ui/select";
import { NamePromptDialog } from "@/components/molecules/NamePromptDialog";
import { TagCombobox } from "@/components/molecules/TagCombobox";
import type { Favorite, Period, SortKey } from "@/lib/api";
import { useErrorText } from "@/hooks/useErrorText";
import { useFavoriteAdd, useFavoriteRemove, useFavorites } from "@/lib/queries";
import { useSession } from "@/lib/stores/session";

export interface FilterPanelProps {
  allTags: string[];
}

const PERIODS: Period[] = ["all", "7", "30", "90"];
const SORTS: SortKey[] = ["updated", "created", "title"];

/** 一覧の絞り込みと、その状態のお気に入り保存。 */
export function FilterPanel({ allTags }: FilterPanelProps) {
  const { t } = useTranslation(["notes", "common"]);
  const session = useSession();
  const { data: favorites = [] } = useFavorites();
  const errorText = useErrorText();
  const favoriteAdd = useFavoriteAdd();
  const favoriteRemove = useFavoriteRemove();
  const [prompt, setPrompt] = useState<"save" | "rename" | null>(null);

  const currentFilter = (name: string): Favorite => ({
    name,
    tags: [...session.selectedTags],
    query: session.query.trim() || null,
    period: session.period,
    sort: session.sort,
  });

  const defaultName = () => {
    const used = favorites
      .map((f) => /^(?:お気に入り|Favorite )(\d+)$/.exec(f.name)?.[1])
      .filter((n): n is string => Boolean(n))
      .map(Number);
    return t("favorite.defaultName", { n: used.length ? Math.max(...used) + 1 : 1 });
  };

  const summary = [
    session.selectedTags.join(" / "),
    session.query.trim() ? t("favorite.queryPart", { query: session.query.trim() }) : "",
    session.period !== "all" ? t("filter.periodDays", { count: Number(session.period) }) : "",
  ]
    .filter(Boolean)
    .join(" ・ ");

  const canSave = session.selectedTags.length > 0 || session.query.trim() !== "";

  return (
    <div className="border-line mb-1.5 border-b pb-1.5">
      <TagCombobox
        allTags={allTags}
        selected={session.selectedTags}
        onAdd={session.addTag}
        onRemove={session.removeTag}
        onClear={session.clearTags}
        labels={{
          placeholder: t("filter.tagPlaceholder"),
          clear: t("filter.clearTags"),
          clearTitle: t("filter.clearTagsTitle"),
          empty: t("list.notFound"),
        }}
      />

      <div className="text-muted mx-2.5 mb-1.5 flex items-center gap-2 text-[11.5px]">
        <label htmlFor="filter-period" className="min-w-[28px]">
          {t("filter.period")}
        </label>
        <Select value={session.period} onValueChange={(v) => session.setPeriod(v as Period)}>
          <SelectTrigger id="filter-period" size="sm" className="flex-1">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {PERIODS.map((p) => (
              <SelectItem key={p} value={p}>
                {p === "all" ? t("filter.periodAll") : t("filter.periodDays", { count: Number(p) })}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      <div className="text-muted mx-2.5 mb-1.5 flex items-center gap-2 text-[11.5px]">
        <label htmlFor="filter-sort" className="min-w-[28px]">
          {t("filter.sort")}
        </label>
        <Select value={session.sort} onValueChange={(v) => session.setSort(v as SortKey)}>
          <SelectTrigger id="filter-sort" size="sm" className="flex-1">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {SORTS.map((s) => (
              <SelectItem key={s} value={s}>
                {s === "updated"
                  ? t("filter.sortUpdated")
                  : s === "created"
                    ? t("filter.sortCreated")
                    : t("filter.sortTitle")}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      <div className="mx-2.5 mt-2 mb-0.5 flex flex-wrap items-center gap-1.5">
        {session.activeFav ? (
          <>
            <span className="text-grow mr-0.5 flex items-center gap-1 text-[11.5px]">
              <Icon as={Star} size="sm" className="fill-current" />
              {session.activeFav}
            </span>
            <Button
              size="sm"
              onClick={() => {
                // 同名で上書き
                favoriteAdd.mutate(currentFilter(session.activeFav!), {
                  onSuccess: () => toast(t("favorite.updated", { name: session.activeFav })),
                });
              }}
            >
              {t("common:action.update")}
            </Button>
            <Button variant="quiet" size="sm" onClick={() => setPrompt("rename")}>
              {t("common:action.rename")}
            </Button>
            <Button variant="quiet" size="sm" onClick={() => setPrompt("save")}>
              {t("common:action.saveAs")}
            </Button>
          </>
        ) : (
          <Button
            size="sm"
            onClick={() => (canSave ? setPrompt("save") : toast(t("favorite.needCondition")))}
          >
            <Icon as={Star} size="sm" />
            {t("favorite.save")}
          </Button>
        )}
      </div>

      <NamePromptDialog
        open={prompt !== null}
        title={prompt === "rename" ? t("favorite.askRename") : t("favorite.askSave")}
        description={summary}
        initialValue={prompt === "rename" ? (session.activeFav ?? "") : defaultName()}
        labels={{ save: t("common:action.save"), cancel: t("common:action.cancel") }}
        onOpenChange={(open) => !open && setPrompt(null)}
        onSubmit={(name) => {
          const previous = session.activeFav;
          favoriteAdd.mutate(currentFilter(name), {
            onSuccess: () => {
              if (prompt === "rename" && previous && previous !== name) {
                favoriteRemove.mutate(previous);
              }
              session.setActiveFav(name);
              toast(prompt === "rename" ? t("common:toast.renamed") : t("favorite.saved"));
            },
            onError: (e) => toast(errorText(e)),
          });
          setPrompt(null);
        }}
      />
    </div>
  );
}
