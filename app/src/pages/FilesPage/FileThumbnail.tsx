import { convertFileSrc } from "@tauri-apps/api/core";
import { useState } from "react";

import { Icon } from "@/components/atoms/Icon";
import { IN_TAURI } from "@/lib/api";
import { useFilePreview } from "@/lib/queries";

import { FILE_ICONS, fileKind } from "./fileKind";
import { fileThumbVariants } from "./variants";

import type { FileCard as FileCardData } from "@/lib/api";

export interface FileThumbnailProps {
  file: FileCardData;
  /** 高さは置く場所が決める(一覧のカードと Modal の左パネルで違う)。 */
  className?: string;
}

/**
 * ファイルの見え方。画像なら中身、それ以外は種類のアイコン。
 *
 * 一覧のカードと、プレビュー Modal の左パネルが共有する。取り寄せは提案せず、
 * 手元にある画像だけを出す — 無いものを在るように見せない。
 */
export function FileThumbnail({ file, className }: FileThumbnailProps) {
  const kind = fileKind(file);
  const [failed, setFailed] = useState(false);
  const state =
    file.availability === "missing"
      ? "missing"
      : file.availability === "unavailable_by_policy"
        ? "unavailable"
        : "local";
  const preview = useFilePreview(
    kind === "image" && file.availability === "local" ? file.id : null,
  );
  const src = IN_TAURI && preview.data?.path ? convertFileSrc(preview.data.path) : null;

  return (
    <span className={fileThumbVariants({ state, className })}>
      {src && !failed ? (
        <img
          src={src}
          alt=""
          loading="lazy"
          className="h-full w-full object-contain"
          onError={() => setFailed(true)}
        />
      ) : (
        <Icon as={FILE_ICONS[kind]} size="lg" />
      )}
    </span>
  );
}
