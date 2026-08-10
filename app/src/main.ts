// kb-app 管理アプリ(v0.2 骨格)。画面はモック(docs/ui-draft.html)の A/B/C に対応。
// ユーザーに見せる概念は「ノート・下書き・つながり・バックアップ」まで(原則7)。
import { marked } from "marked";
import { api, type HomeState, type NoteView } from "./ipc";

const app = document.getElementById("app")!;

type View = "notes" | "inbox";
const state = {
  view: "notes" as View,
  vaultName: "わたしのノート",
  home: null as HomeState | null,
  selected: null as NoteView | null,
  editing: false,
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
  const today = new Date();
  const days = Math.floor((today.getTime() - d.getTime()) / 86400000);
  if (days <= 0) return "きょう";
  if (days === 1) return "きのう";
  if (days < 7) return `${days}日前`;
  return `${d.getMonth() + 1}月${d.getDate()}日`;
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
  state.home = await api.homeState();
}

// ---- 画面A: オンボーディング ----
function renderOnboarding() {
  app.replaceChildren(el(`
    <div class="onb">
      <div class="mark">🌱</div>
      <h1>はじめまして。あなたのノートブックをつくります</h1>
      <p>もう書き始められます。AI やバックアップは、使いたくなったときに繋げます。</p>
      <div><button class="primary" id="onb-go">書いてみる</button></div>
    </div>
  `));
  document.getElementById("onb-go")!.addEventListener("click", async () => {
    const s = await api.onboard();
    if (s.vault_name) state.vaultName = s.vault_name;
    await refreshHome();
    render();
  });
}

// ---- 骨格(サイド+リスト+右ペイン) ----
function render() {
  const home = state.home!;
  const degraded = home.degraded.length
    ? `<div class="degraded">⚠ ${home.degraded.map(esc).join(" / ")}</div>` : "";
  const shell = el(`
    <div class="shell">
      ${degraded}
      <div class="app">
        <nav class="side">
          <div class="nb">${esc(state.vaultName)}</div>
          <button class="nav ${state.view === "notes" ? "on" : ""}" id="nav-notes"><span>📄 ノート</span></button>
          <button class="nav ${state.view === "inbox" ? "on" : ""}" id="nav-inbox"><span>📥 受信箱</span>${home.drafts.length ? `<span class="badge">${home.drafts.length}</span>` : ""}</button>
          <button class="nav grow-btn" id="nav-new"><span>＋ 新しいノート</span></button>
        </nav>
        <div id="pane"></div>
      </div>
    </div>
  `);
  app.replaceChildren(shell);
  document.getElementById("nav-notes")!.addEventListener("click", () => { state.view = "notes"; render(); });
  document.getElementById("nav-inbox")!.addEventListener("click", () => { state.view = "inbox"; render(); });
  document.getElementById("nav-new")!.addEventListener("click", newNote);
  const pane = document.getElementById("pane")!;
  pane.style.display = "flex";
  pane.style.flex = "1";
  pane.style.minWidth = "0";
  if (state.view === "notes") renderNotes(pane);
  else renderInbox(pane);
}

async function newNote() {
  const title = prompt("ノートのタイトル");
  if (!title?.trim()) return;
  const id = await api.noteNew(title.trim());
  await refreshHome();
  state.view = "notes";
  state.selected = await api.noteGet(id);
  state.editing = true;
  render();
}

// ---- 画面B: ノート一覧+本文 ----
function renderNotes(pane: HTMLElement) {
  const home = state.home!;
  const items = state.searching ? null : home.notes;
  const list = el(`
    <div class="list">
      <div class="searchbox"><input id="search" placeholder="🔍 ノートを検索" value="${esc(state.query)}" /></div>
      <div class="items" id="items"></div>
    </div>
  `);
  pane.replaceChildren(list, el(`<div class="editor" id="editor"></div>`));

  const itemsBox = list.querySelector<HTMLElement>("#items")!;
  const showItems = (hits: { id: string; title: string | null; status: string; snippet: string; via: string }[], searchMode: boolean) => {
    itemsBox.replaceChildren();
    if (!hits.length) itemsBox.appendChild(el(`<div class="empty">${searchMode ? "見つかりませんでした" : "まだノートがありません。「＋ 新しいノート」から"}</div>`));
    for (const h of hits) {
      const on = state.selected?.id === h.id ? "on" : "";
      const pill = h.status === "draft" ? " ✎下書き" : "";
      const via = searchMode && h.via === "rescue" ? `<span class="via">·部分一致</span>` : "";
      const item = el(`
        <div class="item ${on}">
          <div class="t">${esc(h.title ?? h.id)}${pill}</div>
          <div class="d">${searchMode ? esc(h.snippet) : fmtDate((h as { generated_at?: string }).generated_at ?? null) || esc(h.snippet)} ${via}</div>
        </div>
      `);
      item.addEventListener("click", async () => {
        state.selected = await api.noteGet(h.id);
        state.editing = false;
        render();
      });
      itemsBox.appendChild(item);
    }
  };

  if (items) showItems(items.map((h) => ({ ...h })), false);

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

  renderEditor(document.getElementById("editor")!);
}

