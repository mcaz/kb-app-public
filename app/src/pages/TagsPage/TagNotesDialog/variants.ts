import { tv } from "tailwind-variants";

export const tagNotesDialogVariants = tv({
  slots: {
    content: "flex flex-col",
    header: "border-line min-w-0 flex-none border-b px-5 py-3 pr-14 text-left",
    title: "text-[15px] leading-relaxed break-words [overflow-wrap:anywhere]",
    description: "text-muted line-clamp-3 text-xs leading-relaxed break-words",
    close: "absolute top-3 right-3 focus-visible:ring-grow focus-visible:ring-2",
    grid: "grid min-h-0 flex-1 grid-cols-[minmax(280px,38%)_minmax(0,1fr)] max-[759px]:grid-cols-1",
    results: "border-line flex min-h-0 min-w-0 flex-col border-r max-[759px]:border-r-0",
    heading: "text-muted flex flex-none items-center gap-1.5 px-3 pt-3 pb-2 text-xs font-medium",
    icon: "size-3.5 shrink-0",
    loading: "ml-auto text-[11px]",
    list: "min-h-0 flex-1 overflow-y-auto px-2 pb-2",
    row: "hover:bg-sel data-[selected=true]:bg-sel focus-visible:ring-grow w-full min-w-0 rounded-md text-left text-sm outline-none focus-visible:ring-2 focus-visible:ring-inset",
    empty: "text-muted px-4 py-8 text-center text-sm",
    alert:
      "border-line flex flex-none flex-col items-start gap-2 border-t px-3 py-3 text-xs leading-relaxed",
    retry: "focus-visible:ring-grow focus-visible:ring-2",
    pagination:
      "border-line flex flex-none flex-wrap items-center justify-between gap-2 border-t px-3 py-2 text-xs",
    loaded: "text-muted",
    preview: "bg-panel min-h-0 min-w-0",
    footer:
      "border-line text-muted flex h-10 flex-none items-center gap-4 border-t px-3 text-[11px] max-[560px]:hidden",
  },
});
