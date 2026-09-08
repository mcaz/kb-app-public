import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { useErrorText } from "@/hooks/useErrorText";

export function useRuntimeReportCopy(
  collect: () => Promise<object>,
  namespace: "diagnostics" | "recoveryPlan",
) {
  const { t } = useTranslation("connect");
  const errorText = useErrorText();
  const [copying, setCopying] = useState(false);
  const [pendingJson, setPendingJson] = useState<string | null>(null);
  const progressLabel = t(`${namespace}.${pendingJson === null ? "inspecting" : "copying"}`);
  const buttonLabel = copying
    ? progressLabel
    : t(`${namespace}.${pendingJson === null ? "copy" : "copySaved"}`);

  async function copy() {
    if (copying) return;
    setCopying(true);
    const toastId = toast.loading(progressLabel);
    try {
      let json = pendingJson;
      if (json === null) json = JSON.stringify(await collect(), null, 2);
      try {
        // WebViewが取得待ち後のclipboard書込を拒否した場合、次のクリックではawait前に開始する。
        await navigator.clipboard.writeText(json);
        setPendingJson(null);
        toast.success(t(`${namespace}.copied`), { id: toastId });
      } catch {
        setPendingJson(json);
        toast.error(t(`${namespace}.copyFailed`), { id: toastId });
      }
    } catch (failure) {
      toast.error(t(`${namespace}.failed`, { error: errorText(failure) }), { id: toastId });
    } finally {
      setCopying(false);
    }
  }

  return { copying, buttonLabel, copy };
}
