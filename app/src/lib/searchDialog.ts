export type SearchPane = "results" | "preview";

/** 結果が更新されても現選択が残るなら維持し、無ければ先頭へ寄せる。 */
export function resolveSearchSelection(
  current: string | null,
  ids: readonly string[],
): string | null {
  if (current && ids.includes(current)) return current;
  return ids[0] ?? null;
}

/** 広い画面では常に結果とプレビューを同時表示するため、遷移状態を無視する。 */
export function effectiveSearchPane(compact: boolean, pane: SearchPane): SearchPane {
  return compact ? pane : "results";
}
