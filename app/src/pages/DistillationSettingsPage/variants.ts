import { tv } from "tailwind-variants";

export const distillationSettingsVariants = tv({
  slots: {
    page: "w-full min-w-0 max-w-[46em] px-6 py-5",
    title: "mb-2 text-lg font-bold",
    description: "text-muted text-xs leading-relaxed",
    section: "border-line bg-panel mt-4 flex min-w-0 flex-col gap-4 rounded-xl border p-4",
    heading: "text-[13px] font-semibold",
    row: "flex min-w-0 items-center justify-between gap-4",
    field: "flex min-w-0 flex-col gap-2",
    label: "text-[13px]",
    controls: "grid min-w-0 gap-4 sm:grid-cols-2",
    input:
      "border-line bg-panel text-ink focus:border-grow w-full min-w-0 rounded-md border px-3 py-2 text-[13px] outline-none disabled:opacity-50",
    actions: "flex flex-wrap items-center gap-3",
    warning: "text-danger text-xs leading-relaxed",
  },
});
