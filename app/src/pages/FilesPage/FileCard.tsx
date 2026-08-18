import { convertFileSrc } from "@tauri-apps/api/core";
import { Braces, FileImage, FileText, FileType2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";
import { IN_TAURI } from "@/lib/api";
import { formatDateTime, formatSize } from "@/lib/format";
import { useFilePreview } from "@/lib/queries";

import { fileKind } from "./fileKind";
import { fileCardVariants, fileThumbVariants } from "./variants";

import type { FileCard as FileCardData } from "@/lib/api";

const ICONS = { pdf: FileText, image: FileImage, document: Braces, other: FileType2 } as const;

function FileThumbnail({ file }: { file: FileCardData }) {
  const kind = fileKind(file);
  const [failed, setFailed] = useState(false);
  const preview = useFilePreview(
    kind === "image" && file.availability === "local" ? file.id : null,
  );
  const src = IN_TAURI && preview.data?.path ? convertFileSrc(preview.data.path) : null;

  if (src && !failed) {
    return (
      <img
        src={src}
        alt=""
        loading="lazy"
        className="h-full w-full object-contain"
        onError={() => setFailed(true)}
      />
    );
  }

  return <Icon as={ICONS[kind]} size="lg" />;
}

export function FileCard({ file, onOpen }: { file: FileCardData; onOpen: () => void }) {
  const { t, i18n } = useTranslation("files");
  const kind = fileKind(file);
  const state =
    file.availability === "missing"
      ? "missing"
      : file.availability === "unavailable_by_policy"
        ? "unavailable"
        : "local";
  const note = file.notes[0]?.title ?? t("noNote");
  const date = formatDateTime(file.added_at, i18n.language, "—").slice(0, 10);

  return (
    <button type="button" className={fileCardVariants()} onClick={onOpen}>
      <span className={fileThumbVariants({ state })}>
        <FileThumbnail file={file} />
      </span>
      <span className="block min-w-0 p-[12px_12px_12px_12px]">
        <span className="line-clamp-2 text-[15px] leading-[1.35] font-bold break-all">
          {file.name}
        </span>
        <span className="text-muted mt-1 line-clamp-2 text-[12.5px] leading-[1.35] break-words">
          {note}
        </span>
        <span className="text-muted mt-2 flex min-w-0 items-center justify-between gap-2 text-xs">
          <span className="flex min-w-0 items-center gap-1.5">
            <Icon as={ICONS[kind]} size="sm" />
            <span className="truncate">{t(`filter.${kind}`)}</span>
            {file.availability === "local" && <span>· {formatSize(file.size)}</span>}
          </span>
          <span className="shrink-0">{date}</span>
        </span>
      </span>
    </button>
  );
}
