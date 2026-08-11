import { tv, type VariantProps } from "tailwind-variants";

const chip = tv({
  slots: {
    root: "inline-flex items-center gap-1 rounded-full text-[11.5px]",
    remove: "cursor-pointer border-none bg-transparent px-1 text-inherit",
  },
  variants: {
    selected: {
      true: { root: "border border-grow bg-grow-soft py-px pr-1 pl-2.5 text-grow" },
      false: {
        root: "cursor-pointer border border-line bg-chip px-2.5 py-px text-muted hover:border-grow hover:text-ink",
      },
    },
  },
  defaultVariants: { selected: false },
});

export type TagChipProps = VariantProps<typeof chip> & {
  tag: string;
  /** 渡すと × が出る(選択中のタグを外す用)。 */
  onRemove?: () => void;
  onClick?: () => void;
  removeLabel?: string;
};

/** タグ1つ分の表示。一覧の絞り込みチップと、本文のタグチップの両方に使う。 */
export function TagChip({ tag, selected, onRemove, onClick, removeLabel }: TagChipProps) {
  const { root, remove } = chip({ selected });
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
