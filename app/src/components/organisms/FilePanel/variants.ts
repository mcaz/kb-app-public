import { tv } from "tailwind-variants";

/**
 * 行の状態は**左枠線と語**で出す(正本の画面設計)。
 * 「同期しない」は正常な選択なので警告色にしない — 琥珀は「手元に無い」だけ。
 */
export const fileRowVariants = tv({
  base: "border-line bg-panel flex min-w-0 flex-wrap items-center gap-2 rounded-lg border-l-[3px] px-3 py-1.5 text-[13px]",
  variants: {
    state: {
      local: "border-l-line",
      missing: "border-l-prop",
      unavailable: "border-l-muted",
      legacy: "border-l-line opacity-80",
    },
  },
  defaultVariants: { state: "local" },
});
