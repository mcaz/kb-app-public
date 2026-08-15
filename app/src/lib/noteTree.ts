import type { Hit } from "@/lib/api";

export type NoteTreeItem =
  | { kind: "folder"; name: string; path: string; children: NoteTreeItem[] }
  | { kind: "note"; id: string; title: string };

interface MutableFolder {
  name: string;
  path: string;
  folders: Map<string, MutableFolder>;
  notes: { id: string; title: string }[];
}

const collator = new Intl.Collator("ja", { numeric: true, sensitivity: "base" });

const folder = (name: string, path: string): MutableFolder => ({
  name,
  path,
  folders: new Map(),
  notes: [],
});

function freezeFolder(value: MutableFolder): NoteTreeItem[] {
  const folders: NoteTreeItem[] = [...value.folders.values()]
    .sort((a, b) => collator.compare(a.name, b.name))
    .map((child) => ({
      kind: "folder",
      name: child.name,
      path: child.path,
      children: freezeFolder(child),
    }));
  const notes: NoteTreeItem[] = value.notes
    .sort((a, b) => collator.compare(a.title, b.title))
    .map((note) => ({ kind: "note", ...note }));
  return [...folders, ...notes];
}

/** ノート ID の `/` 区切りを、そのままサイドバー用の階層へ変換する。 */
export function buildNoteTree(notes: Hit[]): NoteTreeItem[] {
  const root = folder("", "");

  for (const note of notes) {
    const segments = note.id.split("/").filter(Boolean);
    const leaf = segments.pop() ?? note.id;
    let current = root;

    for (const segment of segments) {
      const path = current.path ? `${current.path}/${segment}` : segment;
      const next = current.folders.get(segment) ?? folder(segment, path);
      current.folders.set(segment, next);
      current = next;
    }

    current.notes.push({ id: note.id, title: note.title?.trim() || leaf });
  }

  return freezeFolder(root);
}

/** 選択ノートが検索などから開かれたとき、祖先フォルダを自動で開くための path 一覧。 */
export function noteAncestorPaths(id: string): string[] {
  const segments = id.split("/").filter(Boolean);
  segments.pop();
  return segments.map((_, index) => segments.slice(0, index + 1).join("/"));
}
