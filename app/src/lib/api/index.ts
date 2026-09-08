import { commands } from "@/lib/bindings";

import { KbError } from "./error";

import type { AppError } from "@/lib/bindings";

import type {
  AiGuardStatus,
  AutostartState,
  Availability,
  ConnectState,
  Degradation,
  Favorite,
  FilesPage,
  GraphData,
  GitHubAuthState,
  HomeState,
  ObservationHealth,
  ObservationTrend,
  ObservationTrendFilter,
  NoteCountTrend,
  MaintenanceReport,
  NoteCategories,
  NoteFiles,
  NoteListPage,
  NoteBrowsePage,
  Period,
  SortKey,
  NoteView,
  PreviewFile,
  PurgePlan,
  Purged,
  SearchOutcome,
  Settings,
  SetupState,
  TagOverview,
  ProposalListData,
  ProposalDetailData,
  ProposalMutationData,
  DecisionInput,
  DistillationAiSettings,
  DistillationAiProviderStatus,
  DistillationAiProvider,
  DistillationModelCatalog,
  DistillationQueueView,
  ImmediateDistillationScope,
  ImmediateDistillationResult,
  RuntimeDiagnosticsReport,
  RuntimeRecoveryPlan,
  RuntimeRecoveryRequest,
  RuntimeRecoveryReceipt,
  AppBootMode,
} from "./types";

export * from "./types";
export * from "./error";

/** Tauri の中で動いているか(外はブラウザプレビュー = デモデータ)。 */
export const IN_TAURI = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

type Result<T> = { status: "ok"; data: T } | { status: "error"; error: AppError };

/**
 * コアの Result を素の値に開き、失敗は例外にする。
 * TanStack Query がエラー状態として拾えるようにするため。
 * エラーは種類を保ったまま運ぶ(画面は code で訳し分ける)。
 */
async function unwrap<T>(promise: Promise<Result<T>>): Promise<T> {
  const res = await promise;
  if (res.status === "error") throw new KbError(res.error);
  return res.data;
}

/** デモデータは Tauri 外でしか使わないので、実行時に初めて読み込む。 */
async function demo() {
  return (await import("@/lib/demo")).demoApi;
}

