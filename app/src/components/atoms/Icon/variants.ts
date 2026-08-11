import { tv } from "tailwind-variants";

export const iconVariants = tv({
  base: "shrink-0",
  variants: {
    size: {
      /** 一覧など密な行(現行の 11.5〜12px の文字に釣り合う) */
      sm: "size-3.5",
      /** ナビ・本文の既定 */
      md: "size-4",
      /** ホームのタイルなど */
      lg: "size-5",
    },
  },
  defaultVariants: { size: "md" },
});
