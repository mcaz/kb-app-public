// kb-app 管理アプリ(v0.2 骨格)。画面はモック(docs/ui-draft.html)の A/B/C に対応。
// ユーザーに見せる概念は「ノート・下書き・つながり・バックアップ」まで(原則7)。
import { convertFileSrc } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { marked } from "marked";
import { api, type ConnectState, type HomeState, type NoteView } from "./ipc";

const inTauri = "__TAURI_INTERNALS__" in window;

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

/// 本文に挿入する添付 URL。空白・日本語を含むファイル名でも markdown が壊れないよう
/// パーセントエンコードする(プレビュー側は decodeURIComponent で実パスへ戻す)。
function attachUrl(noteId: string, saved: string): string {
  return encodeURI(`/${noteId}.files/${saved}`);
}

function attachLink(noteId: string, saved: string): string {
  const url = attachUrl(noteId, saved);
  return /(png|jpe?g|gif|webp|svg)$/i.test(saved) ? `![](${url})` : `[${saved}](${url})`;
}

/// ペーストから画像添付を試みる。DOM 経路 → ダメなら Rust クリップボード読み。
/// 戻り値 = 保存名(画像が無ければ null)。
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
  if (items.some((i) => i.kind === "string")) return null; // 通常のテキストペースト
  // DOM に何も来ない = WKWebView の画像ペースト → Rust 側でクリップボードを直接読む
  const res = await api.attachmentPaste(noteId);
  if (!res) return null;
  if (res[1]) toast(`⚠ ${res[1]}`);
  return res[0];
}

async function handleEditorPaste(e: ClipboardEvent, n: NoteView, bodyEl: HTMLTextAreaElement) {
  try {
    const saved = await pasteImage(e, n.id);
    if (!saved) return;
    const link = attachLink(n.id, saved);
    const pos = bodyEl.selectionStart;
    bodyEl.value = bodyEl.value.slice(0, pos) + link + bodyEl.value.slice(bodyEl.selectionEnd);
    bodyEl.selectionStart = bodyEl.selectionEnd = pos + link.length;
    bodyEl.dispatchEvent(new Event("input")); // ライブプレビューへ反映
    toast("画像を添付しました");
  } catch (err2) {
    toast(`ペースト添付に失敗: ${err2}`);
  }
}

// ファイルのドラッグ&ドロップ(FR-C8)。Tauri は DOM の drop にファイルを渡さず
// 自前イベントで絶対パスをくれるため、そちらを購読して Rust 側で読み込む。
// 編集中はカーソル位置にリンク挿入、閲覧中は添付のみ。
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
    const savedNames: string[] = [];
    for (const p of paths) {
      try {
        const [saved, warning] = await api.attachmentAddFromPath(n.id, p);
        if (warning) toast(`⚠ ${warning}`);
        savedNames.push(saved);
      } catch (e) {
        toast(`添付に失敗: ${e}`);
      }
    }
    if (!savedNames.length) return;
    const bodyEl = document.getElementById("body") as HTMLTextAreaElement | null;
    if (state.editing && bodyEl) {
      const links = savedNames.map((s) => attachLink(n.id, s)).join("\n");
      const pos = bodyEl.selectionStart;
      bodyEl.value = bodyEl.value.slice(0, pos) + links + bodyEl.value.slice(bodyEl.selectionEnd);
      bodyEl.selectionStart = bodyEl.selectionEnd = pos + links.length;
      bodyEl.dispatchEvent(new Event("input")); // ライブプレビューへ反映
    } else {
      state.selected = await api.noteGet(n.id);
      render();
    }
    toast(`添付しました(${savedNames.join(", ")})`);
  });
}

// 閲覧モードでも、ノートを開いていればペーストで添付できる(リンク挿入はなし)
document.addEventListener("paste", (e) => {
  const t = e.target as HTMLElement | null;
  if (t && (t.tagName === "TEXTAREA" || t.tagName === "INPUT")) return; // エディタ側が処理
  const n = state.selected;
  if (!n || state.editing || state.view !== "notes") return;
  void (async () => {
    try {
      const saved = await pasteImage(e as ClipboardEvent, n.id);
      if (!saved) return;
      state.selected = await api.noteGet(n.id);
      render();
      toast(`画像を添付しました(${saved})`);
    } catch (err2) {
      toast(`ペースト添付に失敗: ${err2}`);
    }
  })();
});

