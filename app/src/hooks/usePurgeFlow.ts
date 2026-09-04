import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { useErrorText } from "@/hooks/useErrorText";
import { useFilePurgeCommit, useFilePurgePlan } from "@/lib/queries";

import type { PurgePlan } from "@/lib/api";

/**
 * ファイルを取り除く流れ(下見 → 確認 → 実行)。
 *
 * 下見は読み取りだけで、token に対象・版・実体を固定する。確認を挟むのは
 * **画面が本人へ訊いた**という事実そのもので、原本の無い実体(会話から受け取った
 * ファイル)はこれが無いとコアが拒否する(ADR kb-app/artifact-deletion)。
 *
 * @param noteId ノート内から呼ぶときだけ。その行の一覧も更新する
 */
export function usePurgeFlow(noteId?: string) {
  const { t } = useTranslation("notes");
  const errorText = useErrorText();
  const plan = useFilePurgePlan();
  const commit = useFilePurgeCommit();
  const [target, setTarget] = useState<PurgePlan | null>(null);

  const planPurge = async (id: string) => {
    try {
      setTarget(await plan.mutateAsync({ id, reason: t("file.purgeReason") }));
    } catch (e) {
      toast(errorText(e));
    }
  };

  const confirmPurge = async () => {
    if (!target) return;
    try {
      const out = await commit.mutateAsync({
        id: target.id,
        token: target.token,
        confirmed: true,
        noteId,
      });
      setTarget(null);
      toast(out.dropped_object ? t("file.purged") : t("file.purgedKeptObject"));
      if (out.sync_error) toast(t("file.purgeSyncFailed", { error: out.sync_error }));
    } catch (e) {
      // 失敗しても token は使い切られている。下見からやり直させる
      setTarget(null);
      toast(errorText(e));
    }
  };

  return {
    planPurge,
    confirmPurge,
    target,
    clear: () => setTarget(null),
    busy: commit.isPending,
  };
}
