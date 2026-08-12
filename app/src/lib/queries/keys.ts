/** キャッシュキーの一覧。無効化のたびに文字列を書くのをやめ、ここへ集約する。 */
export const queryKeys = {
  setup: ["setup"] as const,
  home: ["home"] as const,
  tagOverview: ["tagOverview"] as const,
  favorites: ["favorites"] as const,
  graph: ["graph"] as const,
  connect: ["connect"] as const,
  note: (id: string) => ["note", id] as const,
  noteFiles: (id: string) => ["noteFiles", id] as const,
  search: (query: string) => ["search", query] as const,
};
