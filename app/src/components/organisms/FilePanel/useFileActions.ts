import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { useErrorText } from "@/hooks/useErrorText";
import { api } from "@/lib/api";
import { useFileAdd, useFileDetach, useFileFetch } from "@/lib/queries";

import type { Added, FileRow } from "@/lib/api";

const MB = 1024 * 1024;

/** FilePanel 私物: ファイルの取り込み・付け替え・取り寄せと、その通知。 */
export function useFileActions(noteId: string) {
  const { t } = useTranslation("notes");
  const errorText = useErrorText();
  const add = useFileAdd();
  const detach = useFileDetach();
  const fetch = useFileFetch();

  /** 拒否ではない知らせ(大きさの警告・区分の固定)は取り込めた後に出す。 */
  const notice = (added: Added) => {
    if (added.forced_local_only) toast(t("file.forcedLocalOnly"));
    if (added.warn_over_bytes !== null) {
      toast(t("file.large", { limit: Math.round(added.warn_over_bytes / MB) }));
    }
  };

  const addPicked = async (supersedes?: string) => {
    const paths = await api.pickFiles(supersedes === undefined);
    for (const path of paths) {
      try {
        notice(await add.mutateAsync({ noteId, path, supersedes }));
      } catch (e) {
        toast(t("file.addFailed", { error: errorText(e) }));
        return;
      }
    }
    if (paths.length > 0) toast(supersedes ? t("file.replaced") : t("file.added"));
  };

  const detachFile = async (file: FileRow) => {
    try {
      await detach.mutateAsync({ noteId, id: file.id, expectedVersion: file.version });
      toast(t("file.detached"));
    } catch (e) {
      toast(errorText(e));
    }
  };

  const fetchFile = async (file: FileRow) => {
    try {
      const state = await fetch.mutateAsync({ noteId, id: file.id });
      toast(state === "local" ? t("file.fetched") : t("file.fetchIncomplete"));
    } catch (e) {
      toast(t("file.fetchFailed", { error: errorText(e) }));
    }
  };

  return {
    addPicked,
    detachFile,
    fetchFile,
    busy: add.isPending || detach.isPending || fetch.isPending,
  };
}