function renderEditor(box: HTMLElement) {
  const n = state.selected;
  if (!n) {
    box.replaceChildren(el(`<div class="placeholder">左の一覧からノートを選ぶか、「＋ 新しいノート」で書き始められます。</div>`));
    return;
  }
  const statusPill =
    n.status === "draft" ? `<span class="status-pill draft">下書き</span>`
    : n.status === "deprecated" ? `<span class="status-pill deprecated">しまってある</span>` : "";
  const related = n.related.length
    ? `つながり: ` + n.related.map(([id, t]) => `<span class="link" data-id="${esc(id)}">${esc(t ?? id)}</span>`).join("、")
    : "";
  if (!state.editing) {
    box.replaceChildren(el(`
      <div>
        <div class="title-row">
          <div class="title">${esc(n.title)}</div>
          <button class="small" id="edit">編集</button>
        </div>
        <div class="meta">${fmtDate(n.generated_at)} ${statusPill} ${related}</div>
        <div class="preview">${marked.parse(n.body) as string}</div>
      </div>
    `));
    document.getElementById("edit")!.addEventListener("click", () => { state.editing = true; render(); });
  } else {
    box.replaceChildren(el(`
      <div style="display:flex;flex-direction:column;flex:1;min-height:0">
        <div class="title-row">
          <input class="title" id="title" value="${esc(n.title)}" />
          <button class="primary small" id="save">保存</button>
          <button class="quiet small" id="cancel">やめる</button>
        </div>
        <div class="meta">${fmtDate(n.generated_at)} ${statusPill}</div>
        <textarea class="body" id="body">${esc(n.body)}</textarea>
      </div>
    `));
    document.getElementById("save")!.addEventListener("click", saveNote);
    document.getElementById("cancel")!.addEventListener("click", () => { state.editing = false; render(); });
    document.getElementById("body")!.addEventListener("keydown", (e) => {
      if ((e as KeyboardEvent).metaKey && (e as KeyboardEvent).key === "s") { e.preventDefault(); void saveNote(); }
    });
  }
  // つながり・本文内リンクのクリックでノートを開く
  box.querySelectorAll<HTMLElement>(".link").forEach((a) =>
    a.addEventListener("click", () => void openNote(a.dataset.id!)));
  box.querySelectorAll<HTMLAnchorElement>(".preview a").forEach((a) => {
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
    state.editing = false;
    render();
  } catch {
    toast("そのノートはまだありません");
  }
}

async function saveNote() {
  const n = state.selected!;
  const title = (document.getElementById("title") as HTMLInputElement).value.trim() || n.title;
  const body = (document.getElementById("body") as HTMLTextAreaElement).value;
  await api.noteSave(n.id, title, body);
  await refreshHome();
  state.selected = await api.noteGet(n.id);
  state.editing = false;
  render();
  toast("保存しました");
}

// ---- 画面C: 受信箱(最小) ----
function renderInbox(pane: HTMLElement) {
  const drafts = state.home!.drafts;
  const box = el(`<div class="inbox"></div>`);
  pane.replaceChildren(box);
  if (!drafts.length) {
    box.appendChild(el(`<div class="none">いまは何も届いていません。AI との会話から下書きが届くと、ここに並びます。</div>`));
    return;
  }
  for (const d of drafts) {
    const card = el(`
      <div class="prop-card">
        <div class="from">AI との会話から</div>
        <div class="what">下書き「${esc(d.title ?? d.id)}」を追加しますか?</div>
        <div class="ex">${esc(d.snippet)}</div>
        <div class="row">
          <button class="primary small" data-act="confirm">追加する</button>
          <button class="small" data-act="view">中身を見る</button>
          <button class="quiet small" data-act="reject">やめておく</button>
        </div>
      </div>
    `);
    card.querySelector('[data-act="confirm"]')!.addEventListener("click", async () => {
      await api.draftConfirm(d.id);
      await refreshHome();
      render();
      toast("ノートに追加しました");
    });
    card.querySelector('[data-act="view"]')!.addEventListener("click", async () => {
      state.view = "notes";
      state.selected = await api.noteGet(d.id);
      state.editing = false;
      render();
    });
    card.querySelector('[data-act="reject"]')!.addEventListener("click", async () => {
      await api.draftReject(d.id);
      await refreshHome();
      render();
      toast("やめておきました(あとから戻せます)");
    });
    box.appendChild(card);
  }
}

void boot();
