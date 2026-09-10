import { tv } from "tailwind-variants";

export const gettingStartedVariants = tv({
  slots: {
    section: "min-w-0 flex-1 overflow-y-auto px-5 py-5",
    title: "text-ink text-lg font-bold",
    lead: "text-muted mt-2 max-w-[46em] text-sm leading-relaxed",
    steps: "mt-5 grid max-w-[46em] list-none gap-4 p-0",
    step: "border-line bg-panel flex min-w-0 items-start gap-3 rounded-xl border p-4",
    number:
      "bg-chip text-muted flex size-6 shrink-0 items-center justify-center rounded-full text-xs font-semibold tabular-nums",
    content: "min-w-0 flex-1",
    heading: "text-ink text-sm font-semibold",
    description: "text-muted mt-2 text-xs leading-relaxed",
    actions: "mt-3 flex flex-wrap gap-2",
    prompt:
      "border-line bg-surface text-ink my-3 rounded-lg border p-3 text-xs leading-relaxed break-words whitespace-pre-wrap select-text",
    hint: "text-muted mt-3 max-w-[46em] text-xs leading-relaxed",
    icon: "size-3.5 shrink-0",
    footnote: "text-muted mt-5 max-w-[46em] text-xs leading-relaxed",
  },
});
