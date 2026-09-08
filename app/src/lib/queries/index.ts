import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { useLocalDayBoundaries } from "@/hooks/useLocalDayBoundaries";
import {
  api,
  type Favorite,
  type Period,
  type SortKey,
  type DecisionInput,
  type ObservationTrend,
  type ObservationTrendFilter,
  type DistillationAiProvider,
} from "@/lib/api";

import { currentNoteMutationScope, queryKeys } from "./keys";
import { liveQueryOptions } from "./refreshPolicy";

export { queryKeys };

/** 起動モードはプロセス固定。復旧画面から通常queryを開始しない。 */
export const useAppBootMode = () =>
  useQuery({
    queryKey: queryKeys.appBootMode,
    queryFn: api.appBootMode,
    staleTime: Infinity,
    retry: false,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
    refetchOnMount: false,
  });

export const useRecoveryPlan = () => useMutation({ mutationFn: api.recoveryPlan, retry: false });

export const useRecoveryApply = () => useMutation({ mutationFn: api.recoveryApply, retry: false });

export const useRecoveryExit = () => useMutation({ mutationFn: api.recoveryExit, retry: false });

export const useProposals = () =>
  useQuery({
    queryKey: queryKeys.proposalList,
    queryFn: api.proposalList,
    ...liveQueryOptions,
  });

export const useProposal = (note: string | null) =>
  useQuery({
    queryKey: queryKeys.proposal(note ?? ""),
    queryFn: () => api.proposalGet(note ?? ""),
    enabled: note !== null,
    ...liveQueryOptions,
  });

export function useDecideProposal() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({
      note,
      expectedEtag,
      input,
    }: {
      note: string;
      expectedEtag: string;
      input: DecisionInput;
    }) => api.proposalDecide(note, expectedEtag, input),
    onSuccess: (result) => {
      qc.setQueryData(queryKeys.proposal(result.ticket.note_id), {
        ticket: result.ticket,
        degraded: result.degraded,
      });
    },
    onSettled: async () => {
      await Promise.all([
        qc.invalidateQueries({ queryKey: queryKeys.proposals }),
        qc.invalidateQueries({ queryKey: queryKeys.home }),
        qc.invalidateQueries({ queryKey: queryKeys.notes }),
      ]);
    },
  });
}

/**
 * サーバ由来の状態はすべてここ経由。旧実装が state に手書きしていたキャッシュ
 * (tagOv / graphCache)と、各所に散っていた再取得はこの層に集約した(ADR-0002)。
 */

export const useSetupState = () => useQuery({ queryKey: queryKeys.setup, queryFn: api.setupState });

export const useSettings = () =>
  useQuery({ queryKey: queryKeys.settings, queryFn: api.settingsGet, ...liveQueryOptions });

export const useDistillationSettings = () =>
  useQuery({
    queryKey: queryKeys.distillationSettings,
    queryFn: api.distillationSettingsGet,
    ...liveQueryOptions,
  });

export const useDistillationProviders = () =>
  useQuery({
    queryKey: queryKeys.distillationProviders,
    queryFn: api.distillationProviders,
    ...liveQueryOptions,
  });

export const useDistillationModels = (provider: DistillationAiProvider | null) =>
  useQuery({
    queryKey: queryKeys.distillationModels(provider),
    queryFn: () => {
      if (provider === null) throw new Error("Model catalog requires a provider");
      return api.distillationModels(provider);
    },
    enabled: provider !== null,
    staleTime: 60_000,
    refetchOnWindowFocus: false,
    retry: false,
  });

export const useDistillationQueue = () =>
  useQuery({
    queryKey: queryKeys.distillationQueue,
    queryFn: api.distillationQueueStatus,
    ...liveQueryOptions,
  });

export function useSetDistillationSettings() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: api.distillationSettingsSet,
    onSuccess: (settings) => {
      qc.setQueryData(queryKeys.distillationSettings, settings);
      void qc.invalidateQueries({ queryKey: queryKeys.distillationQueue });
    },
  });
}

