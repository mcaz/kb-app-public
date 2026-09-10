import { tv } from "tailwind-variants";

export const tagNotePreviewVariants = tv({
  slots: {
    root: "flex h-full min-h-0 flex-col",
    header: "border-line flex-none border-b px-4 py-2.5",
    alert:
      "border-line flex flex-none flex-wrap items-center justify-between gap-2 border-b px-4 py-3 text-sm",
    body: "min-h-0 flex-1",
    button: "focus-visible:ring-grow focus-visible:ring-2",
  },
});
