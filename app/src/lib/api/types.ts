import type {
  AiGuardStatus,
  AutostartState,
  ConnectState,
  Degradation,
  Favorite_Serialize,
  FileCard,
  FilesPage,
  GraphData,
  GitHubAuthState,
  Hit_Serialize,
  HomeState_Serialize,
  MaintenanceReport,
  NoteCategories,
  NoteCategory,
  NoteListPage,
  NoteSummary,
  NoteView,
  PreviewFile,
  SearchOutcome_Serialize,
  Settings,
  SetupState,
  TagInfo,
  TagOverview,
} from "@/lib/bindings";

/**
 * 生成物(bindings.ts)は serde の Serialize / Deserialize で型を分けて出す。
 * 画面が扱うのは常に「コアから返ってきた値」= Serialize 側なので、ここで
 * 製品の語彙に寄せた別名を1箇所だけ用意して、以降はこれを使う。
 */
export type Hit = Hit_Serialize;
export type HomeState = HomeState_Serialize;
export type SearchOutcome = SearchOutcome_Serialize;
export type Favorite = Favorite_Serialize;

export type {
  AiGuardStatus,
  AutostartState,
  ConnectState,
  Degradation,
  FileCard,
  FilesPage,
  GraphData,
  GitHubAuthState,
  MaintenanceReport,
  NoteCategories,
  NoteCategory,
  NoteListPage,
  NoteSummary,
  NoteView,
  PreviewFile,
  Settings,
  SetupState,
  TagInfo,
  TagOverview,
};
export type { Added, Availability, FileRow, LegacyFile, NoteFiles } from "@/lib/bindings";
export type { CareProposal, GraphNode, Stats } from "@/lib/bindings";

/** 一覧の絞り込み条件(お気に入りとして保存できる単位)。 */
export type Period = "all" | "7" | "30" | "90";
export type SortKey = "updated" | "created" | "title";
