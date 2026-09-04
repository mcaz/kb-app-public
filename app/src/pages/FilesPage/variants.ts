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
