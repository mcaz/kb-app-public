import { describe, expect, it } from "vitest";

import { navigationDirection } from "./useNavigationHistoryShortcut";

const key = (overrides: Partial<KeyboardEvent> = {}) => ({
  altKey: false,
  ctrlKey: false,
  key: "",
  metaKey: false,
  shiftKey: false,
  ...overrides,
});

describe("navigationDirection", () => {
  it("macOSではCmd+左右を戻る・進むとして扱う", () => {
    expect(navigationDirection(key({ metaKey: true, key: "ArrowLeft" }), true)).toBe("back");
    expect(navigationDirection(key({ metaKey: true, key: "ArrowRight" }), true)).toBe("forward");
  });

  it("Windows・LinuxではAlt+左右を戻る・進むとして扱う", () => {
    expect(navigationDirection(key({ altKey: true, key: "ArrowLeft" }), false)).toBe("back");
    expect(navigationDirection(key({ altKey: true, key: "ArrowRight" }), false)).toBe("forward");
  });

  it("OSと異なるキー、追加修飾キー、上下キーでは反応しない", () => {
    expect(navigationDirection(key({ altKey: true, key: "ArrowLeft" }), true)).toBeNull();
    expect(navigationDirection(key({ metaKey: true, key: "ArrowLeft" }), false)).toBeNull();
    expect(
      navigationDirection(key({ altKey: true, ctrlKey: true, key: "ArrowLeft" }), false),
    ).toBeNull();
    expect(navigationDirection(key({ altKey: true, key: "ArrowUp" }), false)).toBeNull();
  });
});
