// コアへの取次。Tauri 内では invoke、素のブラウザ(開発プレビュー)ではデモデータで動く。
import { invoke } from "@tauri-apps/api/core";

export interface SetupState {
  needs_onboarding: boolean;
  vault_name: string | null;
  vault_path: string | null;
}
export interface Hit {
  id: string;
  title: string | null;
  status: string;
  snippet: string;
  via: string;
}
export interface Stats { total: number; drafts: number; deprecated: number }
export interface HomeState { stats: Stats; notes: Hit[]; drafts: Hit[]; degraded: string[] }
export interface SearchOutcome { hits: Hit[]; related: [string, string | null][]; degraded: string[] }
export interface NoteView {
  id: string;
  title: string;
  body: string;
  status: string;
  origin: string | null;
  tags: string[];
  generated_at: string | null;
  related: [string, string | null][];
}

const inTauri = "__TAURI_INTERNALS__" in window;

// ---- ブラウザプレビュー用のデモデータ(Tauri 外のみ) ----
const demoNotes: NoteView[] = [
  {
    id: "notes/引っ越し手続きメモ", title: "引っ越し手続きメモ", status: "stable", origin: "human",
    tags: [], generated_at: "2026-08-10T05:00:00Z",
    body: "3月末までにやること:\n\n- 電気・ガス・水道の解約(2週間前まで)\n- 転出届 → 転入届(14日以内)\n- 住所変更: 免許・銀行・[確定申告の準備](/notes/確定申告の準備.md)にも影響\n",
    related: [["notes/確定申告の準備", "確定申告の準備"]],
  },
  {
    id: "notes/確定申告の準備", title: "確定申告の準備", status: "stable", origin: "human",
    tags: [], generated_at: "2026-07-02T05:00:00Z", body: "medical 費の領収書を集める。\n", related: [],
  },
  {
    id: "notes/沖縄旅行の持ち物リスト", title: "沖縄旅行の持ち物リスト", status: "draft", origin: "agent",
    tags: ["旅行"], generated_at: "2026-08-10T06:00:00Z",
    body: "会話でまとめた持ち物:\n\n- 日焼け止め\n- モバイルバッテリー\n- 子どもの浮き輪\n", related: [],
  },
];
const demoHit = (n: NoteView): Hit => ({
  id: n.id, title: n.title, status: n.status, snippet: n.body.slice(0, 60).replace(/\n/g, " "), via: "recent",
});

async function demo<T>(v: T): Promise<T> {
  return new Promise((r) => setTimeout(() => r(v), 30));
}

// ---- API ----
export const api = {
  setupState(): Promise<SetupState> {
    if (!inTauri) {
      const onb = new URLSearchParams(location.search).get("screen") === "onboarding";
      return demo({ needs_onboarding: onb, vault_name: "わたしのノート", vault_path: "(demo)" });
    }
    return invoke("setup_state");
  },
  onboard(): Promise<SetupState> {
    if (!inTauri) return demo({ needs_onboarding: false, vault_name: "わたしのノート", vault_path: "(demo)" });
    return invoke("onboard");
  },
  homeState(): Promise<HomeState> {
    if (!inTauri) {
      const drafts = demoNotes.filter((n) => n.status === "draft").map(demoHit);
      return demo({
        stats: { total: demoNotes.length, drafts: drafts.length, deprecated: 0 },
        notes: demoNotes.map(demoHit), drafts, degraded: [],
      });
    }
    return invoke("home_state");
  },
  noteGet(id: string): Promise<NoteView> {
    if (!inTauri) return demo(demoNotes.find((n) => n.id === id) ?? demoNotes[0]);
    return invoke("note_get", { id });
  },
  noteSave(id: string, title: string, body: string): Promise<void> {
    if (!inTauri) {
      const n = demoNotes.find((x) => x.id === id);
      if (n) { n.title = title; n.body = body; }
      return demo(undefined);
    }
    return invoke("note_save", { id, title, body });
  },
  noteNew(title: string): Promise<string> {
    if (!inTauri) {
      const id = `notes/${title}`;
      demoNotes.unshift({ id, title, status: "stable", origin: "human", tags: [], generated_at: new Date().toISOString(), body: "", related: [] });
      return demo(id);
    }
    return invoke("note_new", { title });
  },
  noteSearch(query: string): Promise<SearchOutcome> {
    if (!inTauri) {
      const hits = demoNotes.filter((n) => (n.title + n.body).includes(query)).map((n) => ({ ...demoHit(n), via: "main" }));
      return demo({ hits, related: [], degraded: [] });
    }
    return invoke("note_search", { query });
  },
  draftConfirm(id: string): Promise<void> {
    if (!inTauri) {
      const n = demoNotes.find((x) => x.id === id);
      if (n) n.status = "stable";
      return demo(undefined);
    }
    return invoke("draft_confirm", { id });
  },
  draftReject(id: string): Promise<void> {
    if (!inTauri) {
      const n = demoNotes.find((x) => x.id === id);
      if (n) n.status = "deprecated";
      return demo(undefined);
    }
    return invoke("draft_reject", { id });
  },
};
