import { tv } from "tailwind-variants";

export const observationTrendVariants = tv({
  slots: {
    section: "mt-7 min-w-0",
    header: "mb-3 flex flex-wrap items-center justify-between gap-2",
    heading: "text-muted text-xl font-medium tracking-[0.04em]",
    refreshIcon: "size-3.5",
    card: "border-line bg-panel min-w-0 rounded-xl border p-4",
    choiceGroup: "flex flex-wrap gap-2",
    clientsHelp: "text-muted mt-2 mb-4 text-[11px] leading-relaxed",
    status: "text-muted text-sm",
    period: "text-muted mb-3 text-xs",
    figure: "mt-4",
    caption: "mb-4 flex flex-wrap items-baseline gap-x-3 gap-y-1",
    totalLabel: "text-muted text-xs",
    totalValue: "text-ink text-2xl font-semibold tabular-nums",
    chart: "grid min-w-0 grid-cols-[auto_minmax(0,1fr)] gap-x-2",
    scale: "text-muted flex h-36 flex-col justify-between text-[10px] tabular-nums",
    bars: "border-line flex h-36 min-w-0 items-end gap-1 border-b",
    barCell: "flex h-full min-w-0 flex-1 items-end justify-center",
    axis: "text-muted mt-1.5 flex justify-between gap-2 text-[10px] tabular-nums",
    help: "text-muted mt-2 text-[11px] leading-relaxed",
    errorsHelp: "text-muted mt-1 text-[11px] leading-relaxed",
    details: "border-line mt-4 border-t pt-3",
    summary: "text-muted cursor-pointer text-xs",
    tableWrap: "mt-2 overflow-x-auto",
    table: "w-full text-left text-xs",
    tableHead: "text-muted",
    dateHeading: "py-1 pr-3 font-normal",
    metricHeading: "py-1 pl-3 text-right font-normal",
    tableBody: "text-ink tabular-nums",
    dateCell: "py-1 pr-3 font-normal whitespace-nowrap",
    metricCell: "py-1 pl-3 text-right",
    disclosure: "text-muted mt-3 text-[11px] leading-relaxed",
  },
  variants: {
    unavailable: { true: { status: "text-danger" } },
  },
});

export const observationTrendMetricVariants = tv({
  base: "min-w-0 cursor-pointer rounded-lg border px-3 py-1.5 text-xs transition-colors",
  variants: {
    selected: {
      true: "border-grow bg-grow-soft text-ink font-medium",
      false: "border-line text-muted hover:bg-sel hover:text-ink",
    },
  },
});

export const observationTrendBarVariants = tv({
  base: "w-full max-w-10 rounded-t-sm",
  variants: {
    metric: {
      hook_output_emitted: "bg-grow",
      propose_successes: "bg-grow",
      update_successes: "bg-grow",
      errors: "bg-danger",
    },
  },
});
