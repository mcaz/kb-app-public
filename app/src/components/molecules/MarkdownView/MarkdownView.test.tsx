import { fireEvent, render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { MarkdownView } from "./MarkdownView";
import { noteIdFromHref, renderMarkdown, vaultImagePath } from "./markdown";

vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: (path: string) => `asset:${path}`,
}));

describe("renderMarkdown", () => {
  it("raw HTMLのscript・event handler・能動URLを挿入前に除く", () => {
    const html = renderMarkdown(
      '<script>globalThis.pwned = true</script><img src="https://evil.example/x" srcset="https://evil.example/y" onerror="fetch(\'https://evil.example/\')"><style>body{background:url(https://evil.example/z)}</style>',
    );

    expect(html).not.toContain("script");
    expect(html).not.toContain("onerror");
    expect(html).not.toContain("src=");
    expect(html).not.toContain("srcset");
    expect(html).not.toContain("style");
    expect(html).not.toContain("evil.example");
  });

  it("Markdown画像は読み込まず、検証待ち属性だけを残す", () => {
    const local = renderMarkdown("![図](/notes/example.files/diagram.png)");
    const external = renderMarkdown("![追跡](https://evil.example/pixel.png)");

    expect(local).toContain('data-kb-image="/notes/example.files/diagram.png"');
    expect(local).not.toContain("src=");
    expect(external).toContain('data-kb-image="https://evil.example/pixel.png"');
    expect(external).not.toContain("src=");
  });
});

describe("vaultImagePath", () => {
  it.each([
    "https://evil.example/x.png",
    "//evil.example/x.png",
    "/../secret.png",
    "/%2e%2e/secret.png",
    "/%252e%252e/secret.png",
    "/safe\\..\\secret.png",
    "/safe/vector.svg",
    "/safe/photo.png?token=x",
  ])("Vault外・能動コンテンツ候補を拒否する: %s", (path) => {
    expect(vaultImagePath(path)).toBeNull();
  });

  it("Vault root相対の受動画像だけを許す", () => {
    expect(vaultImagePath("/notes/example.files/diagram.PNG")).toBe(
      "/notes/example.files/diagram.PNG",
    );
  });
});

describe("MarkdownView", () => {
  it("検証済みVault画像だけにasset URLを与える", () => {
    const { container } = render(
      <MarkdownView
        body={[
          "![local](/notes/example.files/diagram.png)",
          "![external](https://evil.example/pixel.png)",
          "![traversal](/%2e%2e/secret.png)",
        ].join("\n")}
        vaultRoot="/vault"
        inTauri
        onOpenNote={vi.fn()}
      />,
    );
    const images = container.querySelectorAll("img");

    expect(images[0]).toHaveAttribute("src", "asset:/vault/notes/example.files/diagram.png");
    expect(images[1]).not.toHaveAttribute("src");
    expect(images[2]).not.toHaveAttribute("src");
    for (const image of images) expect(image).not.toHaveAttribute("data-kb-image");
  });

  it("外部リンクを遷移させず、Vault内ノートだけをアプリ内で開く", () => {
    const onOpenNote = vi.fn();
    const { getByText } = render(
      <MarkdownView
        body="[inside](/notes/decision.md) [outside](https://example.com/)"
        vaultRoot="/vault"
        inTauri={false}
        onOpenNote={onOpenNote}
      />,
    );

    fireEvent.click(getByText("outside"));
    expect(onOpenNote).not.toHaveBeenCalled();
    fireEvent.click(getByText("inside"));
    expect(onOpenNote).toHaveBeenCalledWith("notes/decision");
  });
});

describe("noteIdFromHref", () => {
  it("path traversalと外部URLをノートIDにしない", () => {
    expect(noteIdFromHref("/notes/a.md")).toBe("notes/a");
    expect(noteIdFromHref("/../outside.md")).toBeNull();
    expect(noteIdFromHref("https://example.com/a.md")).toBeNull();
  });
});
