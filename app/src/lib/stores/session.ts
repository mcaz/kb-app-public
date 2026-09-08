import { create } from "zustand";

import type { Favorite, Period, SortKey } from "@/lib/api";

export type View = "home" | "notes" | "files" | "graph" | "proposals";
export type BrowsePane = "list" | "note";

interface NavigationState {
  view: View;
  selectedId: string | null;
  selectedCategory: string | null;
  browsePane: BrowsePane;
  graphFocus: string | null;
}

interface WorkspaceContext extends NavigationState {
  query: string;
  selectedTags: string[];
  period: Period;
  sort: SortKey;
  activeFav: string | null;
  backStack: NavigationState[];
  forwardStack: NavigationState[];
}

export interface WorkspaceTab extends WorkspaceContext {
  id: string;
}

const categoryOf = (id: string) => {
  const segments = id.split("/").filter(Boolean);
  segments.pop();
  return segments.join("/");
};

interface SessionStore extends WorkspaceContext {
  tabs: WorkspaceTab[];
  activeTabId: string;
  nextTabNumber: number;

  openTab: () => void;
  closeTab: (id: string) => void;
  switchTab: (id: string) => void;
  go: (view: View) => void;
  /** ナビの「ノート」= 検索条件と選択を解除した本文画面へ戻す。 */
  resetNotes: () => void;
  openNote: (id: string) => void;
  openProposal: (id: string | null) => void;
  initializeCategory: (path: string) => void;
  selectCategory: (path: string) => void;
  openListedNote: (id: string) => void;
  showCategoryList: () => void;
  resetSearch: () => void;
  setSearchTags: (tags: string[]) => void;
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

const INITIAL_CONTEXT: WorkspaceContext = {
  view: "notes",
  selectedId: null,
  selectedCategory: null,
  browsePane: "list",
  query: "",
  selectedTags: [],
  period: "all",
  sort: "updated",
  activeFav: null,
  graphFocus: null,
  backStack: [],
  forwardStack: [],
};

const createTab = (id: string, context: WorkspaceContext = INITIAL_CONTEXT): WorkspaceTab => ({
  id,
  ...context,
  selectedTags: [...context.selectedTags],
  backStack: [...context.backStack],
  forwardStack: [...context.forwardStack],
});

const INITIAL_TAB = createTab("tab-1");
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

const navigate = (state: WorkspaceContext, patch: Partial<NavigationState>) => {
  const current = navigationState(state);
  const next = { ...current, ...patch };
  if (sameNavigation(current, next)) return {};
  return {
    ...patch,
    backStack: [...state.backStack, current].slice(-HISTORY_LIMIT),
    forwardStack: [],
  };
};

const workspaceContext = (
  state: WorkspaceContext,
  patch: Partial<WorkspaceContext> = {},
): WorkspaceContext => ({
  view: patch.view ?? state.view,
  selectedId: patch.selectedId !== undefined ? patch.selectedId : state.selectedId,
  selectedCategory:
    patch.selectedCategory !== undefined ? patch.selectedCategory : state.selectedCategory,
  browsePane: patch.browsePane ?? state.browsePane,
  query: patch.query ?? state.query,
  selectedTags: patch.selectedTags ?? state.selectedTags,
  period: patch.period ?? state.period,
  sort: patch.sort ?? state.sort,
  activeFav: patch.activeFav !== undefined ? patch.activeFav : state.activeFav,
  graphFocus: patch.graphFocus !== undefined ? patch.graphFocus : state.graphFocus,
  backStack: patch.backStack ?? state.backStack,
  forwardStack: patch.forwardStack ?? state.forwardStack,
});

/** 現在タブと公開中の互換フィールドを同じ更新で揃え、片方だけが古くなる状態を作らない。 */
const updateActiveTab = (state: SessionStore, patch: Partial<WorkspaceContext>) => {
  const next = workspaceContext(state, patch);
  return {
    ...patch,
    tabs: state.tabs.map((tab) => (tab.id === state.activeTabId ? createTab(tab.id, next) : tab)),
  };
};

const activateTab = (tab: WorkspaceTab) => ({
  ...workspaceContext(tab),
  activeTabId: tab.id,
});

/**
 * 起動中だけの状態(保存しない)。
 * タブごとに現在地と検索条件と履歴を保存し、公開フィールドには選択中タブの文脈だけを写す。
 */
export const useSession = create<SessionStore>()((set) => ({
  ...INITIAL_CONTEXT,
  tabs: [INITIAL_TAB],
  activeTabId: INITIAL_TAB.id,
  nextTabNumber: 2,

  openTab: () =>
    set((state) => {
      const tab = createTab(`tab-${state.nextTabNumber}`, {
        ...INITIAL_CONTEXT,
        view: "home",
      });
      return {
        ...activateTab(tab),
        tabs: [...state.tabs, tab],
        nextTabNumber: state.nextTabNumber + 1,
      };
    }),
  closeTab: (id) =>
    set((state) => {
      if (state.tabs.length === 1) return {};
      const index = state.tabs.findIndex((tab) => tab.id === id);
      if (index < 0) return {};
      const tabs = state.tabs.filter((tab) => tab.id !== id);
      if (id !== state.activeTabId) return { tabs };
      const next = tabs[Math.min(index, tabs.length - 1)];
      return next ? { ...activateTab(next), tabs } : {};
    }),
  switchTab: (id) =>
    set((state) => {
      const tab = state.tabs.find((candidate) => candidate.id === id);
      return tab && tab.id !== state.activeTabId ? activateTab(tab) : {};
    }),
  go: (view) => set((state) => updateActiveTab(state, navigate(state, { view }))),
  openProposal: (id) =>
    set((state) => updateActiveTab(state, navigate(state, { view: "proposals", selectedId: id }))),
  resetNotes: () =>
    set((state) =>
      updateActiveTab(state, {
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
      }),
    ),
  openNote: (id) =>
    set((state) =>
      updateActiveTab(
        state,
        navigate(state, {
          selectedId: id,
          selectedCategory: categoryOf(id),
          browsePane: "note",
          view: "notes",
        }),
      ),
    ),
  initializeCategory: (selectedCategory) =>
    set((state) =>
      state.selectedCategory === null ? updateActiveTab(state, { selectedCategory }) : {},
    ),
  selectCategory: (selectedCategory) =>
    set((state) =>
      updateActiveTab(
        state,
        navigate(state, { selectedCategory, browsePane: "list", view: "notes" }),
      ),
    ),
  openListedNote: (id) =>
    set((state) =>
      updateActiveTab(
        state,
        navigate(state, { selectedId: id, browsePane: "note", view: "notes" }),
      ),
    ),
  showCategoryList: () =>
    set((state) => updateActiveTab(state, navigate(state, { browsePane: "list", view: "notes" }))),
  resetSearch: () =>
    set((state) =>
      updateActiveTab(state, {
        query: "",
        selectedTags: [],
        period: "all",
        sort: "updated",
        activeFav: null,
      }),
    ),
  setSearchTags: (tags) =>
    set((state) => updateActiveTab(state, { selectedTags: [...new Set(tags)] })),
  setQuery: (query) => set((state) => updateActiveTab(state, { query })),
  addTag: (tag) =>
    set((state) =>
      state.selectedTags.includes(tag)
        ? state.view === "notes"
          ? {}
          : updateActiveTab(state, navigate(state, { view: "notes" }))
        : updateActiveTab(state, {
            ...navigate(state, { view: "notes" }),
            selectedTags: [...state.selectedTags, tag],
          }),
    ),
  removeTag: (tag) =>
    set((state) =>
      updateActiveTab(state, {
        selectedTags: state.selectedTags.filter((value) => value !== tag),
      }),
    ),
  clearTags: () => set((state) => updateActiveTab(state, { selectedTags: [] })),
  setPeriod: (period) => set((state) => updateActiveTab(state, { period })),
  setSort: (sort) => set((state) => updateActiveTab(state, { sort })),
  focusGraph: (graphFocus) =>
    set((state) => updateActiveTab(state, navigate(state, { graphFocus, view: "graph" }))),
  applyFavorite: (fav) =>
    set((state) =>
      updateActiveTab(state, {
        selectedTags: [...fav.tags],
        query: fav.query ?? "",
        period: (fav.period as Period | null) ?? "all",
        sort: (fav.sort as SortKey | null) ?? "updated",
        activeFav: fav.name,
      }),
    ),
  setActiveFav: (activeFav) => set((state) => updateActiveTab(state, { activeFav })),
  goBack: () =>
    set((state) => {
      const previous = state.backStack.at(-1);
      if (!previous) return {};
      return updateActiveTab(state, {
        ...previous,
        backStack: state.backStack.slice(0, -1),
        forwardStack: [navigationState(state), ...state.forwardStack].slice(0, HISTORY_LIMIT),
      });
    }),
  goForward: () =>
    set((state) => {
      const next = state.forwardStack[0];
      if (!next) return {};
      return updateActiveTab(state, {
        ...next,
        backStack: [...state.backStack, navigationState(state)].slice(-HISTORY_LIMIT),
        forwardStack: state.forwardStack.slice(1),
      });
    }),
}));
