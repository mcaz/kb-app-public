import { tv } from "tailwind-variants";

export const proposalStatusVariants = tv({
  base: "inline-flex items-center rounded-full px-2.5 py-1 text-[11px] font-medium whitespace-nowrap",
  variants: {
    status: {
      review_pending: "bg-chip text-muted",
      decision_pending: "bg-prop-soft text-prop",
      approved: "bg-grow/10 text-grow",
      rejected: "bg-danger/10 text-danger",
      held: "bg-chip text-ink",
    },
  },
});

export const proposalFilterVariants = tv({
  base: "cursor-pointer rounded-lg border px-3 py-2 text-xs transition-colors",
  variants: {
    active: {
      true: "border-grow bg-grow/10 text-grow",
      false: "border-line bg-panel text-muted hover:text-ink hover:bg-panel-2",
    },
  },
});

export const proposalFieldClass =
  "border-line bg-panel-2 text-ink w-full rounded-lg border px-3 py-2 text-sm outline-none focus:border-grow disabled:opacity-50";

export const proposalChangedVariants = tv({
  base: "text-prop mt-3 text-xs leading-relaxed",
});
