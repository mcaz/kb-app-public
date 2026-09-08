import type { TFunction } from "i18next";

import type { WorkspaceTab } from "@/lib/stores/session";

export function tabTitle(tab: WorkspaceTab, t: TFunction<"common">): string {
  if (tab.view === "notes") {
    // 一覧でも選択IDは履歴用に残るため、NotesPageと同じ表示条件を使う。
    const showList = tab.selectedCategory !== null && tab.browsePane === "list";
    const path = showList ? tab.selectedCategory : (tab.selectedId ?? tab.selectedCategory);
    const name = path?.split("/").filter(Boolean).at(-1);
    return name || t("nav.notes");
  }
  return t(`nav.${tab.view}`);
}
