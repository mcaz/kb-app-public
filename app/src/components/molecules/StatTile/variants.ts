import { tv } from "tailwind-variants";

export const statTileVariants = tv({
  slots: {
    root: "border-line bg-panel rounded-xl border px-4 py-3.5 text-left",
    num: "text-[22px] leading-tight font-bold",
    label: "text-muted mt-0.5 flex items-center gap-1.5 text-xs",
  },
  variants: {
    amber: { true: { root: "border-prop", num: "text-prop" } },
    clickable: { true: { root: "w-full cursor-pointer hover:bg-sel" } },
  },
});
