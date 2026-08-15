import { create } from "zustand";

import type { Favorite, Period, SortKey } from "@/lib/api";

export type View = "home" | "notes" | "graph";
export type BrowsePane = "list" | "note";

const categoryOf = (id: string) => {
  const segments = id.split("/").filter(Boolean);
  segments.pop();
  return segments.join("/");
};

/**
 * 起動中だけの状態(保存しない)。
 * 開いているノートは **id だけ** を持ち、中身は TanStack Query 側のキャッシュから引く
 * — 旧実装が NoteView の実体を抱えて再取得のたびにズレていた点の作り直し。
 */
interface SessionStore {
  view: View;
  selectedId: string | null;
  selectedCategory: string | null;
  browsePane: BrowsePane;
  query: string;
  selectedTags: string[];
  period: Period;
  sort: SortKey;
  activeFav: string | null;
  graphFocus: string | null;

  go: (view: View) => void;
  /** ナビの「ノート」= 検索条件と選択を解除した本文画面へ戻す。 */
  resetNotes: () => void;
  openNote: (id: string) => void;
  selectCategory: (path: string) => void;
  openListedNote: (id: string) => void;
  showCategoryList: () => void;
  setQuery: (query: string) => void;
  addTag: (tag: string) => void;
  removeTag: (tag: string) => void;
  clearTags: () => void;
  setPeriod: (period: Period) => void;
  setSort: (sort: SortKey) => void;
  focusGraph: (id: string | null) => void;
  applyFavorite: (fav: Favorite) => void;
  setActiveFav: (name: string | null) => void;
}

const INITIAL = {
  view: "notes" as View,
  selectedId: null,
  selectedCategory: null,
  browsePane: "list" as BrowsePane,
  query: "",
  selectedTags: [] as string[],
  period: "all" as Period,
  sort: "updated" as SortKey,
  activeFav: null,
  graphFocus: null,
};

export const useSession = create<SessionStore>()((set) => ({
  ...INITIAL,

  go: (view) => set({ view }),
  resetNotes: () => set({ ...INITIAL }),
  openNote: (id) =>
    set({ selectedId: id, selectedCategory: categoryOf(id), browsePane: "note", view: "notes" }),
  selectCategory: (selectedCategory) =>
    set({ selectedCategory, browsePane: "list", view: "notes" }),
  openListedNote: (id) => set({ selectedId: id, browsePane: "note", view: "notes" }),
  showCategoryList: () => set({ browsePane: "list", view: "notes" }),
  setQuery: (query) => set({ query }),
  addTag: (tag) =>
    set((s) =>
      s.selectedTags.includes(tag) ? s : { selectedTags: [...s.selectedTags, tag], view: "notes" },
    ),
  removeTag: (tag) => set((s) => ({ selectedTags: s.selectedTags.filter((t) => t !== tag) })),
  clearTags: () => set({ selectedTags: [] }),
  setPeriod: (period) => set({ period }),
  setSort: (sort) => set({ sort }),
  focusGraph: (graphFocus) => set({ graphFocus, view: "graph" }),
  applyFavorite: (fav) =>
    set({
      selectedTags: [...fav.tags],
      query: fav.query ?? "",
      period: (fav.period as Period | null) ?? "all",
      sort: (fav.sort as SortKey | null) ?? "updated",
      activeFav: fav.name,
      view: "notes",
    }),
  setActiveFav: (activeFav) => set({ activeFav }),
}));
