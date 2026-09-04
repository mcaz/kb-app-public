import { useTranslation } from "react-i18next";

import { formatSize } from "@/lib/format";

import { FileThumbnail } from "./FileThumbnail";
import { PurgeButton } from "./PurgeButton";

import type { FileCard } from "@/lib/api";

export interface OrphanSectionProps {
  /** 一覧の部分集合。どのノートからも外れたもの。 */
  files: FileCard[];
  onPurge: (id: string) => void;
  busy?: boolean;
}

/**
 * どのノートからも外れたファイル。
 *
 * 一覧のカードにも同じ操作があるが、こちらは**片付ける対象だけ**を集める。
 * ADR kb-app/artifact-deletion の言う「整理の下見」で、見えることが本体。
 * 溜まっていなければ節ごと出さない(片付けるものが無いのに掃除の口を見せない)。
 *
 * 集合は一覧のペイロードから引き算で出す。台帳を引き直すと「現行ファイルとは何か」
 * の定義が2つになり、片方だけ変わったときに食い違う。
 */
export function OrphanSection({ files, onPurge, busy = false }: OrphanSectionProps) {
  const { t } = useTranslation("files");
  if (files.length === 0) return null;

  return (
    <section aria-label={t("orphans.label")} className="mt-10">
      <h2 className="text-[15px] font-medium">{t("orphans.title")}</h2>
      <p className="text-muted mt-1 text-xs">{t("orphans.help")}</p>

      <ul className="border-line divide-line mt-3 list-none divide-y rounded-xl border p-0">
        {files.map((file) => (
          <li key={file.id} className="flex items-center gap-3 px-4 py-2.5 text-[13px]">
            <FileThumbnail file={file} className="size-9 shrink-0 rounded border" />
            <span className="min-w-0 break-all">{file.name}</span>
            <i className="text-muted text-[11px] not-italic">{formatSize(file.size)}</i>
            <PurgeButton className="ml-auto" disabled={busy} onClick={() => onPurge(file.id)} />
          </li>
        ))}
      </ul>
    </section>
  );
}
