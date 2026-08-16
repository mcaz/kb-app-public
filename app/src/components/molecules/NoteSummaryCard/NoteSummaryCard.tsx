import { Link2, Paperclip, Sparkles } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";
import { formatDateTime } from "@/lib/format";

export interface NoteSummaryCardProps {
  title: string;
  description?: string | null;
  tags?: string[];
  createdAt: string | null;
  updatedAt: string | null;
  linkedCount?: number;
  hasSimilar?: boolean | null;
  fileCount?: number;
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
  linkedCount,
  hasSimilar,
  fileCount,
  selected = false,
  onOpen,
}: NoteSummaryCardProps) {
  const { t, i18n } = useTranslation("common");
  const at = (value: string | null) => formatDateTime(value, i18n.language, t("date.unknown"));
  const showSignals =
    linkedCount !== undefined || hasSimilar !== undefined || fileCount !== undefined;
  const signalClass = (active: boolean) =>
    `inline-flex items-center gap-1 rounded-full border px-2 py-1 text-[11px] ${
      active ? "border-grow/40 bg-grow-soft text-ink" : "border-line bg-chip text-muted"
    }`;
  const similarState =
    hasSimilar === null
      ? t("noteSignals.unknown")
      : hasSimilar
        ? t("noteSignals.available")
        : t("noteSignals.empty");

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

      {showSignals && (
        <span className="mt-2.5 flex flex-wrap gap-1.5">
          {linkedCount !== undefined && (
            <span className={signalClass(linkedCount > 0)}>
              <Icon as={Link2} size="sm" />
              {t("noteSignals.linked", { count: linkedCount })}
            </span>
          )}
          {hasSimilar !== undefined && (
            <span className={signalClass(hasSimilar === true)}>
              <Icon as={Sparkles} size="sm" />
              {t("noteSignals.similar", { state: similarState })}
            </span>
          )}
          {fileCount !== undefined && (
            <span className={signalClass(fileCount > 0)}>
              <Icon as={Paperclip} size="sm" />
              {t("noteSignals.attachments", { count: fileCount })}
            </span>
          )}
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
