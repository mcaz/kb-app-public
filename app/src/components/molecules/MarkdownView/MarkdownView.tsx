import { convertFileSrc } from "@tauri-apps/api/core";
import { marked } from "marked";
import { useEffect, useMemo, useRef } from "react";

export interface MarkdownViewProps {
  body: string;
  /** vault のルート。本文中の /画像パス を実ファイルへ解決するのに使う。 */
  vaultRoot: string;
  inTauri: boolean;
  onOpenNote: (id: string) => void;
}

/**
 * ノート本文の表示。
 * 描画後に2つだけ手を入れる: vault 内画像を asset プロトコルへ、
 * `.md` へのリンクをアプリ内遷移へ。
 */
export function MarkdownView({ body, vaultRoot, inTauri, onOpenNote }: MarkdownViewProps) {
  const ref = useRef<HTMLDivElement>(null);
  const html = useMemo(() => marked.parse(body, { async: false }), [body]);

  useEffect(() => {
    const root = ref.current;
    if (!root) return;

    for (const img of root.querySelectorAll("img")) {
      const src = decodeURIComponent(img.getAttribute("src") ?? "");
      if (src.startsWith("/") && inTauri) img.src = convertFileSrc(`${vaultRoot}${src}`);
    }

    const onClick = (e: MouseEvent) => {
      const anchor = (e.target as HTMLElement).closest("a");
      if (!anchor) return;
      e.preventDefault();
      const href = decodeURIComponent(anchor.getAttribute("href") ?? "");
      if (href.endsWith(".md")) onOpenNote(href.replace(/^\//, "").replace(/\.md$/, ""));
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
      // 本文は自分の vault 内の Markdown(AI が書いたもの)。外部入力ではない
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}
