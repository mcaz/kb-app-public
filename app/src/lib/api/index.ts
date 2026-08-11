import { commands } from "@/lib/bindings";

import { KbError } from "./error";

import type { AppError } from "@/lib/bindings";

import type {
  ConnectState,
  Favorite,
  GraphData,
  HomeState,
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
  homeState: async (): Promise<HomeState> =>
    IN_TAURI ? unwrap(commands.homeState()) : (await demo()).homeState(),
  tagOverview: async (): Promise<TagOverview> =>
    IN_TAURI ? unwrap(commands.tagOverview()) : (await demo()).tagOverview(),
  noteGet: async (id: string): Promise<NoteView> =>
    IN_TAURI ? unwrap(commands.noteGet(id)) : (await demo()).noteGet(id),
  noteSearch: async (query: string): Promise<SearchOutcome> =>
    IN_TAURI ? unwrap(commands.noteSearch(query)) : (await demo()).noteSearch(query),
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

  /** 戻り値は [保存名, 警告]。 */
  attachmentAdd: async (id: string, name: string, dataBase64: string) =>
    IN_TAURI
      ? unwrap(commands.attachmentAdd(id, name, dataBase64))
      : (await demo()).attachmentAdd(id, name, dataBase64),
  attachmentAddFromPath: async (id: string, path: string) =>
    IN_TAURI
      ? unwrap(commands.attachmentAddFromPath(id, path))
      : (await demo()).attachmentAddFromPath(id, path),
  attachmentPaste: async (id: string) =>
    IN_TAURI ? unwrap(commands.attachmentPaste(id)) : (await demo()).attachmentPaste(id),
  attachmentRemove: async (id: string, name: string): Promise<null> =>
    IN_TAURI ? unwrap(commands.attachmentRemove(id, name)) : (await demo()).attachmentRemove(),

  connectDesktop: async (): Promise<null> =>
    IN_TAURI ? unwrap(commands.connectDesktop()) : (await demo()).connectDesktop(),
  embedEnable: async (): Promise<null> =>
    IN_TAURI ? unwrap(commands.embedEnable()) : (await demo()).embedEnable(),
  backupSetRemote: async (url: string): Promise<null> =>
    IN_TAURI ? unwrap(commands.backupSetRemote(url)) : (await demo()).backupSetRemote(url),
  backupNow: async (): Promise<string> =>
    IN_TAURI ? unwrap(commands.backupNow()) : (await demo()).backupNow(),
  launchAi: async (note: string | null): Promise<null> =>
    IN_TAURI ? unwrap(commands.launchAi(note)) : null,
};
