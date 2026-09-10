import { tv } from "tailwind-variants";

export const settingsDialogVariants = tv({
  slots: {
    connectionPane: "flex min-h-0 min-w-0 flex-1 flex-col",
    guideEntry:
      "border-line text-muted flex shrink-0 flex-wrap items-center justify-between gap-2 border-b px-5 py-3 text-xs",
  },
});