export function useRetryDistillation() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: api.distillationRetryFailed,
    onSuccess: (status) => {
      qc.setQueryData(queryKeys.distillationQueue, status);
    },
  });
}

export function useRequestDistillationNow() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: api.distillationRequestNow,
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: queryKeys.distillationQueue });
    },
  });
}

export const useAiGuardStatus = () =>
  useQuery({ queryKey: queryKeys.aiGuard, queryFn: api.settingsAiGuardStatus });

/** ログイン自動起動。正本はOSのログイン項目なので、設定ファイルとは別に引く。 */
export const useAutostart = () =>
  useQuery({ queryKey: queryKeys.autostart, queryFn: api.autostartStatus });

export function useSetAutostart() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: api.autostartSet,
    onSuccess: (state) => {
      qc.setQueryData(queryKeys.autostart, state);
    },
  });
}

export function useInstallAiGuard() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: api.settingsInstallAiGuard,
    onSuccess: (status) => {
      qc.setQueryData(queryKeys.aiGuard, status);
    },
  });
}

export function useEnableAiGuardDevelopmentMode() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: api.settingsEnableAiGuardDevelopmentMode,
    onSuccess: (status) => {
      qc.setQueryData(queryKeys.aiGuard, status);
    },
  });
}

export function useSetAiKbEnabled() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: api.settingsSetAiKbEnabled,
    onSuccess: (settings) => {
      qc.setQueryData(queryKeys.settings, settings);
      void qc.invalidateQueries({ queryKey: queryKeys.observationHealth });
      void qc.invalidateQueries({ queryKey: queryKeys.distillationQueue });
    },
  });
}

export function useSetClaudeKbEnabled() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: api.settingsSetClaudeKbEnabled,
    onSuccess: (settings) => {
      qc.setQueryData(queryKeys.settings, settings);
      void qc.invalidateQueries({ queryKey: queryKeys.observationHealth });
      void qc.invalidateQueries({ queryKey: queryKeys.distillationQueue });
    },
  });
}

export function useSetHarvestStatusLine() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: api.settingsSetHarvestStatusLine,
    onSuccess: (settings) => {
      qc.setQueryData(queryKeys.settings, settings);
    },
  });
}

export function useSetGptKbEnabled() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: api.settingsSetGptKbEnabled,
    onSuccess: (settings) => {
      qc.setQueryData(queryKeys.settings, settings);
      void qc.invalidateQueries({ queryKey: queryKeys.observationHealth });
      void qc.invalidateQueries({ queryKey: queryKeys.distillationQueue });
    },
  });
}

export const useHomeState = (enabled = true) =>
  useQuery({ queryKey: queryKeys.home, queryFn: api.homeState, enabled, ...liveQueryOptions });

/** 変更トークンだけを短周期で読み、本文や一覧は変更があったときに再取得する。 */
export const useNoteRevision = (enabled: boolean) =>
  useQuery({
    queryKey: queryKeys.noteRevision,
    queryFn: api.noteRevision,
    enabled,
    staleTime: 0,
    retry: false,
    refetchInterval: 1_000,
    // nativeのfocusでenabledを切る。Webviewのvisibilityが遅れても表示中は止めない。
    refetchIntervalInBackground: true,
    refetchOnWindowFocus: false,
    networkMode: "always",
  });

/** 台帳だけを再取得する。Home以外では購読せず、Vault保守や一覧更新へ波及させない。 */
export const useObservationHealth = () =>
  useQuery({
    queryKey: queryKeys.observationHealth,
    queryFn: api.homeObservationHealth,
    ...liveQueryOptions,
  });

/** 日別集計は端末の暦日を使う。既存の直近14日合計の期間定義は変えない。 */
export function useObservationTrend(filter: ObservationTrendFilter) {
  const dayBoundariesMs = useLocalDayBoundaries();
  const { data: settings } = useSettings();
  const disabled =
    settings?.ai_kb_enabled === false ||
    (settings?.claude_kb_enabled === false && settings?.gpt_kb_enabled === false);
  return useQuery({
    // OFF後に非表示だった接続元へ戻っても、ON時の成功キャッシュを再表示しない。
    queryKey: queryKeys.observationTrend(dayBoundariesMs, filter, disabled),
    queryFn: (): Promise<ObservationTrend> =>
      disabled
        ? Promise.resolve({ status: "disabled", days: [] })
        : api.homeObservationTrend(dayBoundariesMs, filter),
    ...liveQueryOptions,
  });
}

