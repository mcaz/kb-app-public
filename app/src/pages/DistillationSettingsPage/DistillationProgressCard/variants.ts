import { tv } from "tailwind-variants";

export const distillationProgressVariants = tv({
  slots: {
    section: "border-line bg-panel mt-4 flex min-w-0 flex-col gap-4 rounded-xl border p-4",
    heading: "text-[13px] font-semibold",
    description: "text-muted text-xs leading-relaxed",
    field: "flex min-w-0 flex-col gap-2",
    actions: "flex flex-wrap items-center gap-3",
    metrics: "grid grid-cols-2 gap-3 sm:grid-cols-5",
    metric: "border-line bg-panel-2 rounded-lg border px-3 py-2",
    count: "text-lg font-semibold tabular-nums",
    issueList: "flex min-w-0 flex-col gap-2",
    issue: "border-line flex min-w-0 flex-col gap-2 rounded-lg border p-3",
    issueHeading: "flex min-w-0 flex-wrap items-start justify-between gap-2",
    issueTitle: "min-w-0 text-[13px] font-semibold break-words",
    issueState: "bg-panel-2 text-muted shrink-0 rounded-md px-2 py-0.5 text-[11px]",
    issueNote: "text-muted text-[11px] break-all",
    issueReason: "text-ink text-xs leading-relaxed break-words",
  },
});
