import type { NoteCategory } from "@/lib/api";

export interface CategoryTreeItem extends NoteCategory {
  children: CategoryTreeItem[];
}

const collator = new Intl.Collator("ja", { numeric: true, sensitivity: "base" });

const parentPath = (path: string) => {
  const index = path.lastIndexOf("/");
  return index < 0 ? "" : path.slice(0, index);
};

/** API の平坦なカテゴリ集計を、サイドバー表示用の木へ変換する。 */
export function buildCategoryTree(categories: NoteCategory[]): CategoryTreeItem[] {
  const nodes = new Map<string, CategoryTreeItem>();
  for (const category of categories) {
    nodes.set(category.path, { ...category, children: [] });
  }

  const roots: CategoryTreeItem[] = [];
  for (const node of nodes.values()) {
    if (node.path === "") {
      roots.push(node);
      continue;
    }
    const parent = nodes.get(parentPath(node.path));
    if (parent && parent.path !== "") parent.children.push(node);
    else roots.push(node);
  }

  const sort = (items: CategoryTreeItem[]) => {
    items.sort((a, b) => collator.compare(a.name, b.name));
    for (const item of items) sort(item.children);
    return items;
  };
  return sort(roots);
}

/** 検索結果からノートを開いたとき、カテゴリの祖先を自動展開するための path 一覧。 */
export function categoryAncestorPaths(path: string): string[] {
  const segments = path.split("/").filter(Boolean);
  segments.pop();
  return segments.map((_, index) => segments.slice(0, index + 1).join("/"));
}
