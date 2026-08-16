import { convertFileSrc } from "@tauri-apps/api/core";
import { useEffect, useMemo, useRef } from "react";

import { noteIdFromHref, renderMarkdown, vaultImagePath } from "./markdown";

export interface MarkdownViewProps {
  body: string;
  /** vault のルート。本文中の /画像パス を実ファイルへ解決するのに使う。 */
  vaultRoot: string;
  inTauri: boolean;
  onOpenNote: (id: string) => void;
}

function vaultAssetPath(vaultRoot: string, imagePath: string): string {
  const separator = vaultRoot.includes("\\") ? "\\" : "/";
  const root = vaultRoot.replace(/[\\/]+$/, "");
  return `${root}${separator}${imagePath.slice(1).replaceAll("/", separator)}`;
}

/** ノート本文を、ネットワークとVault外ファイルへ到達できないHTMLとして表示する。 */
export function MarkdownView({ body, vaultRoot, inTauri, onOpenNote }: MarkdownViewProps) {
  const ref = useRef<HTMLDivElement>(null);
  const html = useMemo(() => renderMarkdown(body), [body]);

  useEffect(() => {
    const root = ref.current;
    if (!root) return;

    for (const img of root.querySelectorAll("img")) {
      const path = vaultImagePath(img.dataset.kbImage ?? "");
      img.removeAttribute("data-kb-image");
      if (path && inTauri) img.src = convertFileSrc(vaultAssetPath(vaultRoot, path));
    }

    const onClick = (e: MouseEvent) => {
      const anchor = (e.target as HTMLElement).closest("a");
      if (!anchor) return;
      e.preventDefault();
      const id = noteIdFromHref(anchor.getAttribute("href") ?? "");
      if (id) onOpenNote(id);
    };

    root.addEventListener("click", onClick);
    return () => root.removeEventListener("click", onClick);
  }, [html, vaultRoot, inTauri, onOpenNote]);

  return (
    <div
      ref={ref}
      // 書式は styles/app.css の .markdown にまとめてある(Markdown が生む要素は
      // 種類が多く、ユーティリティを並べるより1箇所に置いたほうが読める)
      className="markdown"
      // DOMPurify済みのHTMLだけを渡す。AI生成・同期済みMarkdownは信頼境界の外。
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}