export const api = {
  appBootMode: async (): Promise<AppBootMode> => (IN_TAURI ? commands.appBootMode() : "normal"),
  recoveryPlan: async (): Promise<RuntimeRecoveryPlan> => {
    if (!IN_TAURI) throw new KbError({ code: "unexpected", message: "Desktop recovery only" });
    return unwrap(commands.recoveryPlan());
  },
  recoveryApply: async (input: RuntimeRecoveryRequest): Promise<RuntimeRecoveryReceipt> => {
    if (!IN_TAURI) throw new KbError({ code: "unexpected", message: "Desktop recovery only" });
    return unwrap(commands.recoveryApply(input));
  },
  recoveryExit: async (): Promise<void> => {
    if (!IN_TAURI) throw new KbError({ code: "unexpected", message: "Desktop recovery only" });
    await commands.recoveryExit();
  },
  proposalList: async (): Promise<ProposalListData> =>
    IN_TAURI ? unwrap(commands.proposalList()) : (await demo()).proposalList(),
  proposalGet: async (note: string): Promise<ProposalDetailData> =>
    IN_TAURI ? unwrap(commands.proposalGet(note)) : (await demo()).proposalGet(note),
  proposalDecide: async (
    note: string,
    expectedEtag: string,
    input: DecisionInput,
  ): Promise<ProposalMutationData> =>
    IN_TAURI
      ? unwrap(commands.proposalDecide(note, expectedEtag, input))
      : (await demo()).proposalDecide(note, expectedEtag, input),
  setupState: async (): Promise<SetupState> =>
    IN_TAURI ? unwrap(commands.setupState()) : (await demo()).setupState(),
  inspectRuntimeStorage: async (): Promise<RuntimeDiagnosticsReport> => {
    if (!IN_TAURI) {
      throw new KbError({
        code: "unexpected",
        message: "Storage diagnostics require the desktop app",
      });
    }
    return unwrap(commands.inspectRuntimeStorage());
  },
  planRuntimeRecovery: async (): Promise<RuntimeRecoveryPlan> => {
    if (!IN_TAURI) {
      throw new KbError({
        code: "unexpected",
        message: "Recovery planning requires the desktop app",
      });
    }
    return unwrap(commands.planRuntimeRecovery());
  },
  settingsGet: async (): Promise<Settings> =>
    IN_TAURI ? unwrap(commands.settingsGet()) : (await demo()).settingsGet(),
  distillationSettingsGet: async (): Promise<DistillationAiSettings> =>
    IN_TAURI
      ? unwrap(commands.distillationSettingsGet())
      : (await demo()).distillationSettingsGet(),
  distillationSettingsSet: async (
    settings: DistillationAiSettings,
  ): Promise<DistillationAiSettings> =>
    IN_TAURI
      ? unwrap(commands.distillationSettingsSet(settings))
      : (await demo()).distillationSettingsSet(settings),
  distillationProviders: async (): Promise<DistillationAiProviderStatus[]> =>
    IN_TAURI ? commands.distillationProviders() : (await demo()).distillationProviders(),
  distillationModels: async (
    provider: DistillationAiProvider,
  ): Promise<DistillationModelCatalog> =>
    IN_TAURI ? commands.distillationModels(provider) : (await demo()).distillationModels(provider),
  distillationQueueStatus: async (): Promise<DistillationQueueView> =>
    IN_TAURI
      ? unwrap(commands.distillationQueueStatus())
      : (await demo()).distillationQueueStatus(),
  distillationRetryFailed: async (): Promise<DistillationQueueView> =>
    IN_TAURI
      ? unwrap(commands.distillationRetryFailed())
      : (await demo()).distillationRetryFailed(),
  distillationRequestNow: async (
    scope: ImmediateDistillationScope,
  ): Promise<ImmediateDistillationResult> =>
    IN_TAURI
      ? unwrap(commands.distillationRequestNow(scope))
      : (await demo()).distillationRequestNow(scope),
  settingsSetAiKbEnabled: async (enabled: boolean): Promise<Settings> =>
    IN_TAURI
      ? unwrap(commands.settingsSetAiKbEnabled(enabled))
      : (await demo()).settingsSetAiKbEnabled(enabled),
  settingsSetClaudeKbEnabled: async (enabled: boolean): Promise<Settings> =>
    IN_TAURI
      ? unwrap(commands.settingsSetClaudeKbEnabled(enabled))
      : (await demo()).settingsSetClaudeKbEnabled(enabled),
  settingsSetGptKbEnabled: async (enabled: boolean): Promise<Settings> =>
    IN_TAURI
      ? unwrap(commands.settingsSetGptKbEnabled(enabled))
      : (await demo()).settingsSetGptKbEnabled(enabled),
  settingsSetHarvestStatusLine: async (enabled: boolean): Promise<Settings> =>
    IN_TAURI
      ? unwrap(commands.settingsSetHarvestStatusLine(enabled))
      : (await demo()).settingsSetHarvestStatusLine(enabled),
  settingsAiGuardStatus: async (): Promise<AiGuardStatus> =>
    IN_TAURI ? unwrap(commands.settingsAiGuardStatus()) : (await demo()).settingsAiGuardStatus(),
  settingsInstallAiGuard: async (): Promise<AiGuardStatus> =>
    IN_TAURI ? unwrap(commands.settingsInstallAiGuard()) : (await demo()).settingsInstallAiGuard(),
  settingsEnableAiGuardDevelopmentMode: async (): Promise<AiGuardStatus> =>
    IN_TAURI
      ? unwrap(commands.settingsEnableAiGuardDevelopmentMode())
      : (await demo()).settingsEnableAiGuardDevelopmentMode(),
  autostartStatus: async (): Promise<AutostartState> =>
    IN_TAURI ? unwrap(commands.autostartStatus()) : (await demo()).autostartStatus(),
  autostartSet: async (enabled: boolean): Promise<AutostartState> =>
    IN_TAURI ? unwrap(commands.autostartSet(enabled)) : (await demo()).autostartSet(enabled),
  /** trayメニューの文言。ブラウザプレビューにtrayは無いので何もしない。 */
  traySetLabels: async (show: string, quit: string): Promise<void> => {
    if (IN_TAURI) await unwrap(commands.traySetLabels(show, quit));
  },
  windowHide: async (): Promise<void> => {
    if (IN_TAURI) await unwrap(commands.windowHide());
  },
  workspaceTabShortcutsConfigure: async (
    enabled: boolean,
    file: string,
    newTab: string,
    closeTab: string,
  ): Promise<void> => {
    if (IN_TAURI) {
      await unwrap(commands.workspaceTabShortcutsConfigure(enabled, file, newTab, closeTab));
    }
  },
  onboard: async (): Promise<SetupState> =>
    IN_TAURI ? unwrap(commands.onboard()) : (await demo()).onboard(),
  onboardExisting: async (url: string): Promise<SetupState> =>
    IN_TAURI ? unwrap(commands.onboardExisting(url)) : (await demo()).onboard(),
  homeState: async (): Promise<HomeState> =>
    IN_TAURI ? unwrap(commands.homeState()) : (await demo()).homeState(),
  noteRevision: async (): Promise<string> => (IN_TAURI ? unwrap(commands.noteRevision()) : "demo"),
  homeObservationHealth: async (): Promise<ObservationHealth> =>
    IN_TAURI ? unwrap(commands.homeObservationHealth()) : (await demo()).homeObservationHealth(),
  homeObservationTrend: async (
    dayBoundariesMs: number[],
    filter: ObservationTrendFilter,
  ): Promise<ObservationTrend> =>
    IN_TAURI
      ? unwrap(commands.homeObservationTrend(dayBoundariesMs, filter))
      : (await demo()).homeObservationTrend(dayBoundariesMs, filter),
  homeNoteCountTrend: async (localToday: string): Promise<NoteCountTrend> =>
    IN_TAURI
      ? unwrap(commands.homeNoteCountTrend(localToday))
      : (await demo()).homeNoteCountTrend(localToday),
  maintenanceRefresh: async (): Promise<MaintenanceReport> =>
    IN_TAURI ? unwrap(commands.maintenanceRefresh()) : (await demo()).maintenanceRefresh(),
  tagOverview: async (): Promise<TagOverview> =>
    IN_TAURI ? unwrap(commands.tagOverview()) : (await demo()).tagOverview(),
  noteGet: async (id: string): Promise<NoteView> =>
    IN_TAURI ? unwrap(commands.noteGet(id)) : (await demo()).noteGet(id),
  noteSetCurrent: async (id: string): Promise<Degradation[]> => {
    if (IN_TAURI) return unwrap(commands.noteSetCurrent(id));
    await (await demo()).noteGet(id);
    return [];
  },
  noteSearch: async (query: string): Promise<SearchOutcome> =>
    IN_TAURI ? unwrap(commands.noteSearch(query)) : (await demo()).noteSearch(query),
  noteCategories: async (): Promise<NoteCategories> =>
    IN_TAURI ? unwrap(commands.noteCategories()) : (await demo()).noteCategories(),
  noteList: async (category: string, after: string | null, limit = 100): Promise<NoteListPage> =>
    IN_TAURI
      ? unwrap(commands.noteList(category, after, limit))
      : (await demo()).noteList(category, after, limit),
  noteBrowse: async (
    tags: string[],
    period: Period,
    sort: SortKey,
    after: string | null,
    limit = 100,
  ): Promise<NoteBrowsePage> =>
    IN_TAURI
      ? unwrap(commands.noteBrowse(tags, period, sort, after, limit))
      : (await demo()).noteBrowse(tags, period, sort, after, limit),
  graphData: async (): Promise<GraphData> =>
    IN_TAURI ? unwrap(commands.graphData()) : (await demo()).graphData(),
  connectState: async (): Promise<ConnectState> =>
    IN_TAURI ? unwrap(commands.connectState()) : (await demo()).connectState(),
  githubAuthState: async (): Promise<GitHubAuthState> =>
    IN_TAURI ? unwrap(commands.githubAuthState()) : (await demo()).githubAuthState(),
  githubSignIn: async (): Promise<GitHubAuthState> =>
    IN_TAURI ? unwrap(commands.githubSignIn()) : (await demo()).githubSignIn(),
  githubSignOut: async (): Promise<null> =>
    IN_TAURI ? unwrap(commands.githubSignOut()) : (await demo()).githubSignOut(),
  githubOpenDevicePage: async (): Promise<null> =>
    IN_TAURI ? unwrap(commands.githubOpenDevicePage()) : null,

  favoritesList: async (): Promise<Favorite[]> =>
    IN_TAURI ? unwrap(commands.favoritesList()) : (await demo()).favoritesList(),
  favoriteAdd: async (fav: Favorite): Promise<null> =>
    IN_TAURI ? unwrap(commands.favoriteAdd(fav)) : (await demo()).favoriteAdd(fav),
  favoriteRemove: async (name: string): Promise<null> =>
    IN_TAURI ? unwrap(commands.favoriteRemove(name)) : (await demo()).favoriteRemove(name),

  careDismiss: async (key: string): Promise<null> =>
    IN_TAURI ? unwrap(commands.careDismiss(key)) : (await demo()).careDismiss(key),

  noteFiles: async (id: string): Promise<NoteFiles> =>
    IN_TAURI ? unwrap(commands.noteFiles(id)) : (await demo()).noteFiles(id),
  filesList: async (): Promise<FilesPage> =>
    IN_TAURI ? unwrap(commands.filesList()) : (await demo()).filesList(),
  /** 渡すのはパスだけ(中身は運ばない — ADR-0003 決定8)。 */
  fileAdd: async (noteId: string, path: string, supersedes: string | null = null) =>
    IN_TAURI
      ? unwrap(commands.fileAdd(noteId, path, supersedes))
      : (await demo()).fileAdd(noteId, path),
  fileAddFromClipboard: async (noteId: string) =>
    IN_TAURI
      ? unwrap(commands.fileAddFromClipboard(noteId))
      : (await demo()).fileAddFromClipboard(),
  fileDetach: async (noteId: string, id: string, expectedVersion: number): Promise<null> =>
    IN_TAURI
      ? unwrap(commands.fileDetach(noteId, id, expectedVersion))
      : (await demo()).fileDetach(),
  /** purge の下見。まだ消さない。 */
  filePurgePlan: async (id: string, reason: string): Promise<PurgePlan> =>
    IN_TAURI ? unwrap(commands.filePurgePlan(id, reason)) : (await demo()).filePurgePlan(id),
  /**
   * 下見どおりに取り除く。**呼べるのは確認ダイアログの先だけ**なので、コアへ渡す
   * `confirmed` はここで立てる(画面の外へ出さない)。
   */
  filePurgeCommit: async (id: string, token: string): Promise<Purged> =>
    IN_TAURI
      ? unwrap(commands.filePurgeCommit(id, token, true))
      : (await demo()).filePurgeCommit(id),
  fileFetch: async (id: string): Promise<Availability> =>
    IN_TAURI ? unwrap(commands.fileFetch(id)) : (await demo()).fileFetch(),
  /** 中身はコアの resolver 経由でしか出てこない(画面はパスを受け取らない)。 */
  fileOpen: async (id: string): Promise<null> =>
    IN_TAURI ? unwrap(commands.fileOpen(id)) : (await demo()).fileOpen(),
  fileDownload: async (id: string): Promise<boolean> =>
    IN_TAURI ? unwrap(commands.fileDownload(id)) : (await demo()).fileDownload(),
  filePreview: async (id: string): Promise<PreviewFile> =>
    IN_TAURI ? unwrap(commands.filePreview(id)) : (await demo()).filePreview(id),
  legacyOpen: async (noteId: string, name: string): Promise<null> =>
    IN_TAURI ? unwrap(commands.legacyOpen(noteId, name)) : (await demo()).fileOpen(),

  /**
   * ファイルを選ぶ。返るのはパスだけで、中身はここを通らない。
   * dialog プラグインは invoke を包んでいるので、窓口をこの層に揃える(§4)。
   */
  pickFiles: async (multiple = true): Promise<string[]> => {
    if (!IN_TAURI) return (await demo()).pickFiles();
    const { open } = await import("@tauri-apps/plugin-dialog");
    const picked = await open({ multiple });
    if (picked === null) return [];
    return Array.isArray(picked) ? picked : [picked];
  },

  connectDesktop: async (): Promise<null> =>
    IN_TAURI ? unwrap(commands.connectDesktop()) : (await demo()).connectDesktop(),
  embedEnable: async (): Promise<null> =>
    IN_TAURI ? unwrap(commands.embedEnable()) : (await demo()).embedEnable(),
  backupSetRemote: async (url: string): Promise<null> =>
    IN_TAURI ? unwrap(commands.backupSetRemote(url)) : (await demo()).backupSetRemote(url),
  backupCreateRepository: async (name: string): Promise<null> =>
    IN_TAURI ? unwrap(commands.backupCreateRepository(name)) : (await demo()).backupSetRemote(name),
  backupNow: async (): Promise<string> =>
    IN_TAURI ? unwrap(commands.backupNow()) : (await demo()).backupNow(),
  launchAi: async (note: string | null): Promise<null> =>
    IN_TAURI ? unwrap(commands.launchAi(note)) : null,
};
