import { useTranslation } from "react-i18next";

import { formatDateTime } from "@/lib/format";

export interface NoteSummaryCardProps {
  title: string;
  description?: string | null;
  tags?: string[];
  createdAt: string | null;
  updatedAt: string | null;
  selected?: boolean;
  onOpen: () => void;
}

/** 一覧画面で共通して使う、概要と日時を含むノートカード。 */
export function NoteSummaryCard({
  title,
  description,
  tags = [],
  createdAt,
  updatedAt,
  selected = false,
  onOpen,
}: NoteSummaryCardProps) {
  const { t, i18n } = useTranslation("common");
  const at = (value: string | null) => formatDateTime(value, i18n.language, t("date.unknown"));

  return (
    <button
      type="button"
      aria-current={selected ? "page" : undefined}
      onClick={onOpen}
      className={`group w-full cursor-pointer rounded-2xl border px-5 py-4 text-left transition-colors max-[800px]:rounded-xl max-[800px]:px-4 max-[800px]:py-3.5 ${
        selected
          ? "border-grow bg-grow-soft text-ink"
          : "border-line bg-panel-2 text-ink hover:border-grow hover:bg-sel"
      }`}
    >
      <span className="block text-[17px] leading-snug font-bold break-words whitespace-normal">
        {title}
      </span>

      {description && (
        <span className="text-muted mt-1.5 block text-[13px] leading-relaxed break-words whitespace-pre-wrap">
          {description}
        </span>
      )}

      {tags.length > 0 && (
        <span className="mt-2 flex flex-wrap gap-1.5">
          {tags.map((tag) => (
            <span
              key={tag}
              className="border-line bg-chip text-muted rounded-full border px-2 py-0.5 text-[10.5px]"
            >
              {tag}
            </span>
          ))}
        </span>
      )}

      <span className="text-muted mt-2.5 flex flex-wrap gap-x-4 gap-y-0.5 text-xs tabular-nums">
        <span>{t("date.created", { value: at(createdAt) })}</span>
        <span>{t("date.updated", { value: at(updatedAt) })}</span>
      </span>
    </button>
  );
}
