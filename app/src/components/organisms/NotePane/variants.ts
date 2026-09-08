import { tv } from "tailwind-variants";

export const notePaneVariants = tv({
  slots: {
    root: "bg-panel flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden",
    header: "border-line z-10 flex max-h-[50%] flex-none flex-col border-b shadow-sm",
    toolbar:
      "flex min-w-0 flex-none items-center gap-2 border-b border-line px-4 py-2.5 max-[640px]:flex-wrap",
    breadcrumb: "flex min-w-0 flex-1 flex-wrap items-center gap-x-2 gap-y-1 text-sm",
    categoryItem: "flex min-w-0 max-w-full items-center gap-2",
    category:
      "text-muted min-w-0 max-w-48 cursor-pointer truncate rounded-sm text-left underline-offset-4 hover:text-ink hover:underline focus-visible:outline-2 focus-visible:outline-offset-4 focus-visible:outline-grow",
    title: "min-w-0 truncate",
    actions: "flex flex-none items-center gap-1 max-[640px]:ml-auto",
    metadataScroll: "min-h-0 overflow-y-auto px-4 py-3",
    metadata: "border-line bg-panel-2/50 group min-w-0 rounded-xl border px-4 py-3",
    summary:
      "text-muted flex cursor-pointer list-none items-center gap-2 text-xs [&::-webkit-details-marker]:hidden",
    fields:
      "mt-3 grid min-w-0 grid-cols-[104px_minmax(0,1fr)] gap-x-4 gap-y-2 text-sm max-[640px]:grid-cols-[72px_minmax(0,1fr)]",
    label: "text-muted leading-6",
    value: "min-w-0 leading-6 [overflow-wrap:anywhere]",
    tags: "flex min-w-0 flex-wrap items-center gap-1.5 [&_button]:rounded-md",
    scroll: "min-h-0 min-w-0 flex-1 overflow-y-auto overscroll-contain px-5 py-6 max-[640px]:px-4",
    body: "note-reading-body min-w-0",
    source:
      "text-ink m-0 min-w-0 whitespace-pre-wrap break-words font-mono text-[13px] leading-7 [overflow-wrap:anywhere]",
  },
});
