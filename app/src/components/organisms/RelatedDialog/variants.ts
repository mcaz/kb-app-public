import { tv } from "tailwind-variants";

/** 行は検索Modalの結果行と同じ形。つながりと近いノートの区別はタブ側の色が持つ。 */
export const relatedItemVariants = tv({
  base: "flex cursor-pointer flex-col items-stretch gap-1 border border-transparent px-3 py-2.5 data-[selected=true]:border-line",
});

/** 面の色(つながり=緑 / 近いノート=琥珀)はここだけが持つ。 */
export const relatedTabVariants = tv({
  base: "flex cursor-pointer items-center gap-1.5 rounded-t-md border-b-2 border-transparent bg-transparent px-3 py-1.5 text-xs",
  variants: {
    tone: { linked: "", similar: "" },
    active: { true: "font-semibold", false: "text-muted hover:text-ink" },
  },
  compoundVariants: [
    { tone: "linked", active: true, class: "border-grow bg-grow-soft text-grow" },
    { tone: "similar", active: true, class: "border-prop bg-prop-soft text-prop" },
  ],
});
