import { useTranslation } from "react-i18next";

import { Button } from "@/components/atoms/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/atoms/ui/dialog";

import type { PurgePlan } from "@/lib/api";

export interface PurgeDialogProps {
  plan: PurgePlan | null;
  busy?: boolean;
  onConfirm: () => void;
  onOpenChange: (open: boolean) => void;
}

/**
 * purge の確認。**「完全に削除」とは書かない** — `full` の実体は履歴に残り、
 * fresh clone すれば取得できる(ADR kb-app/artifact-deletion)。
 * 画面が回収を約束できるのは、この端末のディスクと以降の同期だけ。
 */
export function PurgeDialog({ plan, busy = false, onConfirm, onOpenChange }: PurgeDialogProps) {
  const { t } = useTranslation("notes");
  if (!plan) return null;

  return (
    <Dialog open onOpenChange={onOpenChange}>
      <DialogContent className="min-w-[320px]">
        <DialogHeader>
          <DialogTitle>{t("file.purgeTitle", { name: plan.display_name })}</DialogTitle>
          <DialogDescription>{t("file.purgeScope")}</DialogDescription>
        </DialogHeader>

        <ul className="text-muted flex list-disc flex-col gap-1 pl-4 text-[12px]">
          {plan.shares_object_with.length > 0 && (
            <li>{t("file.purgeShared", { count: plan.shares_object_with.length })}</li>
          )}
          {plan.superseded_by.length > 0 && <li>{t("file.purgeSuperseded")}</li>}
          {plan.needs_confirmation && <li className="text-attn">{t("file.purgeOnlyCopy")}</li>}
        </ul>

        <div className="flex justify-end gap-2">
          <Button variant="quiet" size="sm" disabled={busy} onClick={() => onOpenChange(false)}>
            {t("file.purgeCancel")}
          </Button>
          <Button size="sm" disabled={busy} onClick={onConfirm}>
            {t("file.purgeConfirm")}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
