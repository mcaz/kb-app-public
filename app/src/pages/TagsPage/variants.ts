import { tv } from "tailwind-variants";

export const tagsPageVariants = tv({
  slots: {
    page: "mx-auto w-full min-w-0 max-w-[1180px] px-7 py-7 max-[720px]:px-4 max-[720px]:py-5",
    heading: "flex flex-wrap items-center gap-3",
    title: "text-[28px] leading-tight font-medium",
    badge: "border-line text-muted rounded-md border px-2 py-1 text-[11px]",
    intro: "text-muted mt-2 text-sm leading-relaxed",
    searchLabel: "relative mt-6 block min-w-0",
    searchIcon: "text-muted pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2",
    search:
      "border-line bg-panel-2 text-ink h-10.5 w-full rounded-lg border py-2.5 pr-3 pl-10 text-[13px] focus-visible:ring-grow focus-visible:ring-2 focus-visible:outline-none",
    hidden: "sr-only",
    count: "text-muted mt-4 mb-3 text-xs",
    table: "border-line bg-panel-2 min-w-0 overflow-hidden rounded-xl border",
    columns:
      "border-line text-muted grid grid-cols-[minmax(140px,22%)_minmax(0,1fr)_6rem_2rem] gap-5 border-b px-5 py-3 text-xs max-[720px]:hidden",
    list: "divide-line divide-y",
    row: "grid min-w-0 grid-cols-[minmax(140px,22%)_minmax(0,1fr)_6rem_2rem] items-start gap-5 px-5 py-4 max-[720px]:grid-cols-[minmax(0,1fr)_6rem_2rem] max-[720px]:gap-2 max-[720px]:px-4",
    tag: "min-w-0 max-[720px]:col-start-1 max-[720px]:row-start-1",
    tagName: "text-ink text-sm leading-relaxed font-semibold break-words [overflow-wrap:anywhere]",
    unregistered: "text-muted mt-1 text-[11px]",
    role: "text-ink min-w-0 text-sm leading-relaxed break-words whitespace-pre-line [overflow-wrap:anywhere] max-[720px]:col-span-3 max-[720px]:row-start-2",
    missingRole:
      "text-muted text-sm leading-relaxed max-[720px]:col-span-3 max-[720px]:row-start-2",
    countHeading: "text-right",
    // 読み上げ用dtのabsolute配置をセル内へ収め、一覧外のスクロール範囲へ逃がさない。
    noteCount:
      "text-ink relative flex h-8 min-w-0 items-center justify-end text-sm tabular-nums max-[720px]:col-start-2 max-[720px]:row-start-1",
    notesButton:
      "justify-self-end focus-visible:ring-grow focus-visible:ring-2 focus-visible:ring-offset-2 max-[720px]:col-start-3 max-[720px]:row-start-1",
    empty: "text-muted px-4 py-14 text-center text-sm",
    alert:
      "border-prop bg-prop-soft text-prop mt-4 flex flex-wrap items-center justify-between gap-3 rounded-lg border px-4 py-3 text-sm leading-relaxed",
    retry: "focus-visible:ring-grow focus-visible:ring-2",
  },
});
