import { beforeEach, describe, expect, it } from "vitest";

import { useSession } from "./session";

beforeEach(() => {
  useSession.setState({
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
    tabs: [
      {
        id: "tab-1",
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
      },
    ],
    activeTabId: "tab-1",
    nextTabNumber: 2,
  });
});

describe("navigation history", () => {
  it("画面とノートの遷移を戻って進める", () => {
    useSession.getState().go("files");
    useSession.getState().openNote("notes/example");

    useSession.getState().goBack();
    expect(useSession.getState().view).toBe("files");

    useSession.getState().goBack();
    expect(useSession.getState().view).toBe("notes");
    expect(useSession.getState().selectedId).toBeNull();

    useSession.getState().goForward();
    expect(useSession.getState().view).toBe("files");
  });

  it("戻った後の新しい遷移で進む履歴を破棄する", () => {
    useSession.getState().go("files");
    useSession.getState().go("home");
    useSession.getState().goBack();

    useSession.getState().go("graph");
    useSession.getState().goForward();

    expect(useSession.getState().view).toBe("graph");
    expect(useSession.getState().forwardStack).toHaveLength(0);
  });

  it("最後まで戻った後のカテゴリ初期化では進む履歴を保持する", () => {
    useSession.getState().go("files");
    useSession.getState().go("home");
    useSession.getState().goBack();
    useSession.getState().goBack();

    useSession.getState().initializeCategory("notes");
    useSession.getState().goForward();

    expect(useSession.getState().view).toBe("files");
    expect(useSession.getState().forwardStack).toHaveLength(1);
  });
});

describe("workspace tabs", () => {
  it("新しいタブをホームで開き、元のタブの現在地を保つ", () => {
    useSession.getState().selectCategory("research");
    useSession.getState().openListedNote("research/search-design");

    useSession.getState().openTab();
    expect(useSession.getState().view).toBe("home");
    expect(useSession.getState().tabs).toHaveLength(2);

    useSession.getState().switchTab("tab-1");
    expect(useSession.getState().view).toBe("notes");
    expect(useSession.getState().selectedId).toBe("research/search-design");
    expect(useSession.getState().browsePane).toBe("note");
  });

  it("タブごとのページ移動と戻る履歴を独立して保つ", () => {
    useSession.getState().go("files");
    useSession.getState().openTab();
    useSession.getState().go("graph");
    useSession.getState().goBack();

    expect(useSession.getState().view).toBe("home");
    useSession.getState().switchTab("tab-1");
    expect(useSession.getState().view).toBe("files");
    expect(useSession.getState().backStack.at(-1)?.view).toBe("notes");
  });

  it("選択中タブを閉じると隣のタブへ移り、最後の1枚は閉じない", () => {
    useSession.getState().go("files");
    useSession.getState().openTab();
    useSession.getState().closeTab("tab-2");

    expect(useSession.getState().activeTabId).toBe("tab-1");
    expect(useSession.getState().view).toBe("files");
    expect(useSession.getState().tabs).toHaveLength(1);

    useSession.getState().closeTab("tab-1");
    expect(useSession.getState().tabs).toHaveLength(1);
  });
});

// 2026-09-08: ホームの件数リンクが以前のカテゴリ・詳細へ戻ったため、検索の初期化を画面遷移から分ける。
describe("all-note search context", () => {
  it.each(["list", "note"] as const)(
    "%sからホームへ来ても、履歴と別タブを保って条件だけ解除する",
    (pane) => {
      const session = () => useSession.getState();
      session().openNote("other/keep");
      const other = session().tabs[0];
      session().openTab();
      session().selectCategory("research");
      if (pane === "note") session().openListedNote("research/previous");
      session().go("home");
      session().applyFavorite({
        name: "old",
        query: "old query",
        tags: ["old"],
        period: "7",
        sort: "title",
      });
      const before = session();
      session().resetSearch();
      expect(session()).toMatchObject({
        view: "home",
        selectedId: before.selectedId,
        selectedCategory: "research",
        browsePane: pane,
        backStack: before.backStack,
        forwardStack: before.forwardStack,
        query: "",
        selectedTags: [],
        period: "all",
        sort: "updated",
        activeFav: null,
      });
      expect(session().tabs[0]).toEqual(other);
      expect(session().tabs[1]).toMatchObject({
        query: "",
        selectedTags: [],
        period: "all",
        sort: "updated",
        activeFav: null,
      });
      session().setSearchTags(["new", "new"]);
      expect(session().selectedTags).toEqual(["new"]);
      expect(session().view).toBe("home");
      expect(session().backStack).toEqual(before.backStack);
    },
  );
});
