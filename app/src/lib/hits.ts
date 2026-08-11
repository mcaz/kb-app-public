import type { Hit, Period, SortKey } from "@/lib/api";

/** 一覧の絞り込み条件。 */
export interface HitFilter {
  tags: string[];
  period: Period;
}

/** タグは AND、期間は「更新がその日数以内」。 */
export function matchesFilter(hit: Hit, filter: HitFilter, now = Date.now()): boolean {
  if (!filter.tags.every((t) => hit.tags.includes(t))) return false;
  if (filter.period === "all") return true;
  const days = Number(filter.period);
  const updated = hit.updated ? new Date(hit.updated).getTime() : 0;
  if (!updated || Number.isNaN(updated)) return false;
  return now - updated <= days * 86_400_000;
}

export function sortHits(hits: readonly Hit[], sort: SortKey, locale: string): Hit[] {
  const out = [...hits];
  if (sort === "title") {
    return out.sort((a, b) => (a.title ?? a.id).localeCompare(b.title ?? b.id, locale));
  }
  const key = (h: Hit) => (sort === "created" ? h.created : h.updated) ?? "";
  return out.sort((a, b) => key(b).localeCompare(key(a)));
}

/** 有効な絞り込みの数(トグルの見出しに出す)。 */
export function activeFilterCount(filter: HitFilter): number {
  return filter.tags.length + (filter.period === "all" ? 0 : 1);
}

export interface Page<T> {
  items: T[];
  page: number;
  pageCount: number;
  total: number;
  from: number;
  to: number;
}

/** ページ番号が範囲外でも必ず有効なページを返す。 */
export function paginate<T>(items: readonly T[], size: number, page: number): Page<T> {
  const pageCount = Math.max(1, Math.ceil(items.length / size));
  const current = Math.min(Math.max(0, page), pageCount - 1);
  return {
    items: items.slice(current * size, (current + 1) * size),
    page: current,
    pageCount,
    total: items.length,
    from: items.length === 0 ? 0 : current * size + 1,
    to: Math.min(items.length, (current + 1) * size),
  };
}
