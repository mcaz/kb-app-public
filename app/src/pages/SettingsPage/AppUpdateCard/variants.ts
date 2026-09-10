import { tv } from "tailwind-variants";

export const appUpdateCardVariants = tv({
  slots: {
    section: "border-line bg-panel mt-5 flex min-w-0 flex-col gap-3 rounded-xl border p-4",
    heading: "text-[13px] font-semibold",
    description: "text-muted text-xs leading-relaxed",
    status: "text-ink text-[13px] leading-relaxed",
    failure: "border-line bg-panel-2 rounded-lg border p-3 text-xs leading-relaxed",
    progressArea: "flex flex-col gap-1.5",
    progress:
      "accent-grow h-2 w-full overflow-hidden rounded-full [&::-webkit-progress-bar]:bg-panel-2 [&::-webkit-progress-value]:bg-grow [&::-moz-progress-bar]:bg-grow",
    actions: "flex flex-wrap items-center gap-2",
    action: "h-auto min-h-8 whitespace-normal",
  },
});
