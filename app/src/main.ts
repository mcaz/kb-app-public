// kb-app 管理アプリ — AI の知識ベースを人間が統治する(2026-08-10 一本化ピボット)。
// ノートは AI 管理の1種類。人間は: 読む・検索する・承諾する・添付する・Claude に指示する。
// 下書きという特別な状態は持たない(2026-08-11)— 暫定の扱いはタグで、意味づけは会話で決まる。
import { convertFileSrc } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import {
  forceCenter, forceCollide, forceLink, forceManyBody, forceSimulation,
  type Simulation, type SimulationLinkDatum, type SimulationNodeDatum,
} from "d3-force";
import { marked } from "marked";
import { api, type ConnectState, type Favorite, type GraphData, type HomeState, type NoteView } from "./ipc";

const inTauri = "__TAURI_INTERNALS__" in window;

const app = document.getElementById("app")!;

type View = "home" | "notes" | "graph" | "connect";
const state = {
  view: "notes" as View,
  selectedTags: [] as string[],
  favorites: [] as Favorite[],
  graphCache: null as GraphData | null,
  listScroll: 0,
  noteScroll: null as { id: string; top: number } | null,
  localGraph: localStorage.getItem("kb.localGraph") !== "off",
  vaultName: "kb",
  home: null as HomeState | null,
  selected: null as NoteView | null,
  searching: false,
  query: "",
};