const app = document.getElementById("app")!;

type View = "notes" | "inbox" | "connect";
type Tab = "all" | "human" | "agent";
const state = {
  view: "notes" as View,
  tab: ((localStorage.getItem("kb.tab") as Tab) || "all") as Tab,
  vaultName: "わたしのノート",
  home: null as HomeState | null,
  selected: null as NoteView | null,
  editing: false,
  searching: false,
  query: "",
};

// タブによる所有フィルタ(origin 不明は human 扱い)
function matchTab(origin: string | null): boolean {
  if (state.tab === "all") return true;
  return state.tab === "agent" ? origin === "agent" : origin !== "agent";
}

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
          <button class="nav ${state.view === "connect" ? "on" : ""}" id="nav-connect"><span>🔗 繋ぐ</span></button>
          <button class="nav grow-btn" id="nav-new"><span>＋ 新しいノート</span></button>
        </nav>
        <div id="pane"></div>
      </div>
    </div>
  `);
  app.replaceChildren(shell);
  document.getElementById("nav-notes")!.addEventListener("click", () => { state.view = "notes"; render(); });
  document.getElementById("nav-inbox")!.addEventListener("click", () => { state.view = "inbox"; render(); });
  document.getElementById("nav-connect")!.addEventListener("click", () => { state.view = "connect"; render(); });
  document.getElementById("nav-new")!.addEventListener("click", newNote);
  const pane = document.getElementById("pane")!;
  pane.style.display = "flex";
  pane.style.flex = "1";
  pane.style.minWidth = "0";
  if (state.view === "notes") renderNotes(pane);
  else if (state.view === "inbox") renderInbox(pane);
  else renderConnect(pane);
}

// window.prompt/alert/confirm は Tauri(WKWebView)では無効(黙って null)。
// ダイアログは必ずアプリ内モーダルで実装する。
function askTitle(): Promise<string | null> {
  return new Promise((resolve) => {
    const overlay = el(`
      <div class="modal-overlay">
        <div class="modal">
          <div class="modal-title">新しいノート</div>
          <input id="modal-input" placeholder="タイトル" />
          <div class="modal-row">
            <button class="primary" id="modal-ok">作成</button>
            <button class="quiet" id="modal-cancel">やめる</button>
          </div>
        </div>
      </div>
    `);
    const input = overlay.querySelector<HTMLInputElement>("#modal-input")!;
    const done = (v: string | null) => { overlay.remove(); resolve(v); };
    overlay.querySelector("#modal-ok")!.addEventListener("click", () => done(input.value.trim() || null));
    overlay.querySelector("#modal-cancel")!.addEventListener("click", () => done(null));
    overlay.addEventListener("click", (e) => { if (e.target === overlay) done(null); });
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") done(input.value.trim() || null);
      if (e.key === "Escape") done(null);
    });
    document.body.appendChild(overlay);
    input.focus();
  });
}

async function newNote() {
  const title = await askTitle();
  if (!title) return;
  const id = await api.noteNew(title);
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
      <div class="tabs">
        <button class="tab ${state.tab === "all" ? "on" : ""}" data-tab="all">すべて</button>
        <button class="tab ${state.tab === "human" ? "on" : ""}" data-tab="human">📝 メモ</button>
        <button class="tab ${state.tab === "agent" ? "on" : ""}" data-tab="agent">🤖 AI</button>
      </div>
      <div class="searchbox"><input id="search" placeholder="🔍 ノートを検索" value="${esc(state.query)}" /></div>
      <div class="items" id="items"></div>
    </div>
  `);
  list.querySelectorAll<HTMLButtonElement>(".tab").forEach((b) =>
    b.addEventListener("click", () => {
      state.tab = b.dataset.tab as Tab;
      localStorage.setItem("kb.tab", state.tab);
      render();
    })
  );
  list.style.width = `${Number(localStorage.getItem("kb.listWidth")) || 230}px`;
  const splitter = el(`<div class="splitter"></div>`);
  splitter.addEventListener("mousedown", (e) => {
    e.preventDefault();
    splitter.classList.add("active");
    const startX = (e as MouseEvent).clientX;
    const startW = list.getBoundingClientRect().width;
    const move = (ev: MouseEvent) => {
      const w = Math.min(520, Math.max(160, startW + ev.clientX - startX));
      list.style.width = `${w}px`;
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
  const showItems = (allHits: { id: string; title: string | null; status: string; snippet: string; via: string; origin: string | null }[], searchMode: boolean) => {
    const hits = allHits.filter((h) => matchTab(h.origin));
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
  // つながりはノート末尾のセクションで表示(Obsidian 風)
  const relatedSection = n.related.length
    ? `<div class="related-section"><div class="head">🔗 つながり</div><ul>${n.related
        .map(
          ([id, t]) =>
            `<li><button class="rel" data-id="${esc(id)}">${esc(t ?? id)}<span class="rid">${esc(id)}</span></button></li>`
        )
        .join("")}</ul></div>`
    : "";
  const attachChips = n.attachments
    .map(
      ([name, size]) =>
        `<span class="chip">📎 ${esc(name)} <i>${fmtSize(size)}</i><button class="chip-x" data-name="${esc(name)}">×</button></span>`
    )
    .join("");
  if (!state.editing) {
    box.replaceChildren(el(`
      <div>
        <div class="title-row">
          <div class="title">${esc(n.title)}</div>
          ${n.origin === "agent"
            ? `<span class="status-pill agent">🤖 AI のノート</span>${n.status !== "draft" ? `<button class="quiet small" id="make-mine">自分のメモにする</button>` : ""}`
            : `<button class="small" id="edit">編集</button><button class="quiet small" id="delete">削除</button>`}
        </div>
        <div class="meta">${fmtDate(n.generated_at)} ${statusPill}</div>
        <div style="margin: 2px 0 12px;"><button class="small" id="talk">🤖 このノートについて Claude と話す</button></div>
        <div class="attach">${n.attachments.length ? `<span class="attach-label">添付:</span>` : ""}${attachChips}<button class="quiet small" id="attach-add">＋ ファイルを添付</button><input type="file" id="attach-file" multiple hidden /></div>
        <div class="preview">${marked.parse(n.body) as string}</div>
        ${relatedSection}
      </div>
    `));
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
    hydratePreview(box.querySelector<HTMLElement>(".preview")!, n, true);
    document.getElementById("edit")?.addEventListener("click", () => { state.editing = true; render(); });
    document.getElementById("delete")?.addEventListener("click", () => void confirmDelete(n));
    document.getElementById("make-mine")?.addEventListener("click", async () => {
      try {
        await api.noteMakeMine(n.id);
        state.selected = await api.noteGet(n.id);
        render();
        toast("自分のメモにしました(以後 AI は読むだけになります)");
      } catch (e) {
        toast(`${e}`);
      }
    });
    document.getElementById("talk")!.addEventListener("click", async () => {
      try {
        await api.launchAi(n.id);
        toast("Claude を開きました");
      } catch (e) {
        toast(`${e}`);
      }
    });
  } else {
    box.replaceChildren(el(`
      <div style="display:flex;flex-direction:column;flex:1;min-height:0">
        <div class="title-row">
          <input class="title" id="title" value="${esc(n.title)}" />
          <button class="primary small" id="save">保存</button>
          <button class="quiet small" id="cancel">やめる</button>
        </div>
        <div class="meta">${fmtDate(n.generated_at)} ${statusPill}</div>
        <div class="edit-split">
          <textarea class="body" id="body">${esc(n.body)}</textarea>
          <div class="preview live" id="live-preview"></div>
        </div>
      </div>
    `));
    // ライブプレビュー(左=編集/右=表示)
    {
      const bodyEl0 = box.querySelector<HTMLTextAreaElement>("#body")!;
      const live = box.querySelector<HTMLElement>("#live-preview")!;
      let timer: number | undefined;
      const renderLive = () => {
        live.innerHTML = marked.parse(bodyEl0.value) as string;
        hydratePreview(live, n, false);
      };
      renderLive();
      bodyEl0.addEventListener("input", () => {
        window.clearTimeout(timer);
        timer = window.setTimeout(renderLive, 150);
      });
    }
    document.getElementById("save")!.addEventListener("click", saveNote);
    document.getElementById("cancel")!.addEventListener("click", () => { state.editing = false; render(); });
    const bodyEl = document.getElementById("body") as HTMLTextAreaElement;
    bodyEl.addEventListener("keydown", (e) => {
      if ((e as KeyboardEvent).metaKey && (e as KeyboardEvent).key === "s") { e.preventDefault(); void saveNote(); }
    });
    // 画像ペースト → 自動添付+カーソル位置にリンク挿入(FR-C8)。
    // DOM 経路(ブラウザ・一部形式)→ ダメなら Rust クリップボード読み(WKWebView 対策)
    bodyEl.addEventListener("paste", (e) => {
      void handleEditorPaste(e as ClipboardEvent, n, bodyEl);
    });
  }
  // つながり(末尾セクション)のクリックでノートを開く
  box.querySelectorAll<HTMLElement>(".rel").forEach((a) =>
    a.addEventListener("click", () => void openNote(a.dataset.id!)));
}

/// プレビュー要素の後処理: vault 内画像を asset プロトコルで表示し、リンクを制御する。
/// navigable=false(編集中のライブプレビュー)はリンク遷移させない(編集内容を失わないため)。
function hydratePreview(root: HTMLElement, n: NoteView, navigable: boolean) {
  root.querySelectorAll<HTMLImageElement>("img").forEach((img) => {
    const src = decodeURIComponent(img.getAttribute("src") ?? "");
    if (src.startsWith("/") && inTauri) {
      img.src = convertFileSrc(`${n.vault_root}${src}`);
    }
  });
  root.querySelectorAll<HTMLAnchorElement>("a").forEach((a) => {
    a.addEventListener("click", (e) => {
      e.preventDefault();
      if (!navigable) return;
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

// 削除確認(confirm() は WKWebView で無効のためアプリ内モーダル)
function confirmDelete(n: NoteView): Promise<void> {
  return new Promise((resolve) => {
    const hasAttach = n.attachments.length > 0;
    const overlay = el(`
      <div class="modal-overlay">
        <div class="modal">
          <div class="modal-title">「${esc(n.title)}」を削除しますか?</div>
          <div class="modal-desc">${hasAttach ? `添付 ${n.attachments.length} 件も一緒に削除されます。` : ""}画面からは消えますが、履歴には残ります。</div>
          <div class="modal-row">
            <button class="danger" id="modal-del">削除する</button>
            <button class="quiet" id="modal-cancel">やめる</button>
          </div>
        </div>
      </div>
    `);
    const done = () => { overlay.remove(); resolve(); };
    overlay.querySelector("#modal-cancel")!.addEventListener("click", done);
    overlay.addEventListener("click", (e) => { if (e.target === overlay) done(); });
    overlay.querySelector("#modal-del")!.addEventListener("click", async () => {
      try {
        await api.noteDelete(n.id);
        state.selected = null;
        await refreshHome();
        done();
        render();
        toast("削除しました");
      } catch (e) {
        done();
        toast(`削除に失敗: ${e}`);
      }
    });
    document.body.appendChild(overlay);
  });
}

async function saveNote() {
  const n = state.selected!;
  const title = (document.getElementById("title") as HTMLInputElement).value.trim() || n.title;
  const body = (document.getElementById("body") as HTMLTextAreaElement).value;
  try {
    await api.noteSave(n.id, title, body);
  } catch (e) {
    toast(`保存に失敗: ${e}`);
    return;
  }
  await refreshHome();
  state.selected = await api.noteGet(n.id);
  state.editing = false;
  render();
  toast("保存しました");
}

// ---- 画面D: 繋ぐ(3カード、全部任意) ----
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
      <div class="desc">会話の中からあなたのノートが引かれ、会話で得た知見が下書きとして受信箱に届くようになります。</div>
      ${state_}
      <div class="row">${c.desktop === "not_connected" ? `<button class="primary small" id="con-ai">接続する</button>` : ""}</div>
    </div>
  `);
  card.querySelector("#con-ai")?.addEventListener("click", async () => {
    try {
      await api.connectDesktop();
      toast("接続しました。Claude Desktop を再起動すると使えます");
      state.view = "connect";
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
