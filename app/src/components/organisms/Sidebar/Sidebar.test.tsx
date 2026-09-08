import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { TooltipProvider } from "@/components/atoms/ui/tooltip";
import { setupI18n } from "@/i18n";
import { useSession } from "@/lib/stores/session";

import { Sidebar, type SidebarProps } from "./Sidebar";

const prefs = vi.hoisted(() => ({ sideCollapsed: false, set: vi.fn() }));
vi.mock("@/lib/stores/prefs", () => ({
  usePrefs: <T,>(selector: (state: typeof prefs) => T) => selector(prefs),
}));
vi.mock("@/hooks/useMediaQuery", () => ({ useMediaQuery: () => false }));

beforeEach(() => {
  setupI18n("ja");
  useSession.setState(useSession.getInitialState(), true);
  prefs.sideCollapsed = false;
});
afterEach(cleanup);

// 2026-09-06: 画面名だけの切替では提案の選択が残り、サイドバーから一覧へ戻れなかった。
describe("proposal sidebar navigation", () => {
  it.each([false, true])("折り畳み=%sでも詳細を解除し、履歴から復帰できる", (collapsed) => {
    prefs.sideCollapsed = collapsed;
    useSession.getState().openProposal("notes/proposal");
    render(
      <TooltipProvider>
        <Sidebar
          categories={[]}
          categoriesError={null}
          categoriesFetching={false}
          onRetryCategories={vi.fn()}
          onOpenSearch={vi.fn()}
          settingsOpen={false}
          onOpenSettings={vi.fn()}
        />
      </TooltipProvider>,
    );

    const proposals = screen.getByRole("button", { name: "提案" });
    fireEvent.click(proposals);
    expect(useSession.getState().view).toBe("proposals");
    expect(useSession.getState().selectedId).toBeNull();
    const historyLength = useSession.getState().backStack.length;
    fireEvent.click(proposals);
    expect(useSession.getState().backStack).toHaveLength(historyLength);

    act(() => useSession.getState().goBack());
    expect(useSession.getState().view).toBe("proposals");
    expect(useSession.getState().selectedId).toBe("notes/proposal");
    act(() => useSession.getState().goForward());
    expect(useSession.getState().selectedId).toBeNull();
  });
});

// 2026-09-07: DB取得失敗を空配列へ潰し、「ノートはまだありません」と誤表示していた。
describe("sidebar category loading", () => {
  const props: SidebarProps = {
    categories: undefined,
    categoriesError: null,
    categoriesFetching: true,
    onRetryCategories: vi.fn(),
    onOpenSearch: vi.fn(),
    settingsOpen: false,
    onOpenSettings: vi.fn(),
  };
  const sidebar = (patch: Partial<SidebarProps> = {}) => (
    <TooltipProvider>
      <Sidebar {...props} {...patch} />
    </TooltipProvider>
  );

  it("未取得と取得失敗を0件と区別し、再試行後の正常な0件だけ空表示にする", () => {
    const retry = vi.fn();
    const view = render(sidebar());
    expect(screen.getByRole("status")).toHaveTextContent("読み込み中…");
    expect(screen.queryByText("ノートはまだありません")).not.toBeInTheDocument();

    view.rerender(
      sidebar({
        categoriesError: "端末上のデータを読み書きできませんでした",
        categoriesFetching: false,
        onRetryCategories: retry,
      }),
    );
    expect(screen.getByRole("alert")).toHaveTextContent("ノート一覧を取得できませんでした");
    expect(screen.queryByText("ノートはまだありません")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "再試行" }));
    expect(retry).toHaveBeenCalledOnce();

    view.rerender(
      sidebar({
        categoriesError: "端末上のデータを読み書きできませんでした",
        categoriesFetching: true,
        onRetryCategories: retry,
      }),
    );
    expect(screen.getByRole("button", { name: "読み込み中…" })).toBeDisabled();
    expect(screen.queryByText("ノートはまだありません")).not.toBeInTheDocument();

    view.rerender(sidebar({ categories: [], categoriesFetching: false }));
    expect(screen.getByText("ノートはまだありません")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("再取得だけが失敗した場合は最後に取得できたカテゴリを保つ", () => {
    render(
      sidebar({
        categories: [{ path: "notes", name: "notes", count: 12 }],
        categoriesError: "端末上のデータを読み書きできませんでした",
        categoriesFetching: false,
      }),
    );
    expect(screen.getByRole("alert")).toBeInTheDocument();
    expect(screen.getByRole("treeitem")).toHaveTextContent("notes12");
    expect(screen.getByRole("button", { name: "再試行" })).toBeEnabled();
    expect(screen.queryByText("ノートはまだありません")).not.toBeInTheDocument();
  });
});
