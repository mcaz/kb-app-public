import { beforeEach, describe, expect, it } from "vitest";

import { setupI18n } from "@/i18n";
import { useSession } from "@/lib/stores/session";

import { tabTitle } from "./tabTitle";

const t = setupI18n("ja").getFixedT("ja", "common");
const titles = () => useSession.getState().tabs.map((tab) => tabTitle(tab, t));

beforeEach(() => {
  useSession.setState(useSession.getInitialState(), true);
});

// 2026-09-08: 一覧へ移っても保持中のノートIDがタブ名に残った回帰を防ぐ。
describe("tabTitle", () => {
  it("カテゴリ選択でノートの選択を保持しながら一覧の名前へ変わる", () => {
    const session = useSession.getState();
    session.openNote("notes/alpha");
    expect(titles()).toEqual(["alpha"]);

    session.selectCategory("research");
    expect(titles()).toEqual(["research"]);
    expect(useSession.getState().selectedId).toBe("notes/alpha");
  });

  it("本文から一覧へ戻るとカテゴリ名になり、履歴で本文へ戻れる", () => {
    const session = useSession.getState();
    session.openNote("team/research/alpha");
    session.showCategoryList();
    expect(titles()).toEqual(["research"]);

    session.goBack();
    expect(titles()).toEqual(["alpha"]);
    session.goForward();
    expect(titles()).toEqual(["research"]);
  });

  it("戻る・進むでカテゴリ一覧とノート詳細の名前を復元する", () => {
    const session = useSession.getState();
    session.openNote("notes/alpha");
    session.selectCategory("research");
    session.openListedNote("research/beta");

    session.goBack();
    expect(titles()).toEqual(["research"]);
    session.goBack();
    expect(titles()).toEqual(["alpha"]);
    session.goForward();
    expect(titles()).toEqual(["research"]);
    session.goForward();
    expect(titles()).toEqual(["beta"]);
  });

  it("非選択タブも自身の表示内容を使い、切替後も履歴を保つ", () => {
    const session = useSession.getState();
    session.openNote("notes/alpha");
    session.selectCategory("research");
    session.openTab();
    session.openNote("notes/beta");
    expect(titles()).toEqual(["research", "beta"]);

    session.switchTab("tab-1");
    expect(titles()).toEqual(["research", "beta"]);
    session.goBack();
    expect(titles()).toEqual(["alpha", "beta"]);
    session.switchTab("tab-2");
    expect(useSession.getState().selectedId).toBe("notes/beta");
    expect(titles()).toEqual(["alpha", "beta"]);
  });

  it.each(["home", "files", "tags", "graph", "proposals"] as const)(
    "%sでは保持中のノート名ではなく画面名を使う",
    (view) => {
      useSession.getState().openNote("notes/alpha");
      useSession.getState().go(view);
      expect(titles()).toEqual([t(`nav.${view}`)]);
    },
  );

  it("選択がない場合とルート一覧ではノート画面名を使う", () => {
    expect(titles()).toEqual([t("nav.notes")]);
    useSession.getState().openNote("alpha");
    useSession.getState().showCategoryList();
    expect(titles()).toEqual([t("nav.notes")]);
  });

  it("カテゴリ未選択で本文を表示する場合はノート名を使う", () => {
    useSession.getState().openProposal("notes/proposal");
    useSession.getState().go("notes");
    expect(useSession.getState().selectedCategory).toBeNull();
    expect(useSession.getState().browsePane).toBe("list");
    expect(titles()).toEqual(["proposal"]);
  });
});
