import { KbError } from "@/lib/api/error";

import type {
  Added,
  Availability,
  CareProposal,
  ConnectState,
  Favorite,
  GraphData,
  FileRow,
  Hit,
  HomeState,
  NoteFiles,
  NoteView,
  SearchOutcome,
  SetupState,
  TagOverview,
} from "@/lib/api/types";

/**
 * ブラウザプレビュー(`npm run dev` を素のブラウザで開いた場合)用のデータ。
 * Tauri の中では読み込まれない(api 側で動的 import しているため本番束にも入らない)。
 * `?screen=onboarding` / `?connect=connected` で状態を切り替えられる。
 */

const params = () => new URLSearchParams(location.search);

const notes: NoteView[] = [
  {
    id: "notes/引っ越し手続きメモ",
    title: "引っ越し手続きメモ",
    status: "stable",
    origin: "agent",
    tags: ["手続き"],
    created_at: "2026-08-02T09:15:00Z",
    generated_at: "2026-08-10T05:00:00Z",
    body:
      "3月末までにやること:\n\n- 電気・ガス・水道の解約(2週間前まで)\n- 転出届 → 転入届(14日以内)\n" +
      "- 住所変更: 免許・銀行・[確定申告の準備](/notes/確定申告の準備.md)にも影響\n",
    related: [["notes/確定申告の準備", "確定申告の準備"]],
    similar: [["notes/沖縄旅行の持ち物リスト", "沖縄旅行の持ち物リスト", 0.42]],
    vault_root: "(demo)",
  },
  {
    id: "notes/確定申告の準備",
    title: "確定申告の準備",
    status: "stable",
    origin: "agent",
    tags: ["手続き", "税金"],
    created_at: "2026-06-20T02:00:00Z",
    generated_at: "2026-07-02T05:00:00Z",
    body: "医療費の領収書を集める。\n",
    related: [],
    similar: [["notes/引っ越し手続きメモ", "引っ越し手続きメモ", 0.38]],
    vault_root: "(demo)",
  },
  {
    id: "notes/沖縄旅行の持ち物リスト",
    title: "沖縄旅行の持ち物リスト",
    status: "stable",
    origin: "agent",
    tags: ["旅行"],
    created_at: "2026-08-10T06:00:00Z",
    generated_at: "2026-08-10T06:00:00Z",
    body: "会話でまとめた持ち物:\n\n- 日焼け止め\n- モバイルバッテリー\n- 子どもの浮き輪\n",
    related: [],
    similar: [],
    vault_root: "(demo)",
  },
];

const toHit = (n: NoteView): Hit => ({
  id: n.id,
  title: n.title,
  status: n.status,
  snippet: n.body.slice(0, 60).replace(/\n/g, " "),
  via: "recent",
  origin: n.origin,
  tags: n.tags,
  created: n.created_at,
  updated: n.generated_at,
});

/** ノートごとのファイル。取得できていない行も1つ置いて、状態の見え方を確かめられるようにする。 */
const files: Record<string, FileRow[]> = {
  "notes/引っ越し手続きメモ": [
    {
      id: "demo-1",
      version: 1,
      name: "間取り図.png",
      size: 245760,
      media_type: "image/png",
      availability: "local",
      sensitivity: "private",
      sync: "full",
      linked: false,
      client_repo: false,
      can_fetch: false,
      added_at: "2026-08-10T05:00:00Z",
    },
    {
      id: "demo-2",
      version: 1,
      name: "内見メモ.pdf",
      size: 1048576,
      media_type: "application/pdf",
      availability: "missing",
      sensitivity: "private",
      sync: "full",
      linked: false,
      client_repo: false,
      can_fetch: true,
      added_at: "2026-08-11T05:00:00Z",
    },
    {
      id: "demo-3",
      version: 1,
      name: "契約書ひな形.docx",
      size: 51200,
      media_type: "application/octet-stream",
      availability: "unavailable_by_policy",
      sensitivity: "private",
      sync: "local_only",
      linked: true,
      client_repo: true,
      can_fetch: false,
      added_at: "2026-08-12T05:00:00Z",
    },
  ],
};

const care: CareProposal[] = [
  {
    key: "broken:notes/引っ越し手続きメモ:notes/新居の契約",
    kind: "broken",
    a: "notes/引っ越し手続きメモ",
    b: "notes/新居の契約",
    detail:
      "「引っ越し手続きメモ」の中のリンク先「notes/新居の契約」がまだありません(未執筆の知識かも)。",
  },
];

let favorites: Favorite[] = [
  { name: "手続きまわり", tags: ["手続き"], query: null, period: "all", sort: "updated" },
];

