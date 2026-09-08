import { tv } from "tailwind-variants";

export const distillationModelFieldsVariants = tv({
  slots: {
    description: "text-muted text-xs leading-relaxed",
    field: "flex min-w-0 flex-col gap-2",
    label: "text-[13px]",
    controls: "grid min-w-0 gap-4 sm:grid-cols-2",
    input:
      "border-line bg-panel text-ink focus:border-grow w-full min-w-0 rounded-md border px-3 py-2 text-[13px] outline-none disabled:opacity-50",
    select: "w-full min-w-0",
    actions: "flex flex-wrap items-center gap-3",
    warning: "text-danger text-xs leading-relaxed",
  },
});
