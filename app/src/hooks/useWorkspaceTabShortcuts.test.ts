import { act, cleanup, fireEvent, render, renderHook, screen } from "@testing-library/react";
import { createElement } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useSession } from "@/lib/stores/session";
import { WorkspaceTabs } from "@/components/organisms/WorkspaceTabs";

import { useWorkspaceTabShortcuts, workspaceTabAction } from "./useWorkspaceTabShortcuts";

const bridge = vi.hoisted(() => ({
  native: true,
  receive: undefined as ((event: { payload: "new" | "close" }) => void) | undefined,
  hide: vi.fn<() => Promise<void>>(),
  configure: vi.fn<(...args: unknown[]) => Promise<void>>(),
  dispose: vi.fn(),
  listen: vi.fn(),
  translate: (key: string) => key,
  errorText: () => "shortcut error",
  error: vi.fn(),
}));

vi.mock("@/lib/api", () => ({
  get IN_TAURI() {
    return bridge.native;
  },
  api: { windowHide: bridge.hide, workspaceTabShortcutsConfigure: bridge.configure },
}));
vi.mock("@/lib/bindings", () => ({
  events: { workspaceTabShortcut: { listen: bridge.listen } },
}));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: bridge.translate, i18n: { language: "ja" } }),
}));
vi.mock("@/hooks/useErrorText", () => ({ useErrorText: () => bridge.errorText }));
vi.mock("@/hooks/useMediaQuery", () => ({ useMediaQuery: () => false }));
vi.mock("sonner", () => ({ toast: { error: bridge.error } }));

const originalPlatform = navigator.platform;
const originalScrollIntoView = Object.getOwnPropertyDescriptor(
  HTMLElement.prototype,
  "scrollIntoView",
);
const key = (overrides: Partial<KeyboardEvent> = {}) => ({
  key: "",
  altKey: false,
  ctrlKey: false,
  metaKey: false,
  shiftKey: false,
  repeat: false,
  isComposing: false,
  ...overrides,
});
const nativeAction = (payload: "new" | "close") => act(() => bridge.receive?.({ payload }));

beforeEach(() => {
  vi.clearAllMocks();
  bridge.native = true;
  bridge.receive = undefined;
  bridge.hide.mockResolvedValue(undefined);
  bridge.configure.mockResolvedValue(undefined);
  bridge.listen.mockImplementation((receive: typeof bridge.receive) => {
    bridge.receive = receive;
    return Promise.resolve(bridge.dispose);
  });
  useSession.setState(useSession.getInitialState(), true);
  Object.defineProperty(navigator, "platform", { configurable: true, value: "MacIntel" });
  Object.defineProperty(HTMLElement.prototype, "scrollIntoView", {
    configurable: true,
    value: vi.fn(),
  });
});

afterEach(() => {
  cleanup();
  document.body.replaceChildren();
  Object.defineProperty(navigator, "platform", { configurable: true, value: originalPlatform });
  if (originalScrollIntoView) {
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", originalScrollIntoView);
  } else Reflect.deleteProperty(HTMLElement.prototype, "scrollIntoView");
});

describe("workspaceTabAction", () => {
  it("macOSはCmd+N/W、他のOSはCtrl+N/Wを使う", () => {
    expect(workspaceTabAction(key({ key: "n", metaKey: true }), true, false)).toBe("new");
    expect(workspaceTabAction(key({ key: "w", metaKey: true }), true, false)).toBe("close");
    expect(workspaceTabAction(key({ key: "n", ctrlKey: true }), false, true)).toBe("new");
    expect(workspaceTabAction(key({ key: "w", ctrlKey: true }), false, true)).toBe("close");
    expect(workspaceTabAction(key({ key: "n", ctrlKey: true }), true, false)).toBeNull();
    expect(workspaceTabAction(key({ key: "n", metaKey: true }), false, false)).toBeNull();
  });

  it("通常のTab、追加修飾キー、IME入力と新規・閉じるの長押しを奪わない", () => {
    for (const event of [
      key({ key: "Tab" }),
      key({ key: "Tab", shiftKey: true }),
      key({ key: "Tab", ctrlKey: true, altKey: true }),
      key({ key: "Tab", ctrlKey: true, metaKey: true }),
      key({ key: "Tab", ctrlKey: true, isComposing: true }),
      key({ key: "n", metaKey: true, shiftKey: true }),
      key({ key: "n", metaKey: true, ctrlKey: true }),
      key({ key: "w", metaKey: true, repeat: true }),
    ]) {
      expect(workspaceTabAction(event, true, false)).toBeNull();
    }
  });
});

