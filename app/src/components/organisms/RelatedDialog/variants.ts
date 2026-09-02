import { tv } from "tailwind-variants";

/**
 * つながり(緑)と近いノート(琥珀)を色で見分ける。右ペインの RelatedList と同じ配色。
 * data-selected の指定を tone ごとに書くのは、CommandItem 既定の bg-accent が
 * tone の背景を上書きして選択中だけ色が消えるため。
 */
export const relatedItemVariants = tv({
  base: "flex cursor-pointer items-center justify-between gap-2 rounded-md border px-2.5 py-2 text-xs",
  variants: {
    tone: {
      linked:
        "border-grow-soft bg-grow-soft text-grow hover:border-grow data-[selected=true]:border-ink data-[selected=true]:bg-grow-soft data-[selected=true]:text-grow",
      similar:
        "border-prop-soft bg-prop-soft text-prop hover:border-prop data-[selected=true]:border-ink data-[selected=true]:bg-prop-soft data-[selected=true]:text-prop",
    },
  },
});
