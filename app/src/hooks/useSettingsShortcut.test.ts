import { describe, expect, it } from "vitest";

import { matchesSettingsShortcut } from "./useSettingsShortcut";

const event = (patch: Partial<KeyboardEvent> = {}) =>
  ({
    altKey: false,
    ctrlKey: false,
    key: ",",
    metaKey: false,
    shiftKey: false,
    ...patch,
  }) as KeyboardEvent;

describe("matchesSettingsShortcut", () => {
  it("Cmd+, を設定ショートカットとして扱う", () => {
    expect(matchesSettingsShortcut(event({ metaKey: true }))).toBe(true);
  });

  it("Ctrl+, も設定ショートカットとして扱う", () => {
    expect(matchesSettingsShortcut(event({ ctrlKey: true }))).toBe(true);
  });

  it("追加修飾キーや別キーでは開かない", () => {
    expect(matchesSettingsShortcut(event({ metaKey: true, shiftKey: true }))).toBe(false);
    expect(matchesSettingsShortcut(event({ altKey: true, metaKey: true }))).toBe(false);
    expect(matchesSettingsShortcut(event({ key: "p", metaKey: true }))).toBe(false);
    expect(matchesSettingsShortcut(event())).toBe(false);
  });
});
