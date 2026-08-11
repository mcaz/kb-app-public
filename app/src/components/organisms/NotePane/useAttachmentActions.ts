import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { useErrorText } from "@/hooks/useErrorText";
import { useAttachmentAdd, useAttachmentRemove } from "@/lib/queries";

const MAX_BYTES = 50 * 1024 * 1024;

function toBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve((reader.result as string).split(",", 2)[1] ?? "");
    reader.onerror = () => reject(new Error("read failed"));
    reader.readAsDataURL(file);
  });
}

/** NotePane 私物: 添付の追加・削除と、その通知。 */
export function useAttachmentActions(noteId: string) {
  const { t } = useTranslation("notes");
  const errorText = useErrorText();
  const add = useAttachmentAdd();
  const remove = useAttachmentRemove();

  const addFiles = async (files: File[], rename?: string) => {
    for (const file of files) {
      if (file.size > MAX_BYTES) {
        toast(t("attachment.tooLarge"));
        continue;
      }
      try {
        const [, warning] = await add.mutateAsync({
          id: noteId,
          name: rename ?? file.name,
          dataBase64: await toBase64(file),
        });
        if (warning) toast(`⚠ ${warning}`);
      } catch (e) {
        toast(t("attachment.failed", { error: errorText(e) }));
        return false;
      }
    }
    return files.length > 0;
  };

  const removeFile = async (name: string) => {
    try {
      await remove.mutateAsync({ id: noteId, name });
      toast(t("attachment.removed"));
    } catch (e) {
      toast(t("attachment.failed", { error: errorText(e) }));
    }
  };

  return { addFiles, removeFile };
}
