import { tv } from "tailwind-variants";

export const statusPillVariants = tv({
  base: "rounded-full px-2.5 py-px text-[11px] whitespace-nowrap",
  variants: {
    tone: {
      muted: "border border-line bg-panel-2 text-muted",
      prop: "bg-prop-soft text-prop",
      grow: "bg-grow-soft text-grow",
    },
  },
  defaultVariants: { tone: "muted" },
});
