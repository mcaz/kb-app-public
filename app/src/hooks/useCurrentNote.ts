import { useMutation } from "@tanstack/react-query";
import { useEffect, useRef } from "react";

import { api } from "@/lib/api";
import { currentNoteMutationScope } from "@/lib/queries/keys";

/** 本文の再取得ではなく、主画面で本文が表示された選択だけをAIへ引き渡す。 */
export function useCurrentNote(noteId: string | null) {
  const { mutate, reset, data, error, variables } = useMutation({
    mutationFn: (id: string) => api.noteSetCurrent(id),
    scope: currentNoteMutationScope,
    networkMode: "always",
    retry: false,
  });
  const requestedId = useRef<string | null>(null);

  useEffect(() => {
    if (requestedId.current === noteId) return;
    requestedId.current = noteId;
    if (noteId === null) reset();
    else mutate(noteId);
  }, [mutate, reset, noteId]);

  // 同じhookの選択が変わった描画でも、前のノートの診断を一瞬表示しない。
  return {
    degraded: variables === noteId ? (data ?? []) : [],
    error: variables === noteId ? error : null,
  };
}
