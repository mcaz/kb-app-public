import type {
  ActivityFeedView,
  ActivityFilter_Deserialize,
  ActivityRowView,
  ActivitySummaryView,
  AiGuardStatus,
  AutostartState,
  ConnectState,
  Degradation,
  Favorite_Serialize,
  FieldChangeView,
  FileCard,
  FilesPage,
  GraphData,
  GitHubAuthState,
  Hit_Serialize,
  HomeState_Serialize,
  MaintenanceReport,
  NoteCategories,
  NoteCategory,
  NoteBrowsePage_Serialize,
  NoteEventView,
  NoteListPage,
  NoteSummary,
  NoteView,
  PreviewFile,
  ProvenanceView,
  SearchOutcome_Serialize,
  SectionAuthorView,
  Settings,
  SetupState,
  TagInfo,
  TagOverview,
} from "@/lib/bindings";

export type { UpdateStatus, UpdatePhase, UpdateFailureKind } from "@/lib/bindings";

/**
 * 生成物(bindings.ts)は serde の Serialize / Deserialize で型を分けて出す。
 * 画面が扱うのは常に「コアから返ってきた値」= Serialize 側なので、ここで
 * 製品の語彙に寄せた別名を1箇所だけ用意して、以降はこれを使う。
 */
export type Hit = Hit_Serialize;
export type HomeState = HomeState_Serialize;
export type NoteBrowsePage = NoteBrowsePage_Serialize;
export type SearchOutcome = SearchOutcome_Serialize;
export type Favorite = Favorite_Serialize;
/** 活動フィードの絞り込み。画面は常に「これから送る条件」= Deserialize 側を組み立てる。 */
export type ActivityFilter = ActivityFilter_Deserialize;

export type {
  ActivityFeedView,
  ActivityRowView,
  ActivitySummaryView,
  AiGuardStatus,
  AutostartState,
  ConnectState,
  Degradation,
  FieldChangeView,
  FileCard,
  FilesPage,
  GraphData,
  GitHubAuthState,
  MaintenanceReport,
  NoteCategories,
  NoteCategory,
  NoteEventView,
  NoteListPage,
  NoteSummary,
  NoteView,
  PreviewFile,
  ProvenanceView,
  SectionAuthorView,
  Settings,
  SetupState,
  TagInfo,
  TagOverview,
};
export type {
  Added,
  DistillationModelCatalog,
  Availability,
  FileRow,
  LegacyFile,
  NoteFiles,
  PurgePlan,
  Purged,
} from "@/lib/bindings";
export type { CareProposal, GraphNode, Stats } from "@/lib/bindings";
export type { ObservationHealth, ObservationSurfaceHealth, SurfaceSummary } from "@/lib/bindings";
export type {
  ClientDiagnosticsReport,
  ClientRuleDiagnostics,
  ClientRegistrations,
  ClientRegistrationView,
  ClientBindingStatus,
  ClientSurface,
  RegistrationClient,
  RegistrationStatus,
  RegistrationRepair,
  RegistrationIssueKind,
  GuardTargetState,
} from "@/lib/bindings";
export type { ObservationTrend, ObservationTrendDay, ObservationTrendFilter } from "@/lib/bindings";
export type { NoteCountTrend, NoteCountTrendDay, NoteCountTrendStatus } from "@/lib/bindings";
export type {
  TicketView,
  TicketStatus,
  DecisionInput,
  DecisionOutcome,
  ProposalRevision,
  ProposalReview,
  ProposalDecision,
} from "@/lib/bindings";
export type { ProposalListData, ProposalDetailData, ProposalMutationData } from "@/lib/bindings";
export type {
  DistillationAiSettings,
  DistillationAiProvider,
  DistillationAiProviderStatus,
  DistillationQueueView,
  DistillationIssueView,
  DistillationIssueError,
  ImmediateDistillationScope,
  ImmediateDistillationResult,
  RuntimeDiagnosticsReport,
  RuntimeRecoveryPlan,
  RuntimeRecoveryRequest,
  RuntimeRecoveryReceipt,
  AppBootMode,
} from "@/lib/bindings";

/** 一覧の絞り込み条件(お気に入りとして保存できる単位)。 */
export type Period = "all" | "7" | "30" | "90";
export type SortKey = "updated" | "created" | "title";
