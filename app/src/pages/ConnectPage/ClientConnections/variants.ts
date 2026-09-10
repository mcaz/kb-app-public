import { tv } from "tailwind-variants";

export const clientConnectionsVariants = tv({
  slots: {
    section: "border-line mx-5 mt-4 border-b pb-5",
    heading: "flex flex-wrap items-start justify-between gap-3",
    title: "text-sm font-semibold",
    description: "text-muted mt-1 text-xs leading-relaxed",
    error: "text-danger mt-3 text-xs leading-relaxed",
    grid: "mt-4 grid grid-cols-[repeat(auto-fit,minmax(min(250px,100%),1fr))] gap-3",
    card: "border-line bg-panel min-w-0 rounded-xl border p-4",
    cardHeading: "mb-3 flex flex-wrap items-center justify-between gap-2",
    facts: "grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-2 text-xs",
    label: "text-muted",
    value: "min-w-0 break-words",
    issues: "text-muted mt-3 list-disc space-y-1 pl-4 text-xs leading-relaxed",
    actions: "mt-4 flex flex-wrap gap-2",
    notice: "border-line bg-chip mt-3 rounded-md border px-3 py-2 text-xs leading-relaxed",
    details: "text-muted mt-3 text-xs",
    detailList: "mt-2 space-y-2 break-all",
  },
});
