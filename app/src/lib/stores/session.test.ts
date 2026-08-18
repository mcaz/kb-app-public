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
