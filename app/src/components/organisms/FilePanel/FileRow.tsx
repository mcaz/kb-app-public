import { FileText, Link2 } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";
import { StatusPill } from "@/components/atoms/StatusPill";
import { Button } from "@/components/atoms/ui/button";
import { formatSize } from "@/lib/format";

import { fileRowVariants } from "./variants";

import type { FileRow as FileRowData } from "@/lib/api";

export interface FileRowProps {
  file: FileRowData;
  onDetach: () => void;
  onPurge: () => void;
  onReplace: () => void;
  onFetch: () => void;
  onOpen: () => void;
  busy?: boolean;
}

/** ファイル1行。FilePanel 専用なので同じディレクトリに置く。 */
export function FileRow({
  file,
  onDetach,
  onPurge,
  onReplace,
  onFetch,
  onOpen,
  busy = false,
}: FileRowProps) {
  const { t } = useTranslation("notes");
  const missing = file.availability === "missing";
  const blocked = file.availability === "unavailable_by_policy";

  return (
    <li
      className={fileRowVariants({
        state: missing ? "missing" : blocked ? "unavailable" : "local",
      })}
    >
      <Icon as={file.linked ? Link2 : FileText} size="sm" className="text-muted" />

      {/* 名前そのものを開く口にする。行の幅を増やさずに済み、開けない行では
          押せる見た目にしない(手元に無いものに触れる余地を残さない) */}
      {file.availability === "local" ? (
        <button
          type="button"
          className="hover:text-grow min-w-0 cursor-pointer border-none bg-transparent p-0 text-left break-all"
          disabled={busy}
          onClick={onOpen}
        >
          {file.name}
        </button>
      ) : (
        <span className="min-w-0 break-all">{file.name}</span>
      )}

      {/* 手元に無い行は大きさも要約も出さない — 中身を知っているかのように見せない */}
      {!missing && <i className="text-muted text-[11px] not-italic">{formatSize(file.size)}</i>}

      {missing && <StatusPill tone="prop">{t("file.missing")}</StatusPill>}
      {blocked && <StatusPill>{t("file.unavailable")}</StatusPill>}
      {file.sync === "local_only" && !blocked && <StatusPill>{t("file.localOnly")}</StatusPill>}
      {file.linked && <StatusPill>{t("file.linked")}</StatusPill>}

      <span className="ml-auto flex flex-wrap items-center gap-1">
        {file.can_fetch && (
          <Button variant="quiet" size="sm" disabled={busy} onClick={onFetch}>
            {t("file.fetch")}
          </Button>
        )}
        <Button variant="quiet" size="sm" disabled={busy} onClick={onReplace}>
          {t("file.newVersion")}
        </Button>
        <Button variant="quiet" size="sm" disabled={busy} onClick={onDetach}>
          {t("file.detach")}
        </Button>
        {/* 実体まで消す。押した先で範囲を明示して確認する(履歴からは消えない) */}
        <Button variant="quiet" size="sm" disabled={busy} onClick={onPurge}>
          {t("file.purge")}
        </Button>
      </span>
    </li>
  );
}
