import { describe, expect, it } from "vitest";

import { formatDateTime } from "./format";

describe("formatDateTime", () => {
  it("日時を画面共通の固定形式で返す", () => {
    const local = new Date(2026, 7, 16, 9, 5, 4).toISOString();
    expect(formatDateTime(local, "ja")).toBe("2026/08/16 09:05:04");
    expect(formatDateTime(local, "en")).toBe("2026/08/16 09:05:04");
  });

  it("値が無い、または不正な場合は指定された代替表示を返す", () => {
    expect(formatDateTime(null, "ja", "不明")).toBe("不明");
    expect(formatDateTime("invalid", "ja", "不明")).toBe("不明");
  });
});
