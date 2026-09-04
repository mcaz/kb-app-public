import { tv } from "tailwind-variants";

export const fileCardVariants = tv({
  base: [
    "group min-w-0 cursor-pointer overflow-hidden rounded-xl border text-left transition-colors",
    "border-line bg-panel-2 text-ink hover:border-grow hover:bg-sel",
  ],
});

export const fileThumbVariants = tv({
  base: "border-line bg-panel flex h-44 items-center justify-center overflow-hidden border-b",
  variants: {
    state: {
      local: "text-muted",
      missing: "bg-prop-soft text-prop",
      unavailable: "text-muted opacity-70",
    },
  },
  defaultVariants: { state: "local" },
});

/**
 * プレビュー Modal の面タブ。関連 Modal(`relatedTabVariants`)と同じ形にして、
 * 「左にタブ・右に詳細」という読み方を画面間で揃える。
 */
export const filePaneTabVariants = tv({
  base: "flex cursor-pointer items-center gap-1.5 rounded-t-md border-b-2 border-transparent bg-transparent px-3 py-1.5 text-xs",
  variants: {
    active: {
      true: "border-grow bg-grow-soft text-grow font-semibold",
      false: "text-muted hover:text-ink",
    },
  },
  defaultVariants: { active: false },
});

/** 参照ノートの行。関連 Modal の結果行と同じ見え方に揃える。 */
export const relatedItemVariants = tv({
  base: "flex w-full cursor-pointer flex-col items-stretch gap-1 rounded-md border border-transparent px-3 py-2.5 text-left text-xs",
  variants: {
    active: { true: "border-line bg-sel", false: "hover:bg-sel/60" },
  },
  defaultVariants: { active: false },
});
