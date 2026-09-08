import { tv } from "tailwind-variants";

export const homeRefreshVariants = tv({
  slots: {
    row: "text-muted mb-3 flex flex-wrap items-center justify-between gap-2 text-xs",
    timestamp: "tabular-nums",
    error: "text-danger mb-3 text-sm",
    icon: "size-3.5",
  },
});