/** 観測日に固定された履歴を読むため、端末の今日を暦日の文字列で渡す。 */
export function useNoteCountTrend() {
  const boundaries = useLocalDayBoundaries();
  const today = new Date(boundaries[13]!);
  const localToday = `${today.getFullYear()}-${String(today.getMonth() + 1).padStart(2, "0")}-${String(today.getDate()).padStart(2, "0")}`;
  return useQuery({
    queryKey: queryKeys.noteCountTrend(localToday),
    queryFn: () => api.homeNoteCountTrend(localToday),
    ...liveQueryOptions,
  });
}

/** Git pullやMarkdown export等はこのqueryだけがバックグラウンドで起動する。 */
export const useMaintenanceRefresh = (enabled = true) =>
  useQuery({
    queryKey: queryKeys.maintenance,
    queryFn: api.maintenanceRefresh,
    enabled,
    staleTime: 60_000,
    refetchInterval: 60_000,
    refetchOnWindowFocus: false,
    networkMode: "always",
  });

export const useTagOverview = () =>
  useQuery({ queryKey: queryKeys.tagOverview, queryFn: api.tagOverview, ...liveQueryOptions });

export const useFavorites = () =>
  useQuery({ queryKey: queryKeys.favorites, queryFn: api.favoritesList });

export const useGraphData = () =>
  useQuery({ queryKey: queryKeys.graph, queryFn: api.graphData, ...liveQueryOptions });

export const useConnectState = () =>
  useQuery({ queryKey: queryKeys.connect, queryFn: api.connectState });

/** 復旧前の証拠なので、自動実行・キャッシュの無効化・再試行は行わない。 */
export const useInspectRuntimeStorage = () =>
  useMutation({ mutationFn: api.inspectRuntimeStorage, retry: false });

export const usePlanRuntimeRecovery = () =>
  useMutation({ mutationFn: api.planRuntimeRecovery, retry: false });

export const useGitHubAuthState = (enabled = true) =>
  useQuery({
    queryKey: queryKeys.githubAuth,
    queryFn: api.githubAuthState,
    enabled,
    // ConnectPageとGitHubAuthPanelが同じ状態を購読する。既定のstaleTime=0だと
    // 後からmountした購読者がKeychain読み出しを即座に再実行する。
    staleTime: 30_000,
  });

export const useNote = (id: string | null) =>
  useQuery({
    queryKey: queryKeys.note(id ?? ""),
    queryFn: () => api.noteGet(id!),
    enabled: id !== null,
    ...liveQueryOptions,
  });

export const useNoteCategories = (enabled = true) =>
  useQuery({
    queryKey: queryKeys.noteCategories,
    queryFn: api.noteCategories,
    enabled,
    ...liveQueryOptions,
  });

/** カテゴリを選んだ時だけ100件ずつ取得し、全ノートを初期表示へ載せない。 */
export const useCategoryNotes = (category: string | null) =>
  useInfiniteQuery({
    queryKey: queryKeys.noteList(category ?? ""),
    queryFn: ({ pageParam }) => api.noteList(category!, pageParam, 100),
    initialPageParam: null as string | null,
    getNextPageParam: (lastPage) => lastPage.next_cursor ?? undefined,
    enabled: category !== null,
    ...liveQueryOptions,
  });

/** 全件一覧の条件はページ分割前にDBへ渡し、未取得ページの一致も見落とさない。 */
export const useNoteBrowse = (tags: string[], period: Period, sort: SortKey, enabled = true) =>
  useInfiniteQuery({
    queryKey: queryKeys.noteBrowse(tags, period, sort),
    queryFn: ({ pageParam }) => api.noteBrowse(tags, period, sort, pageParam, 100),
    initialPageParam: null as string | null,
    getNextPageParam: (lastPage) => lastPage.next_cursor ?? undefined,
    enabled,
    ...liveQueryOptions,
  });

