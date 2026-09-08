import { tv } from "tailwind-variants";

export const distillationTimingVariants = tv({
  slots: {
    section: "border-line bg-panel mt-4 flex min-w-0 flex-col gap-3 rounded-xl border p-4",
    header: "flex min-w-0 flex-wrap items-start justify-between gap-2",
    heading: "text-[13px] font-semibold",
    help: "text-muted text-xs leading-relaxed",
    warning: "text-danger text-xs leading-relaxed",
    list: "flex min-w-0 flex-col gap-2",
    run: "border-line min-w-0 rounded-lg border p-3",
    summary: "cursor-pointer text-xs leading-relaxed",
    result: "text-ink font-semibold",
    elapsed: "text-ink ml-2 font-semibold tabular-nums",
    metadata: "text-muted mt-2 text-xs leading-relaxed break-all",
    content: "mt-3 flex min-w-0 flex-col gap-2",
    stages:
      "border-line grid min-w-0 grid-cols-[minmax(0,1fr)_auto] gap-x-3 gap-y-2 border-t pt-3 text-xs",
    stageLabel: "text-muted min-w-0 break-words",
    stageValue: "text-right tabular-nums",
    state: "text-muted ml-1 text-[11px]",
    code: "text-muted text-[11px] break-all",
  },
});
