import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { useErrorText } from "@/hooks/useErrorText";
import { api, IN_TAURI } from "@/lib/api";
import { queryKeys } from "@/lib/queries";
import { useQueryClient } from "@tanstack/react-query";

/**
 * ノートを開いた状態での ⌘V / ドラッグ&ドロップ = そのノートのファイルとして持ち込む。
 *
 * **どちらもコアの取り込み口へ合流させる**(ADR-0003 決定6)。以前はここだけが
 * 実体の書き込みを直接呼んでいて、区分の判定を通らない経路になっていた。
 * 渡すのはパスだけで、中身はこの層に載せない(決定8。base64 IPC の廃止)。
 */
export function useNoteFileIntake(noteId: string | null, active: boolean) {
  const { t } = useTranslation("notes");
  const errorText = useErrorText();
  const qc = useQueryClient();

  useEffect(() => {
    const refresh = (id: string) => qc.invalidateQueries({ queryKey: queryKeys.noteFiles(id) });

    const onPaste = (e: ClipboardEvent) => {
      const target = e.target as HTMLElement | null;
      if (target && (target.tagName === "TEXTAREA" || target.tagName === "INPUT")) return;
      if (!noteId || !active) return;

      const items = Array.from(e.clipboardData?.items ?? []);
      // 文字列の貼り付けは邪魔しない
      if (!items.some((i) => i.type.startsWith("image/")) && items.some((i) => i.kind === "string"))
        return;

      void (async () => {
        try {
          // 画像は DOM 側で読まず、クリップボードから直接ファイルにして取り込む
          // (WKWebView は DOM の paste に画像を渡さないので、経路も1本で済む)
          const added = await api.fileAddFromClipboard(noteId);
          if (!added) return;
          e.preventDefault();
          if (added.forced_local_only) toast(t("file.forcedLocalOnly"));
          await refresh(noteId);
          toast(t("file.added"));
        } catch (err) {
          toast(t("file.pasteFailed", { error: errorText(err) }));
        }
      })();
    };

    document.addEventListener("paste", onPaste);
    return () => document.removeEventListener("paste", onPaste);
  }, [noteId, active, qc, t, errorText]);

  useEffect(() => {
    if (!IN_TAURI) return;
    let dispose: (() => void) | undefined;

    const drop = async (paths: string[], id: string) => {
      let added = 0;
      for (const path of paths) {
        try {
          const result = await api.fileAdd(id, path);
          if (result.forced_local_only) toast(t("file.forcedLocalOnly"));
          added++;
        } catch (e) {
          toast(t("file.addFailed", { error: errorText(e) }));
        }
      }
      if (added === 0) return;
      await qc.invalidateQueries({ queryKey: queryKeys.noteFiles(id) });
      toast(t("file.addedCount", { count: added }));
    };

    void getCurrentWebview()
      .onDragDropEvent((event) => {
        const kind = event.payload.type;
        if (kind === "over" || kind === "enter") {
          document.body.classList.add("dragover");
          return;
        }
        document.body.classList.remove("dragover");
        if (kind !== "drop") return;
        if (!noteId || !active) {
          toast(t("file.needNote"));
          return;
        }
        void drop((event.payload as { paths?: string[] }).paths ?? [], noteId);
      })
      .then((unlisten) => {
        dispose = unlisten;
      });

    return () => dispose?.();
  }, [noteId, active, qc, t, errorText]);
}
