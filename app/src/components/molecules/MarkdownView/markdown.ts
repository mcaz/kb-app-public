import DOMPurify from "dompurify";
import { marked } from "marked";

const IMAGE_EXTENSIONS = /\.(?:avif|bmp|gif|jpe?g|png|webp)$/i;

function escapeAttribute(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;");
}

/** URL parser と asset protocol の二重decode差を残さない。 */
function decodeFully(raw: string): string | null {
  let decoded = raw;
  try {
    for (let i = 0; i < 4; i += 1) {
      const next = decodeURIComponent(decoded);
      if (next === decoded) break;
      decoded = next;
    }
  } catch {
    return null;
  }
  return /%[0-9a-f]{2}/i.test(decoded) ? null : decoded;
}

/** Vault画像として許すのは、root相対で能動コンテンツではない画像だけ。 */
export function vaultImagePath(raw: string): string | null {
  const path = decodeFully(raw.trim());
  if (
    !path ||
    !path.startsWith("/") ||
    path.startsWith("//") ||
    path.includes("\\") ||
    path.includes("\0") ||
    path.includes("?") ||
    path.includes("#") ||
    !IMAGE_EXTENSIONS.test(path)
  ) {
    return null;
  }
  const parts = path.slice(1).split("/");
  return parts.some((part) => !part || part === "." || part === "..") ? null : path;
}

export function noteIdFromHref(raw: string): string | null {
  const href = decodeFully(raw.trim());
  if (
    !href ||
    href.startsWith("//") ||
    href.includes(":") ||
    href.includes("\\") ||
    href.includes("?") ||
    href.includes("#")
  ) {
    return null;
  }
  const id = href.replace(/^\//, "").replace(/\.md$/, "");
  const parts = id.split("/");
  return href.endsWith(".md") && !parts.some((part) => !part || part === "." || part === "..")
    ? id
    : null;
}

/**
 * raw HTML は許容タグも含めてDOMPurifyを通す。画像URLは挿入前に全て外し、
 * 検証済みのVault画像だけを描画後にasset protocolへ接続する。
 */
export function renderMarkdown(body: string): string {
  const renderer = new marked.Renderer();
  renderer.image = ({ href, text, title }) => {
    const titleAttribute = title ? ` title="${escapeAttribute(title)}"` : "";
    return `<img data-kb-image="${escapeAttribute(href)}" alt="${escapeAttribute(text)}"${titleAttribute}>`;
  };
  const rendered = marked.parse(body, { async: false, renderer });
  return DOMPurify.sanitize(rendered, {
    USE_PROFILES: { html: true },
    FORBID_TAGS: [
      "audio",
      "base",
      "embed",
      "form",
      "iframe",
      "input",
      "link",
      "math",
      "meta",
      "object",
      "select",
      "source",
      "style",
      "svg",
      "textarea",
      "track",
      "video",
    ],
    FORBID_ATTR: [
      "action",
      "autofocus",
      "background",
      "formaction",
      "ping",
      "poster",
      "src",
      "srcset",
      "style",
      "target",
    ],
  });
}
