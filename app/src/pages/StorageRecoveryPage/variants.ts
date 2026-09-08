import { tv } from "tailwind-variants";

export const storageRecoveryVariants = tv({
  slots: {
    root: "bg-ground text-ink min-h-screen p-8",
    content: "mx-auto flex max-w-3xl flex-col gap-6",
    title: "text-2xl font-semibold",
    heading: "text-lg font-semibold",
    description: "text-muted text-sm leading-relaxed",
    card: "border-line bg-paper flex flex-col gap-4 rounded-xl border p-6",
    stats: "grid grid-cols-2 gap-4 sm:grid-cols-3",
    statistic: "flex min-w-0 flex-col gap-1",
    value: "text-xl font-semibold tabular-nums",
    label: "text-muted text-xs",
    actions: "flex flex-wrap items-center gap-3",
    details: "border-line rounded-lg border p-3 text-sm",
    summary: "cursor-pointer font-medium",
    list: "text-muted mt-3 max-h-64 list-inside list-disc overflow-y-auto text-xs leading-relaxed break-all",
    acknowledgement: "flex cursor-pointer items-start gap-3 text-sm leading-relaxed",
    checkbox: "accent-grow mt-1 size-4 shrink-0",
    error: "text-danger text-sm leading-relaxed",
    resultId: "text-muted text-xs break-all",
  },
});
