import { Star } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Icon } from "@/components/atoms/Icon";
import { Button } from "@/components/atoms/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/atoms/ui/dialog";
import { NamePromptDialog } from "@/components/molecules/NamePromptDialog";
import type { Favorite } from "@/lib/api";
import { useFavoriteAdd, useFavoriteRemove, useFavorites } from "@/lib/queries";
import { useSession } from "@/lib/stores/session";

export interface FavoritesDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/** お気に入りの管理(適用・名前変更・削除)。サイドバーから開く。 */
export function FavoritesDialog({ open, onOpenChange }: FavoritesDialogProps) {
  const { t } = useTranslation(["notes", "common"]);
  const { data: favorites = [] } = useFavorites();
  const add = useFavoriteAdd();
  const remove = useFavoriteRemove();
  const applyFavorite = useSession((s) => s.applyFavorite);
  const activeFav = useSession((s) => s.activeFav);
  const setActiveFav = useSession((s) => s.setActiveFav);
  const [renaming, setRenaming] = useState<Favorite | null>(null);

  const describe = (f: Favorite) =>
    [
      f.tags.join(" / "),
      f.query ? t("favorite.queryPart", { query: f.query }) : "",
      f.period && f.period !== "all" ? t("filter.periodDays", { count: Number(f.period) }) : "",
    ]
      .filter(Boolean)
      .join(" ・ ") || t("favorite.noCondition");

  return (
    <>
      <Dialog open={open} onOpenChange={onOpenChange}>
        <DialogContent className="min-w-[460px] sm:max-w-[620px]">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <Icon as={Star} />
              {t("favorite.title")}
            </DialogTitle>
          </DialogHeader>

          <div className="flex max-h-[50vh] flex-col gap-1.5 overflow-y-auto">
            {favorites.length === 0 ? (
              <div className="text-muted text-xs">{t("favorite.empty")}</div>
            ) : (
              favorites.map((fav) => (
                <div
                  key={fav.name}
                  className={`bg-panel flex items-center gap-1.5 rounded-lg border px-2 py-1.5 ${
                    activeFav === fav.name ? "border-grow bg-sel" : "border-line"
                  }`}
                >
                  <button
                    type="button"
                    className="text-ink flex min-w-0 flex-1 cursor-pointer flex-col items-start gap-px border-none bg-transparent px-1 py-0.5 text-left"
                    onClick={() => {
                      applyFavorite(fav);
                      onOpenChange(false);
                    }}
                  >
                    <span className="text-[13px] font-semibold">{fav.name}</span>
                    <span className="text-muted max-w-full truncate text-[11px]">
                      {describe(fav)}
                    </span>
                  </button>
                  <Button variant="quiet" size="sm" onClick={() => setRenaming(fav)}>
                    {t("common:action.rename")}
                  </Button>
                  <Button
                    variant="quiet"
                    size="sm"
                    onClick={() =>
                      remove.mutate(fav.name, {
                        onSuccess: () => {
                          if (activeFav === fav.name) setActiveFav(null);
                          toast(t("common:toast.deleted"));
                        },
                      })
                    }
                  >
                    {t("common:action.delete")}
                  </Button>
                </div>
              ))
            )}
          </div>

          <DialogFooter>
            <Button variant="quiet" onClick={() => onOpenChange(false)}>
              {t("common:action.close")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <NamePromptDialog
        open={renaming !== null}
        title={t("favorite.askRename")}
        initialValue={renaming?.name ?? ""}
        labels={{ save: t("common:action.save"), cancel: t("common:action.cancel") }}
        onOpenChange={(o) => !o && setRenaming(null)}
        onSubmit={(name) => {
          const target = renaming;
          if (!target) return;
          add.mutate(
            { ...target, name },
            {
              onSuccess: () => {
                if (name !== target.name) remove.mutate(target.name);
                if (activeFav === target.name) setActiveFav(name);
                toast(t("common:toast.renamed"));
              },
              onError: (e) => toast(String(e)),
            },
          );
          setRenaming(null);
        }}
      />
    </>
  );
}
