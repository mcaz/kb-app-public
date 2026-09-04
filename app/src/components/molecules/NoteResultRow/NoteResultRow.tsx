import { useTranslation } from "react-i18next";

import { formatDateTime } from "@/lib/format";

export interface NoteResultRowProps {
  title: string;
  /** RFC3339。無ければ日付を出さない。 */
  updated?: string | null;
  snippet?: string;
  tags?: string[];
  /** 近いノートだけ。コサイン距離を右端に出す。 */
  distance?: number | null;
}

/**
 * 一覧の1行(題名・更新・抜粋・タグ)。関連 Modal とファイルの参照ノートで共有する。
 *
 * 題名だけの行は「どのノートだったか」を思い出せない。選ぶ前に中身の見当が
 * 付くよう、判断材料をここへ集める。行の枠と選択状態は置く側が持つ。
 */
export function NoteResultRow({
  title,
  updated = null,
  snippet = "",
  tags = [],
  distance = null,
}: NoteResultRowProps) {
  const { t, i18n } = useTranslation("common");

  return (
    <>
      <div className="flex min-w-0 items-start gap-2">
        <span className="line-clamp-2 min-w-0 flex-1 font-semibold">{title}</span>
        {updated && (
          <span className="text-muted shrink-0 text-[10px]">
            {formatDateTime(updated, i18n.language, t("date.unknown"))}
          </span>
        )}
      </div>
      {snippet && <div className="text-muted line-clamp-2 text-xs">{snippet}</div>}
      {(tags.length > 0 || distance != null) && (
        <div className="text-muted flex items-center gap-2 text-[10.5px]">
          <span className="min-w-0 truncate">{tags.join(" · ")}</span>
          {distance != null && <span className="ml-auto shrink-0">{distance.toFixed(2)}</span>}
        </div>
      )}
    </>
  );
}
