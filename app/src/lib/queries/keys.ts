import type { ActivityFilter, DistillationAiProvider, ObservationTrendFilter } from "@/lib/api";

export const currentNoteMutationScope = { id: "current-note-context" } as const;
export const appUpdateMutationKey = ["appUpdate", "action"] as const;

/** キャッシュキーの一覧。無効化のたびに文字列を書くのをやめ、ここへ集約する。 */
export const queryKeys = {
  appBootMode: ["appBootMode"] as const,
  appUpdate: ["appUpdate"] as const,
  setup: ["setup"] as const,
  settings: ["settings"] as const,
  distillationSettings: ["distillation", "settings"] as const,
  distillationProviders: ["distillation", "providers"] as const,
  distillationModels: (provider: DistillationAiProvider | null) =>
    ["distillation", "models", provider] as const,
  distillationQueue: ["distillation", "queue"] as const,
  aiGuard: ["aiGuard"] as const,
  autostart: ["autostart"] as const,
  home: ["home"] as const,
  noteRevision: ["noteRevision"] as const,
  proposals: ["proposals"] as const,
  proposalList: ["proposals", "list"] as const,
  proposal: (id: string) => ["proposals", "detail", id] as const,
  observationHealth: ["observationHealth"] as const,
  observationTrend: (dayBoundariesMs: number[], filter: ObservationTrendFilter, disabled = false) =>
    ["observationHealth", "trend", dayBoundariesMs, filter, disabled] as const,
  maintenance: ["maintenance"] as const,
  noteCountHistory: ["noteCountHistory"] as const,
  noteCountTrend: (localToday: string) => ["noteCountHistory", "trend", localToday] as const,
  tagOverview: ["tagOverview"] as const,
  favorites: ["favorites"] as const,
  graph: ["graph"] as const,
  connect: ["connect"] as const,
  clientRegistrations: ["clientRegistrations"] as const,
  clientDiagnostics: ["clientDiagnostics"] as const,
  githubAuth: ["githubAuth"] as const,
  notes: ["note"] as const,
  note: (id: string) => ["note", id] as const,
  noteProvenance: (id: string) => ["noteProvenance", id] as const,
  noteHistory: (id: string, limit: number, withDiff: boolean) =>
    ["noteHistory", id, limit, withDiff] as const,
  activityFeed: (limit: number, filter: ActivityFilter) => ["activityFeed", limit, filter] as const,
  noteFiles: (id: string) => ["noteFiles", id] as const,
  files: ["files"] as const,
  filePreview: (id: string) => ["filePreview", id] as const,
  noteCategories: ["noteCategories"] as const,
  noteLists: ["noteList"] as const,
  noteBrowse: (tags: string[], period: string, sort: string) =>
    ["noteList", "browse", tags, period, sort] as const,
  noteList: (category: string) => ["noteList", category] as const,
  searches: ["search"] as const,
  search: (query: string) => ["search", query] as const,
};
