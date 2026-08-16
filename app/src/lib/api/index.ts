import { commands } from "@/lib/bindings";

import { KbError } from "./error";

import type { AppError } from "@/lib/bindings";

import type {
  Availability,
  ConnectState,
  Favorite,
  GraphData,
  HomeState,
  NoteCategory,
  NoteFiles,
  NoteListPage,
  NoteView,
  SearchOutcome,
  SetupState,
  TagOverview,
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
  setupState: async (): Promise<SetupState> =>
    IN_TAURI ? unwrap(commands.setupState()) : (await demo()).setupState(),
  onboard: async (): Promise<SetupState> =>
    IN_TAURI ? unwrap(commands.onboard()) : (await demo()).onboard(),
  onboardExisting: async (url: string): Promise<SetupState> =>
    IN_TAURI ? unwrap(commands.onboardExisting(url)) : (await demo()).onboard(),
  homeState: async (): Promise<HomeState> =>
    IN_TAURI ? unwrap(commands.homeState()) : (await demo()).homeState(),
  tagOverview: async (): Promise<TagOverview> =>
    IN_TAURI ? unwrap(commands.tagOverview()) : (await demo()).tagOverview(),
  noteGet: async (id: string): Promise<NoteView> =>
    IN_TAURI ? unwrap(commands.noteGet(id)) : (await demo()).noteGet(id),
  noteSearch: async (query: string): Promise<SearchOutcome> =>
    IN_TAURI ? unwrap(commands.noteSearch(query)) : (await demo()).noteSearch(query),
  noteCategories: async (): Promise<NoteCategory[]> =>
    IN_TAURI ? unwrap(commands.noteCategories()) : (await demo()).noteCategories(),
  noteList: async (category: string, after: string | null, limit = 100): Promise<NoteListPage> =>
    IN_TAURI
      ? unwrap(commands.noteList(category, after, limit))
      : (await demo()).noteList(category, after, limit),
  graphData: async (): Promise<GraphData> =>
    IN_TAURI ? unwrap(commands.graphData()) : (await demo()).graphData(),
  connectState: async (): Promise<ConnectState> =>
    IN_TAURI ? unwrap(commands.connectState()) : (await demo()).connectState(),

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
  fileFetch: async (id: string): Promise<Availability> =>
    IN_TAURI ? unwrap(commands.fileFetch(id)) : (await demo()).fileFetch(),
  /** 中身はコアの resolver 経由でしか出てこない(画面はパスを受け取らない)。 */
  fileOpen: async (id: string): Promise<null> =>
    IN_TAURI ? unwrap(commands.fileOpen(id)) : (await demo()).fileOpen(),
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
