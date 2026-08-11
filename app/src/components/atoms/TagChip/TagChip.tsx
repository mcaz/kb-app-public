import { type VariantProps } from "tailwind-variants";

import { tagChipVariants } from "./variants";

export type TagChipProps = VariantProps<typeof tagChipVariants> & {
  tag: string;
  /** 渡すと × が出る(選択中のタグを外す用)。 */
  onRemove?: () => void;
  onClick?: () => void;
  removeLabel?: string;
};

/** タグ1つ分の表示。一覧の絞り込みチップと、本文のタグチップの両方に使う。 */
export function TagChip({ tag, selected, onRemove, onClick, removeLabel }: TagChipProps) {
  const { root, remove } = tagChipVariants({ selected });
  const content = (
    <>
      {tag}
      {onRemove && (
        <button
          type="button"
          className={remove()}
          aria-label={removeLabel ?? `${tag} ×`}
          onClick={(e) => {
            e.stopPropagation();
            onRemove();
          }}
        >
          ×
        </button>
      )}
    </>
  );

  if (onClick) {
    return (
      <button type="button" className={root()} onClick={onClick}>
        {content}
      </button>
    );
  }
  return <span className={root()}>{content}</span>;
}
