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
  /** [`usePurgeFlow`] の戻り値をそのまま渡す。結線を画面ごとに書き写さない。 */
  flow: {
    target: PurgePlan | null;
    busy: boolean;
    confirmPurge: () => void | Promise<void>;
    clear: () => void;
  };
}

/**
 * purge の確認。**「完全に削除」とは書かない** — `full` の実体は履歴に残り、
 * fresh clone すれば取得できる(ADR kb-app/artifact-deletion)。
 * 画面が回収を約束できるのは、この端末のディスクと以降の同期だけ。
 */
export function PurgeDialog({ flow }: PurgeDialogProps) {
  const { t } = useTranslation("notes");
  const plan = flow.target;
  if (!plan) return null;

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) flow.clear();
      }}
    >
      <DialogContent className="min-w-[320px]">
        <DialogHeader>
          <DialogTitle>{t("file.purgeTitle", { name: plan.display_name })}</DialogTitle>
          <DialogDescription>{t("file.purgeScope")}</DialogDescription>
        </DialogHeader>

        <ul className="text-muted flex list-disc flex-col gap-1 pl-4 text-[12px]">
          {/* まだノートが持っているものを消すと、そのノートからファイルが消える。
              一覧からは参照中でも押せるので、ここで件数を見せて止める機会を作る */}
          {plan.notes.length > 0 && (
            <li className="text-attn">{t("file.purgeStillUsed", { count: plan.notes.length })}</li>
          )}
          {plan.shares_object_with.length > 0 && (
            <li>{t("file.purgeShared", { count: plan.shares_object_with.length })}</li>
          )}
          {/* 参照名は本文リンク(kb-artifact-ref:)の宛先。ファイルごと消える以上
              残しても宙に浮くので一緒に外すが、黙って外さない */}
          {plan.refs.length > 0 && (
            <li>
              {t("file.purgeDropsRefs", { count: plan.refs.length, names: plan.refs.join("、") })}
            </li>
          )}
          {plan.superseded_by.length > 0 && <li>{t("file.purgeSuperseded")}</li>}
          {plan.needs_confirmation && <li className="text-attn">{t("file.purgeOnlyCopy")}</li>}
        </ul>

        <div className="flex justify-end gap-2">
          <Button variant="quiet" size="sm" disabled={flow.busy} onClick={() => flow.clear()}>
            {t("file.purgeCancel")}
          </Button>
          <Button size="sm" disabled={flow.busy} onClick={() => void flow.confirmPurge()}>
            {t("file.purgeConfirm")}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
