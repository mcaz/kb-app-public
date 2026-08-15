import { Star } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Icon } from "@/components/atoms/Icon";
import { Button } from "@/components/atoms/ui/button";
import { NamePromptDialog } from "@/components/molecules/NamePromptDialog";
import { useErrorText } from "@/hooks/useErrorText";
import type { Favorite, Period, SortKey } from "@/lib/api";
import { useFavoriteAdd, useFavoriteRemove, useFavorites } from "@/lib/queries";
import { useSession } from "@/lib/stores/session";

interface SavedSearchActionsProps {
  query: string;
  tags: string[];
  period: Period;
  sort: SortKey;
}

/** 現在のModal検索条件を保存・更新する。 */
export function SavedSearchActions({ query, tags, period, sort }: SavedSearchActionsProps) {
  const { t } = useTranslation(["notes", "common"]);
  const { data: favorites = [] } = useFavorites();
  const activeFav = useSession((state) => state.activeFav);
  const setActiveFav = useSession((state) => state.setActiveFav);
  const errorText = useErrorText();
  const favoriteAdd = useFavoriteAdd();
  const favoriteRemove = useFavoriteRemove();
  const [prompt, setPrompt] = useState<"save" | "rename" | null>(null);

  const currentFilter = (name: string): Favorite => ({
    name,
    tags: [...tags],
    query: query.trim() || null,
    period,
    sort,
  });

  const defaultName = () => {
    const used = favorites
      .map((favorite) => /^(?:お気に入り|Favorite )(\d+)$/.exec(favorite.name)?.[1])
      .filter((value): value is string => Boolean(value))
      .map(Number);
    return t("notes:favorite.defaultName", {
      n: used.length ? Math.max(...used) + 1 : 1,
    });
  };

  const summary = [
    tags.join(" / "),
    query.trim() ? t("notes:favorite.queryPart", { query: query.trim() }) : "",
    period !== "all" ? t("notes:filter.periodDays", { count: Number(period) }) : "",
  ]
    .filter(Boolean)
    .join(" ・ ");

  const canSave = tags.length > 0 || query.trim() !== "" || period !== "all" || sort !== "updated";

  return (
    <>
      <div className="border-line mt-3 flex flex-wrap items-center gap-1.5 border-t pt-3">
        {activeFav ? (
          <>
            <span className="text-grow mr-0.5 flex min-w-0 items-center gap-1 text-[11.5px]">
              <Icon as={Star} size="sm" className="fill-current" />
              <span className="truncate">{activeFav}</span>
            </span>
            <Button
              size="sm"
              onClick={() =>
                favoriteAdd.mutate(currentFilter(activeFav), {
                  onSuccess: () => toast(t("notes:favorite.updated", { name: activeFav })),
                  onError: (error) => toast(errorText(error)),
                })
              }
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
            onClick={() => (canSave ? setPrompt("save") : toast(t("notes:favorite.needCondition")))}
          >
            <Icon as={Star} size="sm" />
            {t("notes:favorite.save")}
          </Button>
        )}
      </div>

      <NamePromptDialog
        open={prompt !== null}
        title={prompt === "rename" ? t("notes:favorite.askRename") : t("notes:favorite.askSave")}
        description={summary}
        initialValue={prompt === "rename" ? (activeFav ?? "") : defaultName()}
        labels={{ save: t("common:action.save"), cancel: t("common:action.cancel") }}
        onOpenChange={(open) => !open && setPrompt(null)}
        onSubmit={(name) => {
          const previous = activeFav;
          favoriteAdd.mutate(currentFilter(name), {
            onSuccess: () => {
              if (prompt === "rename" && previous && previous !== name) {
                favoriteRemove.mutate(previous);
              }
              setActiveFav(name);
              toast(prompt === "rename" ? t("common:toast.renamed") : t("notes:favorite.saved"));
            },
            onError: (error) => toast(errorText(error)),
          });
          setPrompt(null);
        }}
      />
    </>
  );
}
