import { describe, expect, it } from "vitest";

import { fileKind, filterFiles } from "./fileKind";

import type { FileCard } from "@/lib/api";

const file = (overrides: Partial<FileCard> = {}): FileCard => ({
  id: "file-1",
  version: 1,
  name: "design.pdf",
  size: 120,
  media_type: "application/pdf",
  availability: "local",
  sensitivity: "private",
  sync: "full",
  linked: false,
  client_repo: false,
  can_fetch: false,
  added_at: "2026-08-18T00:00:00Z",
  notes: [{ id: "notes/design", title: "画面設計" }],
  ...overrides,
});

describe("ファイル一覧の絞り込み", () => {
  it("media typeと拡張子を画面用の4分類へまとめる", () => {
    expect(fileKind(file())).toBe("pdf");
    expect(fileKind(file({ name: "photo.png", media_type: "image/png" }))).toBe("image");
    expect(fileKind(file({ name: "memo.md", media_type: "application/octet-stream" }))).toBe(
      "document",
    );
  });

  it("ファイル名とノート名の両方を検索できる", () => {
    const files = [file(), file({ id: "file-2", name: "photo.png", media_type: "image/png" })];
    expect(filterFiles(files, "画面設計", "all")).toHaveLength(2);
    expect(filterFiles(files, "photo", "all").map((item) => item.id)).toEqual(["file-2"]);
    expect(filterFiles(files, "", "image").map((item) => item.id)).toEqual(["file-2"]);
  });
});
