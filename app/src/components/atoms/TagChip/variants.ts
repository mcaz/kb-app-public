import { tv } from "tailwind-variants";

export const tagChipVariants = tv({
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
