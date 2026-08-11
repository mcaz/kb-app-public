import { create } from "zustand";

import type { Favorite, Period, SortKey } from "@/lib/api";

export type View = "home" | "notes" | "graph" | "connect" | "settings";

/**
 * 起動中だけの状態(保存しない)。
 * 開いているノートは **id だけ** を持ち、中身は TanStack Query 側のキャッシュから引く
 * — 旧実装が NoteView の実体を抱えて再取得のたびにズレていた点の作り直し。
 */
interface SessionStore {
  view: View;
  selectedId: string | null;
  secondaryId: string | null;
  query: string;
  selectedTags: string[];
  period: Period;
  sort: SortKey;
  activeFav: string | null;
  graphFocus: string | null;
  listPage: number;

  go: (view: View) => void;
  /** ナビの「ノート」= まっさらな一覧に戻す(絞り込み・検索・選択を解除)。 */
  resetNotes: () => void;
  openNote: (id: string) => void;
  openBeside: (id: string) => void;
  closeSecondary: () => void;
  promoteSecondary: () => void;
  setQuery: (query: string) => void;
  addTag: (tag: string) => void;
  removeTag: (tag: string) => void;
  clearTags: () => void;
  setPeriod: (period: Period) => void;
  setSort: (sort: SortKey) => void;
  setListPage: (page: number) => void;
  focusGraph: (id: string | null) => void;
  applyFavorite: (fav: Favorite) => void;
  setActiveFav: (name: string | null) => void;
}

const INITIAL = {
  view: "notes" as View,
  selectedId: null,
  secondaryId: null,
  query: "",
  selectedTags: [] as string[],
  period: "all" as Period,
  sort: "updated" as SortKey,
  activeFav: null,
  graphFocus: null,
  listPage: 0,
};

export const useSession = create<SessionStore>()((set, get) => ({
  ...INITIAL,

  go: (view) => set({ view }),
  resetNotes: () => set({ ...INITIAL }),
  openNote: (id) =>
    // 主ノートが変わったら並べ表示は畳む(前のノートの関連が残らないように)
    set({ selectedId: id, secondaryId: null, view: "notes" }),
  openBeside: (id) => set({ secondaryId: get().secondaryId === id ? null : id }),
  closeSecondary: () => set({ secondaryId: null }),
  promoteSecondary: () => {
    const { secondaryId } = get();
    if (secondaryId) set({ selectedId: secondaryId, secondaryId: null });
  },
  setQuery: (query) => set({ query, listPage: 0 }),
  addTag: (tag) =>
    set((s) =>
      s.selectedTags.includes(tag)
        ? s
        : { selectedTags: [...s.selectedTags, tag], listPage: 0, view: "notes" },
    ),
  removeTag: (tag) => set((s) => ({ selectedTags: s.selectedTags.filter((t) => t !== tag) })),
  clearTags: () => set({ selectedTags: [], listPage: 0 }),
  setPeriod: (period) => set({ period, listPage: 0 }),
  setSort: (sort) => set({ sort }),
  setListPage: (listPage) => set({ listPage }),
  focusGraph: (graphFocus) => set({ graphFocus, view: "graph" }),
  applyFavorite: (fav) =>
    set({
      selectedTags: [...fav.tags],
      query: fav.query ?? "",
      period: (fav.period as Period | null) ?? "all",
      sort: (fav.sort as SortKey | null) ?? "updated",
      listPage: 0,
      activeFav: fav.name,
      view: "notes",
    }),
  setActiveFav: (activeFav) => set({ activeFav }),
}));
