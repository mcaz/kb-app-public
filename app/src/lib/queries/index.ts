import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { api, type Favorite } from "@/lib/api";

import { queryKeys } from "./keys";

export { queryKeys };

/**
 * サーバ由来の状態はすべてここ経由。旧実装が state に手書きしていたキャッシュ
 * (tagOv / graphCache)と、各所に散っていた再取得はこの層に集約した(ADR-0002)。
 */

export const useSetupState = () => useQuery({ queryKey: queryKeys.setup, queryFn: api.setupState });

export const useHomeState = () => useQuery({ queryKey: queryKeys.home, queryFn: api.homeState });

export const useTagOverview = () =>
  useQuery({ queryKey: queryKeys.tagOverview, queryFn: api.tagOverview });

export const useFavorites = () =>
  useQuery({ queryKey: queryKeys.favorites, queryFn: api.favoritesList });

export const useGraphData = () => useQuery({ queryKey: queryKeys.graph, queryFn: api.graphData });

export const useConnectState = () =>
  useQuery({ queryKey: queryKeys.connect, queryFn: api.connectState });

export const useNote = (id: string | null) =>
  useQuery({
    queryKey: queryKeys.note(id ?? ""),
    queryFn: () => api.noteGet(id!),
    enabled: id !== null,
  });

/** 検索は空文字なら投げない(一覧はホームの recent を使う)。 */
export const useNoteSearch = (query: string) =>
  useQuery({
    queryKey: queryKeys.search(query),
    queryFn: () => api.noteSearch(query),
    enabled: query.trim().length > 0,
    placeholderData: (previous) => previous,
  });

export function useOnboard() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: api.onboard,
    onSuccess: async () => {
      await qc.invalidateQueries();
    },
  });
}

export function useCareDismiss() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (key: string) => api.careDismiss(key),
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: queryKeys.home });
    },
  });
}

export function useFavoriteAdd() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (fav: Favorite) => api.favoriteAdd(fav),
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: queryKeys.favorites });
    },
  });
}

export function useFavoriteRemove() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (name: string) => api.favoriteRemove(name),
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: queryKeys.favorites });
    },
  });
}

/** 添付の増減はノートの内容を変えるので、そのノートだけ引き直す。 */
function useNoteMutation<TArgs, TData>(
  fn: (args: TArgs) => Promise<TData>,
  noteId: (args: TArgs) => string,
) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: fn,
    onSuccess: async (_data, args) => {
      await qc.invalidateQueries({ queryKey: queryKeys.note(noteId(args)) });
    },
  });
}

export const useAttachmentAdd = () =>
  useNoteMutation(
    (a: { id: string; name: string; dataBase64: string }) =>
      api.attachmentAdd(a.id, a.name, a.dataBase64),
    (a) => a.id,
  );

export const useAttachmentAddFromPath = () =>
  useNoteMutation(
    (a: { id: string; path: string }) => api.attachmentAddFromPath(a.id, a.path),
    (a) => a.id,
  );

export const useAttachmentPaste = () =>
  useNoteMutation(
    (a: { id: string }) => api.attachmentPaste(a.id),
    (a) => a.id,
  );

export const useAttachmentRemove = () =>
  useNoteMutation(
    (a: { id: string; name: string }) => api.attachmentRemove(a.id, a.name),
    (a) => a.id,
  );

export function useConnectDesktop() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: api.connectDesktop,
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: queryKeys.connect });
    },
  });
}

export function useEmbedEnable() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: api.embedEnable,
    onSuccess: async () => {
      await Promise.all([
        qc.invalidateQueries({ queryKey: queryKeys.connect }),
        qc.invalidateQueries({ queryKey: queryKeys.home }),
      ]);
    },
  });
}

export function useBackupSetRemote() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (url: string) => api.backupSetRemote(url),
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: queryKeys.connect });
    },
  });
}

export function useBackupNow() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: api.backupNow,
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: queryKeys.connect });
    },
  });
}

export const useLaunchAi = () =>
  useMutation({ mutationFn: (id: string | null) => api.launchAi(id) });
