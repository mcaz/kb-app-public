import type { TagInfo } from "@/lib/api";

export interface TagRowProps {
  tag: TagInfo;
  noDescriptionLabel: string;
  onClick: () => void;
}

/** ホームのタグ一覧の1行(タグ名・件数・説明)。 */
export function TagRow({ tag, noDescriptionLabel, onClick }: TagRowProps) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="border-line bg-panel text-ink hover:border-grow hover:bg-sel grid grid-cols-[minmax(90px,auto)_34px_1fr] items-baseline gap-2.5 rounded-lg border px-3 py-1.5 text-left text-[12.5px]"
    >
      <span className="text-grow font-semibold">{tag.tag}</span>
      <span className="text-muted text-right text-[11px]">{tag.count}</span>
      <span className="text-muted truncate">
        {tag.description ?? <i className="opacity-60">{noDescriptionLabel}</i>}
      </span>
    </button>
  );
}