function el(html: string): HTMLElement {
  const t = document.createElement("template");
  t.innerHTML = html.trim();
  return t.content.firstElementChild as HTMLElement;
}
function esc(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);
}
function toast(msg: string) {
  const t = el(`<div class="toast">${esc(msg)}</div>`);
  document.body.appendChild(t);
  requestAnimationFrame(() => t.classList.add("show"));
  setTimeout(() => { t.classList.remove("show"); setTimeout(() => t.remove(), 300); }, 1800);
}
function fmtDate(iso: string | null): string {
  if (!iso) return "";
  const d = new Date(iso);
  const days = Math.floor((Date.now() - d.getTime()) / 86400000);
  if (days <= 0) return "きょう";
  if (days === 1) return "きのう";
  if (days < 7) return `${days}日前`;
  return `${d.getMonth() + 1}月${d.getDate()}日`;
}
function fmtSize(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)}MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)}KB`;
  return `${bytes}B`;
}
function fileToBase64(f: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve((r.result as string).split(",", 2)[1] ?? "");
    r.onerror = reject;
    r.readAsDataURL(f);
  });
}

async function addAttachmentFile(noteId: string, file: File, rename?: string): Promise<string | null> {
  if (file.size > 50 * 1024 * 1024) {
    toast("50MB を超えるファイルは添付できません");
    return null;
  }
  const b64 = await fileToBase64(file);
  const [saved, warning] = await api.attachmentAdd(noteId, rename ?? file.name, b64);
  if (warning) toast(`⚠ ${warning}`);
  return saved;
}

/// ペーストから画像添付(素材の持ち込み)。DOM 経路 → ダメなら Rust クリップボード読み。
async function pasteImage(e: ClipboardEvent, noteId: string): Promise<string | null> {
  const items = Array.from(e.clipboardData?.items ?? []);
  const img = items.find((i) => i.type.startsWith("image/"));
  if (img) {
    e.preventDefault();
    const file = img.getAsFile();
    if (!file) return null;
    const ext = img.type.split("/")[1] ?? "png";
    return addAttachmentFile(noteId, file, `pasted-${Date.now()}.${ext}`);
  }
  if (items.some((i) => i.kind === "string")) return null;
  const res = await api.attachmentPaste(noteId);
  if (!res) return null;
  if (res[1]) toast(`⚠ ${res[1]}`);
  return res[0];
}

// ノートを開いた状態での ⌘V / ドラッグ&ドロップ = 添付として持ち込む
document.addEventListener("paste", (e) => {
  const t = e.target as HTMLElement | null;
  if (t && (t.tagName === "TEXTAREA" || t.tagName === "INPUT")) return;
  const n = state.selected;
  if (!n || state.view !== "notes") return;
  void (async () => {
    try {
      const saved = await pasteImage(e as ClipboardEvent, n.id);
      if (!saved) return;
      state.selected = await api.noteGet(n.id);
      render();
      toast(`添付しました(${saved})`);
    } catch (err2) {
      toast(`ペースト添付に失敗: ${err2}`);
    }
  })();
});

if (inTauri) {
  void getCurrentWebview().onDragDropEvent(async (event) => {
    const kind = event.payload.type;
    if (kind === "over" || kind === "enter") {
      document.body.classList.add("dragover");
      return;
    }
    document.body.classList.remove("dragover");
    if (kind !== "drop") return;
    const n = state.selected;
    if (!n || state.view !== "notes") {
      toast("ノートを開いてからドロップしてください");
      return;
    }
    const paths: string[] = (event.payload as { paths: string[] }).paths ?? [];
    let added = 0;
    for (const p of paths) {
      try {
        const [saved, warning] = await api.attachmentAddFromPath(n.id, p);
        if (warning) toast(`⚠ ${warning}`);
        added++;
        void saved;
      } catch (e) {
        toast(`添付に失敗: ${e}`);
      }
    }
    if (!added) return;
    state.selected = await api.noteGet(n.id);
    render();
    toast(`添付しました(${added} 件)`);
  });
}

// ---- 起動 ----
async function boot() {
  const setup = await api.setupState();
  if (setup.needs_onboarding) return renderOnboarding();
  if (setup.vault_name) state.vaultName = setup.vault_name;
  await refreshHome();
  render();
}

async function refreshHome() {
  const [home, favs] = await Promise.all([api.homeState(), api.favoritesList()]);
  state.home = home;
  state.favorites = favs;
}

function renderOnboarding() {
  app.replaceChildren(el(`
    <div class="onb">
      <div class="mark">🌱</div>
      <h1>あなたの知識ベースをつくります</h1>
      <p>Claude と繋ぐと、会話から知識が育ち始めます。</p>
      <div><button class="primary" id="onb-go">はじめる</button></div>
    </div>
  `));
  document.getElementById("onb-go")!.addEventListener("click", async () => {
    const s = await api.onboard();
    if (s.vault_name) state.vaultName = s.vault_name;
    await refreshHome();
    render();
  });
}

// ---- 骨格 ----
function render() {
  const home = state.home!;
  const pendingCount = home.care.length;
  const degraded = home.degraded.length
    ? `<div class="degraded">⚠ ${home.degraded.map(esc).join(" / ")}</div>` : "";
  const shell = el(`
    <div class="shell">
      ${degraded}
      <div class="app">
        <nav class="side">
          <div class="nb">${esc(state.vaultName)}</div>
          <button class="nav ${state.view === "home" ? "on" : ""}" id="nav-home"><span>🏠 ホーム</span></button>
          <button class="nav ${state.view === "notes" ? "on" : ""}" id="nav-notes"><span>📄 ノート</span>${pendingCount ? `<span class="badge">${pendingCount}</span>` : ""}</button>
          <button class="nav ${state.view === "graph" ? "on" : ""}" id="nav-graph"><span>🕸️ グラフ</span></button>
          <button class="nav ${state.view === "connect" ? "on" : ""}" id="nav-connect"><span>🔗 繋ぐ</span></button>
          ${state.favorites.length ? `<div class="fav-head">★ お気に入り</div>` : ""}
          ${state.favorites
            .map((f) => {
              const on = state.view === "notes"
                && f.tags.length === state.selectedTags.length
                && f.tags.every((t) => state.selectedTags.includes(t));
              return `<div class="nav fav ${on ? "on" : ""}" data-fav="${esc(f.name)}" title="${esc(f.tags.join(" / "))}">
                <span>${esc(f.name)}</span><span class="fav-x" data-favx="${esc(f.name)}">×</span>
              </div>`;
            })
            .join("")}
        </nav>
        <div id="pane"></div>
      </div>
    </div>
  `);
  app.replaceChildren(shell);
  document.getElementById("nav-home")!.addEventListener("click", () => { state.view = "home"; render(); });
  document.getElementById("nav-notes")!.addEventListener("click", () => { state.view = "notes"; render(); });
  document.getElementById("nav-graph")!.addEventListener("click", () => { state.view = "graph"; render(); });
  document.getElementById("nav-connect")!.addEventListener("click", () => { state.view = "connect"; render(); });
  shell.querySelectorAll<HTMLElement>("[data-fav]").forEach((f) =>
    f.addEventListener("click", (e) => {
      if ((e.target as HTMLElement).dataset.favx) return; // × は別処理
      const fav = state.favorites.find((x) => x.name === f.dataset.fav);
      if (!fav) return;
      state.selectedTags = [...fav.tags];
      state.view = "notes";
      render();
    })
  );
  shell.querySelectorAll<HTMLElement>("[data-favx]").forEach((x) =>
    x.addEventListener("click", async (e) => {
      e.stopPropagation();
      await api.favoriteRemove(x.dataset.favx!);
      state.favorites = await api.favoritesList();
      render();
      toast("お気に入りを外しました");
    })
  );
  const pane = document.getElementById("pane")!;
  pane.style.display = "flex";
  pane.style.flex = "1";
  pane.style.minWidth = "0";
  if (state.view === "home") renderHome(pane);
  else if (state.view === "notes") renderNotes(pane);
  else if (state.view === "graph") renderGraph(pane);
  else renderConnect(pane);
}

// ---- ノート一覧+本文(唯一のハブ)----
function careIds(): Set<string> {
  const s = new Set<string>();
  for (const c of state.home!.care) {
    s.add(c.a);
    s.add(c.b);
  }
  return s;
}

function matchFilter(h: { tags: string[] }): boolean {
  return state.selectedTags.every((t) => h.tags.includes(t));
}

function renderNotes(pane: HTMLElement) {
  const home = state.home!;
  const list = el(`
    <div class="list">
      <div class="searchbox"><input id="search" placeholder="🔍 ノートを検索" value="${esc(state.query)}" /></div>
      <div class="tag-select" id="tag-select"></div>
      <div class="items" id="items"></div>
    </div>
  `);
  buildTagSelect(list.querySelector<HTMLElement>("#tag-select")!, home.tags.map(([t]) => t));
  list.style.width = `${Number(localStorage.getItem("kb.listWidth")) || 260}px`;
  const splitter = el(`<div class="splitter"></div>`);
  splitter.addEventListener("mousedown", (e) => {
    e.preventDefault();
    splitter.classList.add("active");
    const startX = (e as MouseEvent).clientX;
    const startW = list.getBoundingClientRect().width;
    const move = (ev: MouseEvent) => {
      list.style.width = `${Math.min(520, Math.max(180, startW + ev.clientX - startX))}px`;
    };
    const up = () => {
      splitter.classList.remove("active");
      localStorage.setItem("kb.listWidth", String(Math.round(list.getBoundingClientRect().width)));
      document.removeEventListener("mousemove", move);
      document.removeEventListener("mouseup", up);
    };
    document.addEventListener("mousemove", move);
    document.addEventListener("mouseup", up);
  });
  pane.replaceChildren(list, splitter, el(`<div class="editor" id="editor"></div>`));

  const itemsBox = list.querySelector<HTMLElement>("#items")!;
  // 再描画で DOM を作り直すため、位置を明示的に持ち越す(選択でリストが先頭に戻らないように)
  itemsBox.addEventListener("scroll", () => { state.listScroll = itemsBox.scrollTop; });
  const showItems = (
    allHits: { id: string; title: string | null; status: string; snippet: string; tags: string[] }[],
    searchMode: boolean
  ) => {
    const hits = allHits.filter((h) => matchFilter(h));
    itemsBox.replaceChildren();
    if (!hits.length) {
      itemsBox.appendChild(el(`<div class="empty">${searchMode ? "見つかりませんでした" : "該当するノートがありません"}</div>`));
    }
    const care = careIds();
    for (const h of hits) {
      const on = state.selected?.id === h.id ? "on" : "";
      const marks = care.has(h.id) ? " 🔧" : "";
      const item = el(`
        <div class="item ${on}">
          <div class="t">${esc(h.title ?? h.id)}${marks}</div>
          <div class="d">${esc(h.snippet.slice(0, 80))}</div>
        </div>
      `);
      item.addEventListener("click", () => void openNote(h.id));
      itemsBox.appendChild(item);
    }
    itemsBox.scrollTop = state.listScroll; // 溢れなければブラウザが 0 に丸める
  };

  if (!state.searching) showItems(home.notes, false);

  const search = list.querySelector<HTMLInputElement>("#search")!;
  let timer: number | undefined;
  search.addEventListener("input", () => {
    state.query = search.value;
    window.clearTimeout(timer);
    timer = window.setTimeout(async () => {
      if (!state.query.trim()) {
        state.searching = false;
        showItems(state.home!.notes, false);
        return;
      }
      state.searching = true;
      const out = await api.noteSearch(state.query.trim());
      showItems(out.hits, true);
      if (out.degraded.length) toast(`⚠ ${out.degraded[0]}`);
    }, 250);
  });
  if (state.searching && state.query.trim()) {
    void api.noteSearch(state.query.trim()).then((out) => showItems(out.hits, true));
  }

  renderNoteView(document.getElementById("editor")!);
}

/// タグのオートコンプリート複数選択(検索フィールドの下)。
/// 選択タグは AND で絞り込み。Enter=先頭候補、空欄で Backspace=末尾を外す。
function buildTagSelect(box: HTMLElement, allTags: string[]) {
  const selectedChips = state.selectedTags
    .map((t) => `<span class="tsel-chip">${esc(t)}<button class="chip-x" data-tag="${esc(t)}">×</button></span>`)
    .join("");
  const saveBtn = state.selectedTags.length ? `<button class="fav-save" id="fav-save" title="この組み合わせをお気に入りに保存">★</button>` : "";
  box.innerHTML = `${selectedChips}<span class="tsel-wrap"><input id="tag-input" placeholder="${state.selectedTags.length ? "" : "タグで絞り込み"}" autocomplete="off" /><div class="tag-dd" id="tag-dd" hidden></div></span>${saveBtn}`;
  box.querySelector("#fav-save")?.addEventListener("click", () => void saveFavorite());
  const input = box.querySelector<HTMLInputElement>("#tag-input")!;
  const dd = box.querySelector<HTMLElement>("#tag-dd")!;

  const addTag = (t: string) => {
    if (!state.selectedTags.includes(t)) state.selectedTags.push(t);
    render();
  };
  box.querySelectorAll<HTMLButtonElement>(".chip-x").forEach((b) =>
    b.addEventListener("click", () => {
      state.selectedTags = state.selectedTags.filter((t) => t !== b.dataset.tag);
      render();
    })
  );

  let candidates: string[] = [];
  const refreshDd = () => {
    const q = input.value.trim().toLowerCase();
    candidates = allTags
      .filter((t) => !state.selectedTags.includes(t))
      .filter((t) => !q || t.toLowerCase().includes(q))
      .slice(0, 8);
    if (!candidates.length || document.activeElement !== input) {
      dd.hidden = true;
      return;
    }
    dd.innerHTML = candidates.map((t) => `<button class="tag-dd-item" data-tag="${esc(t)}">${esc(t)}</button>`).join("");
    dd.hidden = false;
    dd.querySelectorAll<HTMLButtonElement>(".tag-dd-item").forEach((b) =>
      // mousedown: input の blur より先に発火させる
      b.addEventListener("mousedown", (e) => { e.preventDefault(); addTag(b.dataset.tag!); })
    );
  };
  input.addEventListener("input", refreshDd);
  input.addEventListener("focus", refreshDd);
  input.addEventListener("blur", () => setTimeout(() => { dd.hidden = true; }, 150));
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && candidates.length) {
      e.preventDefault();
      addTag(candidates[0]);
    } else if (e.key === "Backspace" && !input.value && state.selectedTags.length) {
      state.selectedTags.pop();
      render();
    } else if (e.key === "Escape") {
      dd.hidden = true;
    }
  });
}

/// 選択中のタグをお気に入りとして保存(名前はアプリ内モーダルで入力)。
function saveFavorite(): Promise<void> {
  return new Promise((resolve) => {
    const tags = [...state.selectedTags];
    const overlay = el(`
      <div class="modal-overlay">
        <div class="modal">
          <div class="modal-title">お気に入りに保存</div>
          <div class="modal-desc">${tags.map((t) => esc(t)).join(" / ")}</div>
          <input id="fav-name" placeholder="名前(例: 開発まわり)" value="${esc(tags.join("・"))}" />
          <div class="modal-row">
            <button class="primary" id="fav-ok">保存</button>
            <button class="quiet" id="fav-cancel">やめる</button>
          </div>
        </div>
      </div>
    `);
    const input = overlay.querySelector<HTMLInputElement>("#fav-name")!;
    const done = () => { overlay.remove(); resolve(); };
    const commit = async () => {
      const name = input.value.trim();
      if (!name) { toast("名前を入れてください"); return; }
      try {
        await api.favoriteAdd(name, tags);
        state.favorites = await api.favoritesList();
        done();
        render();
        toast("お気に入りに保存しました");
      } catch (e) {
        done();
        toast(`${e}`);
      }
    };
    overlay.querySelector("#fav-ok")!.addEventListener("click", () => void commit());
    overlay.querySelector("#fav-cancel")!.addEventListener("click", done);
    overlay.addEventListener("click", (e) => { if (e.target === overlay) done(); });
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") void commit();
      if (e.key === "Escape") done();
    });
    document.body.appendChild(overlay);
    input.focus();
    input.select();
  });
}

function renderNoteView(box: HTMLElement) {
  const n = state.selected;
  if (!n) {
    box.replaceChildren(el(`<div class="placeholder">左の一覧からノートを選ぶと、ここに表示されます。ノートは Claude との会話から育ちます。</div>`));
    return;
  }
  const statusPill =
    n.status === "deprecated" ? `<span class="status-pill deprecated">しまってある</span>` : "";
  const tagChips = n.tags.map((t) => {
    return `<button class="tag-chip" data-tag="${esc(t)}">${esc(t)}</button>`;
  }).join("");
  const attachChips = n.attachments
    .map(
      ([name, size]) =>
        `<span class="chip">📎 ${esc(name)} <i>${fmtSize(size)}</i><button class="chip-x" data-name="${esc(name)}">×</button></span>`
    )
    .join("");
  // このノートへのお手入れ提案
  const myCare = state.home!.care.filter((c) => c.a === n.id || c.b === n.id);
  const careBars = myCare
    .map(
      (c, i) => `<div class="approve-bar care">🔧 ${esc(c.detail)}
        ${c.kind === "connect" ? `<button class="primary small" data-care-ok="${i}">つなげる</button>` : ""}
        <button class="quiet small" data-care-no="${i}">${c.kind === "connect" ? "このまま" : "確認した"}</button>
      </div>`
    )
    .join("");
  const relatedSection = n.related.length
    ? `<div class="related-section"><div class="head">🔗 つながり</div><ul>${n.related
        .map(
          ([id, t]) =>
            `<li><button class="rel" data-id="${esc(id)}">${esc(t ?? id)}<span class="rid">${esc(id)}</span></button></li>`
        )
        .join("")}</ul></div>`
    : "";
  box.replaceChildren(el(`
    <div class="note-split">
     <div class="note-main">
      <div class="title-row">
        <div class="title">${esc(n.title)}</div>
        <button class="quiet small" id="lg-toggle" title="つながりのグラフ">${state.localGraph ? "🕸️ 隠す" : "🕸️ 表示"}</button>
      </div>
      <div class="meta">${fmtDate(n.generated_at)} ${statusPill} ${tagChips}</div>
      ${careBars}
      <div style="margin: 2px 0 12px;"><button class="small" id="talk">🤖 このノートについて Claude と話す</button></div>
      <div class="attach">${n.attachments.length ? `<span class="attach-label">添付:</span>` : ""}${attachChips}<button class="quiet small" id="attach-add">＋ ファイルを添付</button><input type="file" id="attach-file" multiple hidden /></div>
      <div class="preview">${marked.parse(n.body) as string}</div>
      ${relatedSection}
     </div>
     ${state.localGraph ? `<div class="local-graph" id="local-graph"><div class="lg-head">🕸️ つながり</div><div class="lg-body" id="lg-body"><div class="lg-empty">読み込み中…</div></div></div>` : ""}
    </div>
  `));
  box.querySelector("#lg-toggle")!.addEventListener("click", () => {
    state.localGraph = !state.localGraph;
    localStorage.setItem("kb.localGraph", state.localGraph ? "on" : "off");
    render();
  });
  const lgBody = box.querySelector<HTMLElement>("#lg-body");
  if (lgBody) void renderLocalGraph(lgBody, n.id);

  if (state.noteScroll?.id === n.id) box.scrollTop = state.noteScroll.top;
  box.onscroll = () => { state.noteScroll = { id: n.id, top: box.scrollTop }; };

  box.querySelectorAll<HTMLButtonElement>("[data-care-ok]").forEach((b) =>
    b.addEventListener("click", async () => {
      const c = myCare[Number(b.dataset.careOk)];
      try {
        await api.careAccept(c.key);
        state.graphCache = null;
        await refreshHome();
        state.selected = await api.noteGet(n.id);
        render();
        toast("つなげました");
      } catch (e) {
        toast(`${e}`);
      }
    })
  );
  box.querySelectorAll<HTMLButtonElement>("[data-care-no]").forEach((b) =>
    b.addEventListener("click", async () => {
      const c = myCare[Number(b.dataset.careNo)];
      await api.careDismiss(c.key);
      await refreshHome();
      render();
      toast("了解、そのままにします");
    })
  );
  box.querySelectorAll<HTMLButtonElement>(".tag-chip").forEach((b) =>
    b.addEventListener("click", () => {
      if (!state.selectedTags.includes(b.dataset.tag!)) state.selectedTags.push(b.dataset.tag!);
      render();
    })
  );
  const fileInput = box.querySelector<HTMLInputElement>("#attach-file")!;
  box.querySelector("#attach-add")!.addEventListener("click", () => fileInput.click());
  fileInput.addEventListener("change", async () => {
    for (const f of Array.from(fileInput.files ?? [])) {
      await addAttachmentFile(n.id, f);
    }
    state.selected = await api.noteGet(n.id);
    render();
    toast("添付しました");
  });
  box.querySelectorAll<HTMLButtonElement>(".chip-x").forEach((b) =>
    b.addEventListener("click", async () => {
      await api.attachmentRemove(n.id, b.dataset.name!);
      state.selected = await api.noteGet(n.id);
      render();
      toast("添付を削除しました(履歴には残ります)");
    })
  );
  document.getElementById("talk")!.addEventListener("click", async () => {
    try {
      await api.launchAi(n.id);
      toast("Claude を開きました");
    } catch (e) {
      toast(`${e}`);
    }
  });
  hydratePreview(box.querySelector<HTMLElement>(".preview")!, n);
  box.querySelectorAll<HTMLElement>(".rel").forEach((a) =>
    a.addEventListener("click", () => void openNote(a.dataset.id!)));
}

/// プレビュー後処理: vault 内画像を asset プロトコルで表示し、ノートリンクを遷移可能に。
function hydratePreview(root: HTMLElement, n: NoteView) {
  root.querySelectorAll<HTMLImageElement>("img").forEach((img) => {
    const src = decodeURIComponent(img.getAttribute("src") ?? "");
    if (src.startsWith("/") && inTauri) {
      img.src = convertFileSrc(`${n.vault_root}${src}`);
    }
  });
  root.querySelectorAll<HTMLAnchorElement>("a").forEach((a) => {
    a.addEventListener("click", (e) => {
      e.preventDefault();
      const href = decodeURIComponent(a.getAttribute("href") ?? "");
      if (href.endsWith(".md")) void openNote(href.replace(/^\//, "").replace(/\.md$/, ""));
    });
  });
}

async function openNote(id: string) {
  try {
    state.selected = await api.noteGet(id);
    state.view = "notes";
    render();
  } catch {
    toast("そのノートはまだありません");
  }
}

// ---- ホーム ----
function renderHome(pane: HTMLElement) {
  const home = state.home!;
  const s = home.stats;
  const box = el(`<div class="dash"></div>`);
  pane.replaceChildren(box);
  const warnings = [...home.degraded];
  const tiles = el(`
    <div>
      <div class="dash-warnings"></div>
      <div class="dash-tiles">
        <button class="tile" data-go="all"><div class="num">${s.total - s.deprecated}</div><div class="lbl">📄 ノート</div></button>
        <div class="tile ${home.care.length ? "amber" : ""}"><div class="num">${home.care.length}</div><div class="lbl">🔧 提案</div></div>
        <div class="tile"><div class="num">${s.links}</div><div class="lbl">🔗 つながり</div></div>
        <div class="tile"><div class="num">${s.embed_enabled ? `${s.embedded}/${s.total}` : "オフ"}</div><div class="lbl">✨ かしこい検索</div></div>
        <div class="tile" id="tile-sync"><div class="num">…</div><div class="lbl">☁️ バックアップ</div></div>
      </div>
      <div class="dash-head">最近のノート</div>
      <div class="dash-recent"></div>
    </div>
  `);
  box.appendChild(tiles);

  const warnBox = tiles.querySelector<HTMLElement>(".dash-warnings")!;
  const renderWarnings = () => {
    warnBox.replaceChildren(...warnings.map((w) => el(`<div class="dash-warn">⚠ ${esc(w)}</div>`)));
  };
  renderWarnings();

  tiles.querySelectorAll<HTMLButtonElement>("[data-go]").forEach((t) =>
    t.addEventListener("click", () => {
      state.view = "notes";
      state.selectedTags = [];
      render();
    })
  );

  const recentBox = tiles.querySelector<HTMLElement>(".dash-recent")!;
  for (const h of home.notes.slice(0, 6)) {
    const row = el(`
      <button class="dash-note">
        <span class="t">${esc(h.title ?? h.id)}</span>
        <span class="d">${esc(h.snippet.slice(0, 60))}</span>
      </button>
    `);
    row.addEventListener("click", () => void openNote(h.id));
    recentBox.appendChild(row);
  }

  void api.connectState().then((c) => {
    const tile = tiles.querySelector<HTMLElement>("#tile-sync")!;
    const num = tile.querySelector<HTMLElement>(".num")!;
    if (!c.backup.remote) num.textContent = "未設定";
    else if (c.backup.pending > 0) num.textContent = `残 ${c.backup.pending}`;
    else num.textContent = "✓";
    if (c.sync_error) {
      warnings.push(`同期エラー: ${c.sync_error}`);
      renderWarnings();
      tile.classList.add("amber");
    }
  });
}

// ---- つながりグラフ(緑=確定ノート / 琥珀=下書き)----
type GNode = SimulationNodeDatum & GraphData["nodes"][number];
let graphSim: Simulation<GNode, SimulationLinkDatum<GNode>> | null = null;

function renderGraph(pane: HTMLElement) {
  graphSim?.stop();
  const wrap = el(`
    <div class="graph-wrap">
      <canvas></canvas>
      <div class="graph-legend">
        <span class="hint">クリックで開く / ドラッグで動かす / ホイールで拡大</span>
      </div>
    </div>
  `);
  pane.replaceChildren(wrap);
  const canvas = wrap.querySelector("canvas")!;
  void api.graphData().then((data) => startGraph(wrap, canvas, data));
}

function startGraph(wrap: HTMLElement, canvas: HTMLCanvasElement, data: GraphData, centerId?: string) {
  const css = getComputedStyle(document.documentElement);
  const colNode = css.getPropertyValue("--grow").trim() || "#3E7550";
  const colCenter = css.getPropertyValue("--prop").trim() || "#A97B2F";
  const colLine = css.getPropertyValue("--line").trim() || "#888";
  const colInk = css.getPropertyValue("--ink").trim() || "#222";
  const ctx = canvas.getContext("2d")!;
  const dpr = window.devicePixelRatio || 1;

  const nodes: GNode[] = data.nodes.map((n) => ({ ...n }));
  const links = data.edges.map(([source, target]) => ({ source, target })) as SimulationLinkDatum<GNode>[];
  const t = { x: 0, y: 0, k: 1 };
  let hovered: GNode | null = null;

  const size = () => {
    canvas.width = wrap.clientWidth * dpr;
    canvas.height = wrap.clientHeight * dpr;
  };
  size();
  new ResizeObserver(() => { size(); draw(); }).observe(wrap);

  const radius = (n: GNode) =>
    n.id === centerId ? 7 : 3.5 + Math.min(6, Math.sqrt(n.degree) * 1.6);

  const sim = forceSimulation(nodes)
    .force("link", forceLink<GNode, SimulationLinkDatum<GNode>>(links).id((d) => d.id).distance(70).strength(0.5))
    .force("charge", forceManyBody().strength(-130))
    .force("center", forceCenter(wrap.clientWidth / 2, wrap.clientHeight / 2))
    .force("collide", forceCollide<GNode>().radius((d) => radius(d) + 3))
    .on("tick", () => draw());
  graphSim = sim as Simulation<GNode, SimulationLinkDatum<GNode>>;

  function draw() {
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, canvas.width / dpr, canvas.height / dpr);
    ctx.translate(t.x, t.y);
    ctx.scale(t.k, t.k);
    ctx.strokeStyle = colLine;
    ctx.lineWidth = 1 / t.k;
    ctx.globalAlpha = 0.35;
    for (const l of links) {
      const s = l.source as GNode;
      const d = l.target as GNode;
      if (s.x == null || d.x == null) continue;
      ctx.beginPath();
      ctx.moveTo(s.x!, s.y!);
      ctx.lineTo(d.x!, d.y!);
      ctx.stroke();
    }
    ctx.globalAlpha = 1;
    for (const n of nodes) {
      if (n.x == null) continue;
      ctx.beginPath();
      ctx.arc(n.x!, n.y!, radius(n), 0, Math.PI * 2);
      ctx.fillStyle = n.id === centerId ? colCenter : colNode;
      ctx.fill();
      if (n === hovered) {
        ctx.strokeStyle = colInk;
        ctx.lineWidth = 2 / t.k;
        ctx.stroke();
      }
      if (centerId || t.k > 1.05 || n === hovered) {
        ctx.fillStyle = colInk;
        ctx.font = `${10.5 / t.k}px sans-serif`;
        const label = n.title.length > 16 ? `${n.title.slice(0, 16)}…` : n.title;
        ctx.fillText(label, n.x! + radius(n) + 3 / t.k, n.y! + 4 / t.k);
      }
    }
  }

  const toGraph = (mx: number, my: number) => ({ x: (mx - t.x) / t.k, y: (my - t.y) / t.k });
  const hit = (mx: number, my: number): GNode | null => {
    const p = toGraph(mx, my);
    for (const n of nodes) {
      if (n.x == null) continue;
      const dx = p.x - n.x!;
      const dy = p.y - n.y!;
      if (dx * dx + dy * dy <= (radius(n) + 3) ** 2) return n;
    }
    return null;
  };

  canvas.addEventListener("wheel", (e) => {
    e.preventDefault();
    const rect = canvas.getBoundingClientRect();
    const mx = e.clientX - rect.left;
    const my = e.clientY - rect.top;
    const k2 = Math.min(4, Math.max(0.2, t.k * (e.deltaY < 0 ? 1.12 : 0.89)));
    t.x = mx - ((mx - t.x) / t.k) * k2;
    t.y = my - ((my - t.y) / t.k) * k2;
    t.k = k2;
    draw();
  }, { passive: false });

  canvas.addEventListener("mousedown", (e) => {
    const rect = canvas.getBoundingClientRect();
    const mx = e.clientX - rect.left;
    const my = e.clientY - rect.top;
    const node = hit(mx, my);
    const start = { mx, my, moved: false };
    const move = (ev: MouseEvent) => {
      const cx = ev.clientX - rect.left;
      const cy = ev.clientY - rect.top;
      if (Math.abs(cx - start.mx) + Math.abs(cy - start.my) > 4) start.moved = true;
      if (node) {
        const p = toGraph(cx, cy);
        node.fx = p.x;
        node.fy = p.y;
        sim.alphaTarget(0.25).restart();
      } else {
        t.x += cx - start.mx;
        t.y += cy - start.my;
        start.mx = cx;
        start.my = cy;
        draw();
      }
    };
    const up = () => {
      document.removeEventListener("mousemove", move);
      document.removeEventListener("mouseup", up);
      if (node) {
        node.fx = null;
        node.fy = null;
        sim.alphaTarget(0);
        if (!start.moved) void openNote(node.id);
      }
    };
    document.addEventListener("mousemove", move);
    document.addEventListener("mouseup", up);
  });

  canvas.addEventListener("mousemove", (e) => {
    const rect = canvas.getBoundingClientRect();
    const h = hit(e.clientX - rect.left, e.clientY - rect.top);
    if (h !== hovered) {
      hovered = h;
      canvas.style.cursor = h ? "pointer" : "default";
      draw();
    }
  });
}

/// 中心ノートから hops ホップ以内の部分グラフを取り出す(ローカルグラフ用)。
function subgraph(data: GraphData, centerId: string, hops = 2): GraphData {
  const adj = new Map<string, Set<string>>();
  for (const [a, b] of data.edges) {
    if (!adj.has(a)) adj.set(a, new Set());
    if (!adj.has(b)) adj.set(b, new Set());
    adj.get(a)!.add(b);
    adj.get(b)!.add(a);
  }
  const keep = new Set<string>([centerId]);
  let frontier = [centerId];
  for (let h = 0; h < hops; h++) {
    const next: string[] = [];
    for (const id of frontier) {
      for (const nb of adj.get(id) ?? []) {
        if (!keep.has(nb)) {
          keep.add(nb);
          next.push(nb);
        }
      }
    }
    frontier = next;
    if (!frontier.length) break;
  }
  return {
    nodes: data.nodes.filter((n) => keep.has(n.id)),
    edges: data.edges.filter(([a, b]) => keep.has(a) && keep.has(b)),
  };
}

/// ノート閲覧ビュー横のローカルグラフ(選択ノートを中心にした近傍)。
async function renderLocalGraph(box: HTMLElement, centerId: string) {
  if (!state.graphCache) {
    try {
      state.graphCache = await api.graphData();
    } catch {
      box.replaceChildren(el(`<div class="lg-empty">グラフを読み込めませんでした</div>`));
      return;
    }
  }
  const sub = subgraph(state.graphCache, centerId, 2);
  if (sub.nodes.length <= 1) {
    box.replaceChildren(el(`<div class="lg-empty">つながりはまだありません</div>`));
    return;
  }
  const wrap = el(`<div class="lg-canvas"><canvas></canvas></div>`);
  box.replaceChildren(wrap);
  requestAnimationFrame(() => startGraph(wrap, wrap.querySelector("canvas")!, sub, centerId));
}

// ---- 繋ぐ ----
function renderConnect(pane: HTMLElement) {
  const box = el(`<div class="connect"><div class="loading">確認中…</div></div>`);
  pane.replaceChildren(box);
  void api.connectState().then((c: ConnectState) => {
    box.replaceChildren(
      connectCardAi(c),
      connectCardSearch(c),
      connectCardBackup(c),
    );
  });
}

function connectCardAi(c: ConnectState): HTMLElement {
  const state_ =
    c.desktop === "connected" ? `<span class="state ok">接続済み</span>`
    : c.desktop === "not_found" ? `<span class="state off">Claude Desktop が見つかりません</span>`
    : `<span class="state off">未接続</span>`;
  const card = el(`
    <div class="con-card">
      <div class="name">🤖 AI アプリ(Claude)</div>
      <div class="desc">会話の中からあなたの知識ベースが引かれ、会話で得た知見が下書きとして届くようになります。</div>
      ${state_}
      <div class="row">${c.desktop === "not_connected" ? `<button class="primary small" id="con-ai">接続する</button>` : ""}</div>
    </div>
  `);
  card.querySelector("#con-ai")?.addEventListener("click", async () => {
    try {
      await api.connectDesktop();
      toast("接続しました。Claude Desktop を再起動すると使えます");
      render();
    } catch (e) {
      toast(`接続できませんでした: ${e}`);
    }
  });
  return card;
}

function connectCardSearch(c: ConnectState): HTMLElement {
  const s = c.smart_search;
  const state_ =
    s.state === "enabled"
      ? `<span class="state ok">有効(${s.embedded}/${s.total} 件)</span>`
      : s.state === "downloading"
        ? `<span class="state off">ダウンロード中…</span>`
        : `<span class="state off">オフ</span>`;
  const card = el(`
    <div class="con-card">
      <div class="name">✨ かしこい検索</div>
      <div class="desc">言い回しが違っても意味で見つかる検索。オンにするだけで、外部送信はありません(初回のみ検索用データ 約560MB を取得)。</div>
      ${state_}
      <div class="row">${s.state === "not_installed" ? `<button class="primary small" id="con-emb">オンにする</button>` : ""}</div>
    </div>
  `);
  card.querySelector("#con-emb")?.addEventListener("click", async () => {
    toast("かしこい検索を準備中…(数分かかります)");
    try {
      await api.embedEnable();
      toast("かしこい検索が有効になりました");
      render();
    } catch (e) {
      toast(`${e}`);
    }
  });
  return card;
}

function connectCardBackup(c: ConnectState): HTMLElement {
  const has = !!c.backup.remote;
  const state_ = has
    ? `<span class="state ok">接続済み</span>`
    : `<span class="state off">未設定</span>`;
  const pending = has && c.backup.pending > 0
    ? `<div class="desc">まだ送れていない変更が ${c.backup.pending} 件あります。</div>` : "";
  const error = c.sync_error
    ? `<div class="desc" style="color: var(--danger)">⚠ ${esc(c.sync_error)}</div>` : "";
  const setup = has ? "" : `
    <input id="con-bk-url" placeholder="GitHub リポジトリの URL(git@github.com:…)" style="width:100%;font-size:12px;margin-bottom:8px" />`;
  const card = el(`
    <div class="con-card">
      <div class="name">☁️ バックアップ</div>
      <div class="desc">ノートは変わるたびに自動で控えられ、他の端末で書いた分も取り込まれます。非公開のまま、対象はノートだけです。</div>
      ${state_}
      ${pending}
      ${error}
      ${setup}
      <div class="row">${has
        ? `<button class="primary small" id="con-bk">今すぐ同期</button>`
        : `<button class="primary small" id="con-bk-set">接続する</button>`}</div>
    </div>
  `);
  card.querySelector("#con-bk")?.addEventListener("click", async () => {
    try {
      toast(await api.backupNow());
      render();
    } catch (e) {
      toast(`${e}`);
    }
  });
  card.querySelector("#con-bk-set")?.addEventListener("click", async () => {
    const url = (card.querySelector<HTMLInputElement>("#con-bk-url")!).value.trim();
    if (!url) { toast("リポジトリの URL を入れてください"); return; }
    try {
      await api.backupSetRemote(url);
      toast("バックアップ先を設定し、初回の控えを送りました");
      render();
    } catch (e) {
      toast(`${e}`);
    }
  });
  return card;
}

void boot();
