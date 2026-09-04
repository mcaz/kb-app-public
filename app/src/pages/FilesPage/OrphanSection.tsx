import { useTranslation } from "react-i18next";

import { Button } from "@/components/atoms/ui/button";
import { formatSize } from "@/lib/format";
import { useFilesOrphans } from "@/lib/queries";

export interface OrphanSectionProps {
  onPurge: (id: string) => void;
  busy?: boolean;
}

/**
 * どのノートからも外れたファイル。
 *
 * 一覧のカードにも同じ操作があるが、こちらは**片付ける対象だけ**を集める。
 * ADR kb-app/artifact-deletion の言う「整理の下見」で、見えることが本体。
 * 溜まっていなければ節ごと出さない(片付けるものが無いのに掃除の口を見せない)。
 */
export function OrphanSection({ onPurge, busy = false }: OrphanSectionProps) {
  const { t } = useTranslation("files");
  const { data } = useFilesOrphans();
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
              disabled={busy}
              onClick={() => onPurge(file.id)}
            >
              {t("orphans.purge")}
            </Button>
          </li>
        ))}
      </ul>
    </section>
  );
}
