import type { TagInfo } from "@/lib/api";

/** AIの役割説明も検索対象にし、未使用の登録語を絞り込みで落とさない。 */
export function filterTags(tags: readonly TagInfo[], query: string): TagInfo[] {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return [...tags];
  return tags.filter((item) =>
    `${item.tag}\n${item.description ?? ""}`.toLocaleLowerCase().includes(needle),
  );
}
