import { useTranslation } from "react-i18next";

import { Button } from "@/components/atoms/ui/button";
import { PurgeDialog } from "@/components/molecules/PurgeDialog";
import { usePurgeFlow } from "@/hooks/usePurgeFlow";
import { formatSize } from "@/lib/format";
import { useFilesOrphans } from "@/lib/queries";

/**
 * どのノートからも外れたファイル。
 *
 * 「このノートから外す」は実体を消さないので、拾う場所が無いと置き場に溜まり続ける。
 * ここが ADR kb-app/artifact-deletion の言う「整理の下見」で、**見えることが本体**。
 * 溜まっていなければ節ごと出さない(片付けるものが無いのに掃除の口を見せない)。
 */
export function OrphanSection() {
  const { t } = useTranslation("files");
  const { data } = useFilesOrphans();
  const purge = usePurgeFlow();
  const orphans = data ?? [];

  if (orphans.length === 0) return null;

  return (
    <section aria-label={t("orphans.label")} className="mt-10">
      <h2 className="text-[15px] font-medium">{t("orphans.title")}</h2>
      <p className="text-muted mt-1 text-xs">{t("orphans.help")}</p>

      <ul className="border-line divide-line mt-3 list-none divide-y rounded-xl border p-0">
        {orphans.map((file) => (
          <li key={file.id} className="flex items-center gap-2 px-4 py-2.5 text-[13px]">
            <span className="min-w-0 break-all">{file.name}</span>
            <i className="text-muted text-[11px] not-italic">{formatSize(file.size)}</i>
            <Button
              variant="quiet"
              size="sm"
              className="ml-auto"
              disabled={purge.busy}
              onClick={() => void purge.planPurge(file.id)}
            >
              {t("orphans.purge")}
            </Button>
          </li>
        ))}
      </ul>

      <PurgeDialog
        plan={purge.target}
        busy={purge.busy}
        onConfirm={() => void purge.confirmPurge()}
        onOpenChange={(open) => {
          if (!open) purge.clear();
        }}
      />
    </section>
  );
}
