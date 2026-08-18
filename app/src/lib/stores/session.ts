import { create } from "zustand";

import type { Favorite, Period, SortKey } from "@/lib/api";

export type View = "home" | "notes" | "files" | "graph";
export type BrowsePane = "list" | "note";

interface NavigationState {
  view: View;
  selectedId: string | null;
  selectedCategory: string | null;
  browsePane: BrowsePane;
  graphFocus: string | null;
}

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
  backStack: NavigationState[];
  forwardStack: NavigationState[];

  go: (view: View) => void;
  /** ナビの「ノート」= 検索条件と選択を解除した本文画面へ戻す。 */
  resetNotes: () => void;
  openNote: (id: string) => void;
  initializeCategory: (path: string) => void;
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
  goBack: () => void;
  goForward: () => void;
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
  backStack: [] as NavigationState[],
  forwardStack: [] as NavigationState[],
};

const HISTORY_LIMIT = 50;

const navigationState = (state: NavigationState): NavigationState => ({
  view: state.view,
  selectedId: state.selectedId,
  selectedCategory: state.selectedCategory,
  browsePane: state.browsePane,
  graphFocus: state.graphFocus,
});

const sameNavigation = (left: NavigationState, right: NavigationState) =>
  left.view === right.view &&
  left.selectedId === right.selectedId &&
  left.selectedCategory === right.selectedCategory &&
  left.browsePane === right.browsePane &&
  left.graphFocus === right.graphFocus;

const navigate = (state: SessionStore, patch: Partial<NavigationState>) => {
  const current = navigationState(state);
  const next = { ...current, ...patch };
  if (sameNavigation(current, next)) return {};
  return {
    ...patch,
    backStack: [...state.backStack, current].slice(-HISTORY_LIMIT),
    forwardStack: [],
  };
};

export const useSession = create<SessionStore>()((set) => ({
  ...INITIAL,

  go: (view) => set((state) => navigate(state, { view })),
  resetNotes: () =>
    set((state) => ({
      ...navigate(state, {
        view: "notes",
        selectedId: null,
        selectedCategory: null,
        browsePane: "list",
        graphFocus: null,
      }),
      query: "",
      selectedTags: [],
      period: "all",
      sort: "updated",
      activeFav: null,
    })),
  openNote: (id) =>
    set((state) =>
      navigate(state, {
        selectedId: id,
        selectedCategory: categoryOf(id),
        browsePane: "note",
        view: "notes",
      }),
    ),
  initializeCategory: (selectedCategory) =>
    set((state) => (state.selectedCategory === null ? { selectedCategory } : {})),
  selectCategory: (selectedCategory) =>
    set((state) => navigate(state, { selectedCategory, browsePane: "list", view: "notes" })),
  openListedNote: (id) =>
    set((state) => navigate(state, { selectedId: id, browsePane: "note", view: "notes" })),
  showCategoryList: () => set((state) => navigate(state, { browsePane: "list", view: "notes" })),
  setQuery: (query) => set({ query }),
  addTag: (tag) =>
    set((s) =>
      s.selectedTags.includes(tag) ? s : { selectedTags: [...s.selectedTags, tag], view: "notes" },
    ),
  removeTag: (tag) => set((s) => ({ selectedTags: s.selectedTags.filter((t) => t !== tag) })),
  clearTags: () => set({ selectedTags: [] }),
  setPeriod: (period) => set({ period }),
  setSort: (sort) => set({ sort }),
  focusGraph: (graphFocus) => set((state) => navigate(state, { graphFocus, view: "graph" })),
  applyFavorite: (fav) =>
    set((state) => ({
      ...navigate(state, { view: "notes" }),
      selectedTags: [...fav.tags],
      query: fav.query ?? "",
      period: (fav.period as Period | null) ?? "all",
      sort: (fav.sort as SortKey | null) ?? "updated",
      activeFav: fav.name,
    })),
  setActiveFav: (activeFav) => set({ activeFav }),
  goBack: () =>
    set((state) => {
      const previous = state.backStack.at(-1);
      if (!previous) return {};
      return {
        ...previous,
        backStack: state.backStack.slice(0, -1),
        forwardStack: [navigationState(state), ...state.forwardStack].slice(0, HISTORY_LIMIT),
      };
    }),
  goForward: () =>
    set((state) => {
      const next = state.forwardStack[0];
      if (!next) return {};
      return {
        ...next,
        backStack: [...state.backStack, navigationState(state)].slice(-HISTORY_LIMIT),
        forwardStack: state.forwardStack.slice(1),
      };
    }),
}));
