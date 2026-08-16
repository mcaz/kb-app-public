import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

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

export const useNoteCategories = () =>
  useQuery({ queryKey: queryKeys.noteCategories, queryFn: api.noteCategories });

/** カテゴリを選んだ時だけ100件ずつ取得し、全ノートを初期表示へ載せない。 */
export const useCategoryNotes = (category: string | null) =>
  useInfiniteQuery({
    queryKey: queryKeys.noteList(category ?? ""),
    queryFn: ({ pageParam }) => api.noteList(category!, pageParam, 100),
    initialPageParam: null as string | null,
    getNextPageParam: (lastPage) => lastPage.next_cursor ?? undefined,
    enabled: category !== null,
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

export function useOnboardExisting() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (url: string) => api.onboardExisting(url),
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

/** そのノートのファイル(最新版だけ)と、まだ移行していない旧添付。 */
export const useNoteFiles = (id: string | null) =>
  useQuery({
    queryKey: queryKeys.noteFiles(id ?? ""),
    queryFn: () => api.noteFiles(id!),
    enabled: id !== null,
  });

/**
 * ファイルの操作。ファイル欄と、添付件数を持つノート一覧を更新する。
 */
function useFileMutation<TArgs, TData>(
  fn: (args: TArgs) => Promise<TData>,
  noteId: (args: TArgs) => string,
) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: fn,
    onSuccess: async (_data, args) => {
      await Promise.all([
        qc.invalidateQueries({ queryKey: queryKeys.noteFiles(noteId(args)) }),
        qc.invalidateQueries({ queryKey: queryKeys.noteLists }),
      ]);
    },
  });
}

export const useFileAdd = () =>
  useFileMutation(
    (a: { noteId: string; path: string; supersedes?: string }) =>
      api.fileAdd(a.noteId, a.path, a.supersedes ?? null),
    (a) => a.noteId,
  );

export const useFileAddFromClipboard = () =>
  useFileMutation(
    (a: { noteId: string }) => api.fileAddFromClipboard(a.noteId),
    (a) => a.noteId,
  );

export const useFileDetach = () =>
  useFileMutation(
    (a: { noteId: string; id: string; expectedVersion: number }) =>
      api.fileDetach(a.noteId, a.id, a.expectedVersion),
    (a) => a.noteId,
  );

/** 開くだけ。台帳も画面の一覧も変わらないので、無効化しない。 */
export const useFileOpen = () => useMutation({ mutationFn: (id: string) => api.fileOpen(id) });

export const useLegacyOpen = () =>
  useMutation({
    mutationFn: (a: { noteId: string; name: string }) => api.legacyOpen(a.noteId, a.name),
  });

export const useFileFetch = () =>
  useFileMutation(
    (a: { noteId: string; id: string }) => api.fileFetch(a.id),
    (a) => a.noteId,
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

export function useBackupCreateRepository() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (name: string) => api.backupCreateRepository(name),
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
