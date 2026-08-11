import { ChevronLeft, ChevronRight } from "lucide-react";

import { Button } from "@/components/atoms/ui/button";

export interface PagerProps {
  page: number;
  pageCount: number;
  from: number;
  to: number;
  total: number;
  onChange: (page: number) => void;
  labels: { previous: string; next: string };
  align?: "center" | "start";
}

/** ページ送り。1ページに収まるときは何も出さない。 */
export function Pager({
  page,
  pageCount,
  from,
  to,
  total,
  onChange,
  labels,
  align = "center",
}: PagerProps) {
  if (pageCount <= 1) return null;
  return (
    <div
      className={`text-muted flex items-center gap-2.5 px-2.5 py-1.5 text-[11.5px] ${
        align === "center" ? "justify-center" : "justify-start"
      }`}
    >
      <Button
        size="icon"
        aria-label={labels.previous}
        disabled={page === 0}
        onClick={() => onChange(page - 1)}
      >
        <ChevronLeft />
      </Button>
      <span>
        {from}–{to} / {total}
      </span>
      <Button
        size="icon"
        aria-label={labels.next}
        disabled={page >= pageCount - 1}
        onClick={() => onChange(page + 1)}
      >
        <ChevronRight />
      </Button>
    </div>
  );
}