/** 全文検索は空文字なら投げない。 */
export const useNoteSearch = (query: string, enabled = true) =>
  useQuery({
    queryKey: queryKeys.search(query),
    queryFn: () => api.noteSearch(query),
    enabled: enabled && query.trim().length > 0,
    placeholderData: (previous) => previous,
    ...liveQueryOptions,
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
    onError: async () => {
      await qc.invalidateQueries({ queryKey: queryKeys.githubAuth });
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
    ...liveQueryOptions,
  });

export const useFiles = () =>
  useQuery({
    queryKey: queryKeys.files,
    queryFn: api.filesList,
    ...liveQueryOptions,
  });

export const useFilePreview = (id: string | null) =>
  useQuery({
    queryKey: queryKeys.filePreview(id ?? ""),
    queryFn: () => api.filePreview(id!),
    enabled: id !== null,
  });

/**
 * ファイルの操作。ファイル欄と、添付件数を持つノート一覧を更新する。
 */
function useFileMutation<TArgs, TData>(
  fn: (args: TArgs) => Promise<TData>,
  /** ノートの外から呼ぶ操作(ファイル一覧からの purge 等)では undefined。 */
  noteId: (args: TArgs) => string | undefined,
) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: fn,
    onSuccess: async (_data, args) => {
      const note = noteId(args);
      await Promise.all([
        ...(note ? [qc.invalidateQueries({ queryKey: queryKeys.noteFiles(note) })] : []),
        qc.invalidateQueries({ queryKey: queryKeys.files }),
        // ノート一覧の file_count もこの操作で変わる
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

/** 下見は読み取りだけ。何も変わらないので無効化しない。 */
export const useFilePurgePlan = () =>
  useMutation({
    mutationFn: (a: { id: string; reason: string }) => api.filePurgePlan(a.id, a.reason),
  });

export const useFilePurgeCommit = () =>
  useFileMutation(
    (a: { id: string; token: string; noteId?: string }) => api.filePurgeCommit(a.id, a.token),
    (a) => a.noteId,
  );

/** 開くだけ。台帳も画面の一覧も変わらないので、無効化しない。 */
export const useFileOpen = () => useMutation({ mutationFn: (id: string) => api.fileOpen(id) });

export const useFileDownload = () =>
  useMutation({ mutationFn: (id: string) => api.fileDownload(id) });

export const useLegacyOpen = () =>
  useMutation({
    mutationFn: (a: { noteId: string; name: string }) => api.legacyOpen(a.noteId, a.name),
  });

export const useFileFetch = () => {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (a: { noteId?: string; id: string }) => api.fileFetch(a.id),
    onSuccess: async (_data, args) => {
      const updates = [qc.invalidateQueries({ queryKey: queryKeys.files })];
      if (args.noteId) {
        updates.push(qc.invalidateQueries({ queryKey: queryKeys.noteFiles(args.noteId) }));
      }
      await Promise.all(updates);
    },
  });
};

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
    onSettled: async () => {
      await qc.invalidateQueries({ queryKey: queryKeys.githubAuth });
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
    onSettled: async () => {
      await qc.invalidateQueries({ queryKey: queryKeys.githubAuth });
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
    onSettled: async () => {
      await qc.invalidateQueries({ queryKey: queryKeys.githubAuth });
    },
  });
}

export function useGitHubSignIn() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: api.githubSignIn,
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: queryKeys.githubAuth });
    },
  });
}

export function useGitHubSignOut() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: api.githubSignOut,
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: queryKeys.githubAuth });
    },
  });
}

export const useGitHubOpenDevicePage = () => useMutation({ mutationFn: api.githubOpenDevicePage });

export const useLaunchAi = () =>
  useMutation({
    mutationFn: (id: string | null) => api.launchAi(id),
    scope: currentNoteMutationScope,
    networkMode: "always",
  });
