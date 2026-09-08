import { tv } from "tailwind-variants";

export const fileFilterVariants = tv({
  slots: {
    search:
      "border-line bg-panel-2 text-ink h-10.5 w-full rounded-lg border py-2.5 pr-3 pl-10 text-[13px]",
    kind: "bg-panel-2 w-40 data-[size=default]:h-10.5",
  },
});

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