const connect: ConnectState = {
  desktop: (params().get("connect") as ConnectState["desktop"] | null) ?? "not_connected",
  backup: { remote: null, pending: 4 },
  sync_error: null,
  smart_search: { state: "not_installed", embedded: 0, total: 3 },
};

const delay = <T>(value: T): Promise<T> =>
  new Promise((resolve) => setTimeout(() => resolve(value), 30));

export const demoApi = {
  setupState: (): Promise<SetupState> =>
    delay({
      needs_onboarding: params().get("screen") === "onboarding",
      vault_name: "わたしのノート",
      vault_path: "(demo)",
    }),
  onboard: (): Promise<SetupState> =>
    delay({ needs_onboarding: false, vault_name: "わたしのノート", vault_path: "(demo)" }),
  homeState: (): Promise<HomeState> =>
    delay({
      stats: {
        total: notes.length,
        deprecated: 0,
        memos: 0,
        agent_notes: notes.length,
        links: 2,
        embed_enabled: true,
        embedded: notes.length,
      },
      notes: notes.map(toHit),
      care,
      tags: [
        ["手続き", 2],
        ["旅行", 1],
        ["税金", 1],
      ],
      degraded: [],
    }),
  tagOverview: (): Promise<TagOverview> =>
    delay({
      tags: [
        { tag: "手続き", count: 2, description: "役所・契約など、期限がある手続きの記録" },
        { tag: "税金", count: 1, description: "確定申告まわり" },
        { tag: "旅行", count: 1, description: null },
      ],
      glossary_note: "notes/タグ運用",
    }),
  noteGet: (id: string): Promise<NoteView> => {
    const found = notes.find((n) => n.id === id);
    if (!found) return Promise.reject(new KbError({ code: "note_not_found", id }));
    return delay(found);
  },
  noteSearch: (query: string): Promise<SearchOutcome> =>
    delay({
      hits: notes
        .filter((n) => (n.title + n.body).includes(query))
        .map((n) => ({ ...toHit(n), via: "main" })),
      related: [],
      degraded: [],
    }),
  graphData: (): Promise<GraphData> => {
    const edges: [string, string][] = notes.flatMap((n) =>
      n.related.map(([dst]) => [n.id, dst] as [string, string]),
    );
    return delay({
      nodes: notes.map((n) => ({
        id: n.id,
        title: n.title,
        origin: n.origin,
        status: n.status,
        degree: edges.filter(([s, d]) => s === n.id || d === n.id).length,
      })),
      edges,
    });
  },
  connectState: (): Promise<ConnectState> => delay(connect),
  favoritesList: (): Promise<Favorite[]> => delay(favorites),
  favoriteAdd: (fav: Favorite) => {
    favorites = [...favorites.filter((f) => f.name !== fav.name), fav];
    return delay(null);
  },
  favoriteRemove: (name: string) => {
    favorites = favorites.filter((f) => f.name !== name);
    return delay(null);
  },
  careDismiss: (key: string) => {
    const i = care.findIndex((c) => c.key === key);
    if (i >= 0) care.splice(i, 1);
    return delay(null);
  },
  noteFiles: (id: string): Promise<NoteFiles> =>
    delay({
      files: files[id] ?? [],
      legacy: id === "notes/引っ越し手続きメモ" ? [{ name: "旧・間取り図.png", size: 245760 }] : [],
    }),
  fileAdd: (noteId: string, path: string): Promise<Added> => {
    const file: FileRow = {
      id: `demo-${Object.values(files).flat().length + 1}`,
      version: 1,
      name: path.split("/").pop() ?? "file",
      size: 34567,
      media_type: "image/png",
      availability: "local",
      sensitivity: "private",
      sync: "full",
      linked: false,
      client_repo: false,
      can_fetch: false,
      added_at: new Date().toISOString(),
    };
    files[noteId] = [...(files[noteId] ?? []), file];
    return delay({ file, warn_over_bytes: null, forced_local_only: false });
  },
  // ブラウザでは DOM の paste 経路が動くのでフォールバックは不要
  fileAddFromClipboard: () => delay<Added | null>(null),
  fileDetach: () => delay(null),
  fileFetch: () => delay<Availability>("local"),
  // ブラウザにネイティブの選択画面は無い(この経路は Tauri でしか通らない)
  pickFiles: () => delay<string[]>([]),
  connectDesktop: () => {
    connect.desktop = "connected";
    return delay(null);
  },
  embedEnable: () => {
    connect.smart_search = { state: "enabled", embedded: notes.length, total: notes.length };
    return delay(null);
  },
  backupSetRemote: (url: string) => {
    connect.backup.remote = url;
    return delay(null);
  },
  backupNow: () => {
    const n = connect.backup.pending;
    connect.backup.pending = 0;
    return delay(`バックアップ完了(${n} 件)`);
  },
};
