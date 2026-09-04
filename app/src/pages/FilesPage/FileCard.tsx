import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";
import { formatDateTime, formatSize } from "@/lib/format";

import { FILE_ICONS, fileKind } from "./fileKind";
import { FileThumbnail } from "./FileThumbnail";
import { PurgeButton } from "./PurgeButton";
import { fileCardVariants } from "./variants";

import type { FileCard as FileCardData } from "@/lib/api";

export interface FileCardProps {
  file: FileCardData;
  onOpen: () => void;
  onPurge: () => void;
  busy?: boolean;
}

export function FileCard({ file, onOpen, onPurge, busy = false }: FileCardProps) {
  const { t, i18n } = useTranslation("files");
  const kind = fileKind(file);
  const date = formatDateTime(file.added_at, i18n.language, "—").slice(0, 10);
  // 1つのファイルを複数のノートが持てる。**何本から参照されているか**が
  // 取り除いてよいかの判断材料になるので、題名より件数を先に出す
  const references =
    file.notes.length === 0
      ? t("references.none")
      : file.notes.length === 1
        ? (file.notes[0]?.title ?? t("noNote"))
        : t("references.many", { count: file.notes.length });

  return (
    // カード全体を button にすると中へ操作を足せない(button の入れ子は不正)。
    // 開く口は内側の button に閉じ込め、操作はその外に並べる
    <div className={fileCardVariants()}>
      <button type="button" className="block w-full min-w-0 text-left" onClick={onOpen}>
        <FileThumbnail file={file} />
        <span className="block min-w-0 p-[12px_12px_4px_12px]">
          <span className="line-clamp-2 text-[15px] leading-[1.35] font-bold break-all">
            {file.name}
          </span>
          <span
            className={
              file.notes.length === 0
                ? "text-prop mt-1 line-clamp-2 text-[12.5px] leading-[1.35] break-words"
                : "text-muted mt-1 line-clamp-2 text-[12.5px] leading-[1.35] break-words"
            }
          >
            {references}
          </span>
          <span className="text-muted mt-2 flex min-w-0 items-center justify-between gap-2 text-xs">
            <span className="flex min-w-0 items-center gap-1.5">
              <Icon as={FILE_ICONS[kind]} size="sm" />
              <span className="truncate">{t(`filter.${kind}`)}</span>
              {file.availability === "local" && <span>· {formatSize(file.size)}</span>}
            </span>
            <span className="shrink-0">{date}</span>
          </span>
        </span>
      </button>

      <div className="flex justify-end px-2 pb-2">
        <PurgeButton disabled={busy} onClick={onPurge} />
      </div>
    </div>
  );
}
