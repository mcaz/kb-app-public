import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { useErrorText } from "@/hooks/useErrorText";
import { api, IN_TAURI } from "@/lib/api";
import { queryKeys } from "@/lib/queries";
import { useQueryClient } from "@tanstack/react-query";

/**
 * ノートを開いた状態での ⌘V / ドラッグ&ドロップ = 添付として持ち込む。
 * 旧実装はモジュール読み込み時に document へ購読を張っていたので、ここで
 * ライフサイクルに載せ替える(解除できるようにする)。
 */
export function useNoteFileIntake(noteId: string | null, active: boolean) {
  const { t } = useTranslation("notes");
  const errorText = useErrorText();
  const qc = useQueryClient();

  useEffect(() => {
    const refresh = (id: string) => qc.invalidateQueries({ queryKey: queryKeys.note(id) });

    const onPaste = (e: ClipboardEvent) => {
      const target = e.target as HTMLElement | null;
      if (target && (target.tagName === "TEXTAREA" || target.tagName === "INPUT")) return;
      if (!noteId || !active) return;

      const items = Array.from(e.clipboardData?.items ?? []);
      const image = items.find((i) => i.type.startsWith("image/"));

      void (async () => {
        try {
          if (image) {
            e.preventDefault();
            const file = image.getAsFile();
            if (!file) return;
            const ext = image.type.split("/")[1] ?? "png";
            const base64 = await new Promise<string>((resolve, reject) => {
              const reader = new FileReader();
              reader.onload = () => resolve((reader.result as string).split(",", 2)[1] ?? "");
              reader.onerror = () => reject(new Error("read failed"));
              reader.readAsDataURL(file);
            });
            const [saved, warning] = await api.attachmentAdd(
              noteId,
              `pasted-${Date.now()}.${ext}`,
              base64,
            );
            if (warning) toast(`⚠ ${warning}`);
            await refresh(noteId);
            toast(t("attachment.addedNamed", { name: saved }));
            return;
          }
          // 文字列のペーストは邪魔しない
          if (items.some((i) => i.kind === "string")) return;
          // WKWebView は DOM に画像を渡さないため Rust 側から読む
          const result = await api.attachmentPaste(noteId);
          if (!result) return;
          const [saved, warning] = result;
          if (warning) toast(`⚠ ${warning}`);
          await refresh(noteId);
          toast(t("attachment.addedNamed", { name: saved }));
        } catch (err) {
          toast(t("attachment.pasteFailed", { error: errorText(err) }));
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
          const [, warning] = await api.attachmentAddFromPath(id, path);
          if (warning) toast(`⚠ ${warning}`);
          added++;
        } catch (e) {
          toast(t("attachment.failed", { error: errorText(e) }));
        }
      }
      if (added === 0) return;
      await qc.invalidateQueries({ queryKey: queryKeys.note(id) });
      toast(t("attachment.addedCount", { count: added }));
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
          toast(t("attachment.needNote"));
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