describe("workspace tab shortcuts", () => {
  // 2026-09-08: 標準CloseWindowとDOMが二重に動くと、2枚から1枚へ閉じた直後に窓まで隠れる。
  it("MacのメニューとDOMに同じCmd+N/Wが来ても一度だけ操作する", async () => {
    renderHook(() => useWorkspaceTabShortcuts());
    await act(async () => {});
    expect(bridge.configure).toHaveBeenCalledWith(
      true,
      "nav.tabMenu",
      "nav.newTab",
      "nav.closeCurrentTab",
    );
    nativeAction("new");
    fireEvent.keyDown(window, { key: "n", metaKey: true });
    expect(useSession.getState().tabs).toHaveLength(2);
    expect(useSession.getState().view).toBe("home");

    nativeAction("close");
    fireEvent.keyDown(window, { key: "w", metaKey: true });
    expect(useSession.getState().tabs).toHaveLength(1);
    expect(bridge.hide).not.toHaveBeenCalled();
  });

  it("最後の1枚では現在地と履歴を残してウィンドウ操作を呼ぶ", () => {
    useSession.getState().openNote("notes/example");
    useSession.getState().setQuery("keep query");
    const tab = useSession.getState().tabs[0];
    renderHook(() => useWorkspaceTabShortcuts());
    nativeAction("close");
    expect(bridge.hide).toHaveBeenCalledTimes(1);
    expect(useSession.getState().tabs).toEqual([tab]);
    expect(useSession.getState().query).toBe("keep query");
    expect(useSession.getState().selectedId).toBe("notes/example");
  });

  it("入力中にも前後へ循環し、各タブの検索条件と履歴を保つ", () => {
    useSession.getState().openNote("notes/one");
    useSession.getState().setQuery("first");
    useSession.getState().addTag("kb-app");
    useSession.getState().openTab();
    useSession.getState().go("files");
    useSession.getState().setQuery("second");
    const tabs = useSession.getState().tabs;
    const input = document.createElement("input");
    document.body.append(input);
    input.focus();
    renderHook(() => useWorkspaceTabShortcuts());

    expect(fireEvent.keyDown(input, { key: "Tab", ctrlKey: true })).toBe(false);
    expect(useSession.getState().activeTabId).toBe("tab-1");
    expect(useSession.getState().query).toBe("first");
    expect(useSession.getState().selectedTags).toEqual(["kb-app"]);
    fireEvent.keyDown(input, { key: "Tab", ctrlKey: true, shiftKey: true });
    expect(useSession.getState().activeTabId).toBe("tab-2");
    expect(useSession.getState().query).toBe("second");
    expect(useSession.getState().tabs).toEqual(tabs);
    expect(fireEvent.keyDown(input, { key: "Tab" })).toBe(true);
  });

  it("1枚のタブ移動は現在地を変えず、閉じる動作にもならない", () => {
    renderHook(() => useWorkspaceTabShortcuts());
    const before = useSession.getState();
    fireEvent.keyDown(window, { key: "Tab", ctrlKey: true });
    fireEvent.keyDown(window, { key: "Tab", ctrlKey: true, shiftKey: true });
    expect(useSession.getState()).toBe(before);
    expect(bridge.hide).not.toHaveBeenCalled();
  });

  // 2026-09-08: 選択だけを変えると後続のArrow操作が旧タブの位置から計算される。
  it("見出しのフォーカスは切替先へ追従し、入力欄のフォーカスは奪わない", () => {
    useSession.getState().openTab();
    useSession.getState().openTab();
    useSession.getState().switchTab("tab-1");
    render(createElement(WorkspaceTabs));
    renderHook(() => useWorkspaceTabShortcuts());
    const headings = screen.getAllByRole("tab");
    headings[0]?.focus();
    fireEvent.keyDown(document.activeElement ?? window, { key: "Tab", ctrlKey: true });
    expect(document.activeElement).toBe(headings[1]);
    fireEvent.keyDown(document.activeElement ?? window, { key: "ArrowRight" });
    expect(document.activeElement).toBe(headings[2]);
    expect(useSession.getState().activeTabId).toBe("tab-3");

    expect(
      fireEvent.keyDown(document.activeElement ?? window, {
        key: "ArrowLeft",
        metaKey: true,
      }),
    ).toBe(true);
    expect(useSession.getState().activeTabId).toBe("tab-3");
    const input = document.createElement("input");
    document.body.append(input);
    input.focus();
    fireEvent.keyDown(input, { key: "Tab", ctrlKey: true });
    expect(useSession.getState().activeTabId).toBe("tab-1");
    expect(document.activeElement).toBe(input);
  });

  it.each(["dialog", "alertdialog"])("%sを開いている間は背後のタブを操作しない", (role) => {
    useSession.getState().openTab();
    const dialog = document.createElement("div");
    dialog.setAttribute("role", role);
    document.body.append(dialog);
    renderHook(() => useWorkspaceTabShortcuts());
    const before = useSession.getState();
    nativeAction("new");
    nativeAction("close");
    fireEvent.keyDown(window, { key: "Tab", ctrlKey: true });
    expect(useSession.getState()).toBe(before);
    expect(bridge.hide).not.toHaveBeenCalled();
  });

  it("IME変換中はネイティブ操作も抑止し、確定後に再開する", () => {
    renderHook(() => useWorkspaceTabShortcuts());
    fireEvent.compositionStart(window);
    nativeAction("new");
    nativeAction("close");
    expect(useSession.getState().tabs).toHaveLength(1);
    expect(bridge.hide).not.toHaveBeenCalled();
    fireEvent.compositionEnd(window);
    nativeAction("new");
    expect(useSession.getState().tabs).toHaveLength(2);
  });

  it("無効化と購読解除の後は古いイベントで操作しない", async () => {
    const { rerender } = renderHook(({ enabled }) => useWorkspaceTabShortcuts(enabled), {
      initialProps: { enabled: true },
    });
    await act(async () => {});
    rerender({ enabled: false });
    nativeAction("new");
    fireEvent.keyDown(window, { key: "Tab", ctrlKey: true });
    expect(useSession.getState().tabs).toHaveLength(1);
    expect(bridge.dispose).toHaveBeenCalledTimes(1);
    expect(bridge.configure).toHaveBeenLastCalledWith(
      false,
      "nav.tabMenu",
      "nav.newTab",
      "nav.closeCurrentTab",
    );
  });

  it("解除後に購読が完了してもメニューを有効化しない", async () => {
    let subscribe: ((dispose: () => void) => void) | undefined;
    bridge.listen.mockImplementation(
      () => new Promise<() => void>((resolve) => (subscribe = resolve)),
    );
    const { unmount } = renderHook(() => useWorkspaceTabShortcuts());
    unmount();
    await act(() => {
      subscribe?.(bridge.dispose);
      return Promise.resolve();
    });
    expect(bridge.dispose).toHaveBeenCalledTimes(1);
    expect(bridge.configure.mock.calls.some(([enabled]) => enabled === true)).toBe(false);
  });

  it("他のOSではDOMのCtrl+N/Wを処理し、メニュー購読をしない", () => {
    Object.defineProperty(navigator, "platform", { configurable: true, value: "Linux x86_64" });
    renderHook(() => useWorkspaceTabShortcuts());
    fireEvent.keyDown(window, { key: "n", ctrlKey: true });
    expect(useSession.getState().tabs).toHaveLength(2);
    fireEvent.keyDown(window, { key: "w", ctrlKey: true });
    expect(useSession.getState().tabs).toHaveLength(1);
    expect(bridge.listen).not.toHaveBeenCalled();
    expect(bridge.hide).not.toHaveBeenCalled();
  });
});
