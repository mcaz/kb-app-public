import { tv } from "tailwind-variants";

/**
 * 行そのものは検索Modalの結果行と同じ形にし、つながり(緑)と近いノート(琥珀)は
 * 左端の2pxだけで見分ける。3行になった行を色面で塗ると一覧が読めなくなるため。
 * data-selected 側にも tone を書くのは、選択枠の border-line が左端まで上書きするから。
 */
export const relatedItemVariants = tv({
  base: "flex cursor-pointer flex-col items-stretch gap-1 border border-l-2 border-transparent px-3 py-2.5 data-[selected=true]:border-line",
  variants: {
    tone: {
      linked: "border-l-grow data-[selected=true]:border-l-grow",
      similar: "border-l-prop data-[selected=true]:border-l-prop",
    },
  },
});
