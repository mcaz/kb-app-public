import { useEffect, useId, useMemo, useState } from "react";
import { ChevronDown, ChevronRight, Folder, FolderOpen, NotebookText } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";
import {
  buildCategoryTree,
  categoryAncestorPaths,
  type CategoryTreeItem,
} from "@/lib/categoryTree";

import type { NoteCategory } from "@/lib/api";

export interface CategoryAccordionProps {
  categories: NoteCategory[];
  active: boolean;
  selectedCategory: string | null;
  onActivate: () => void;
  onSelectCategory: (path: string) => void;
}

export function CategoryAccordion({
  categories,
  active,
  selectedCategory,
  onActivate,
  onSelectCategory,
}: CategoryAccordionProps) {
  const { t } = useTranslation();
  const contentId = useId();
  const tree = useMemo(() => buildCategoryTree(categories), [categories]);
  const [expanded, setExpanded] = useState(true);
  const [openFolders, setOpenFolders] = useState<Set<string>>(() => new Set());

  useEffect(() => {
    const first = tree[0];
    if (selectedCategory !== null || !first) return;
    onSelectCategory(first.path);
  }, [onSelectCategory, selectedCategory, tree]);

  useEffect(() => {
    if (selectedCategory === null) return;
    let current = true;
    queueMicrotask(() => {
      if (!current) return;
      setExpanded(true);
      setOpenFolders((folders) => {
        const next = new Set(folders);
        let changed = false;
        for (const path of categoryAncestorPaths(selectedCategory)) {
          if (!next.has(path)) {
            next.add(path);
            changed = true;
          }
        }
        return changed ? next : folders;
      });
    });
    return () => {
      current = false;
    };
  }, [selectedCategory]);

  const selectCategory = (item: CategoryTreeItem) => {
    onSelectCategory(item.path);
    if (item.children.length) {
      setOpenFolders((current) => {
        const next = new Set(current);
        if (next.has(item.path)) next.delete(item.path);
        else next.add(item.path);
        return next;
      });
    }
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <button
        type="button"
        aria-expanded={expanded}
        aria-controls={contentId}
        onClick={() => {
          onActivate();
          setExpanded((value) => !value);
        }}
        className={`flex cursor-pointer items-center gap-2 rounded-md border-none bg-transparent px-2.5 py-1.5 text-left text-[13px] whitespace-nowrap ${
          active ? "bg-sel text-ink" : "text-muted hover:text-ink"
        }`}
      >
        <Icon as={NotebookText} />
        <span className="min-w-0 flex-1 truncate">{t("nav.notes")}</span>
        <Icon as={expanded ? ChevronDown : ChevronRight} size="sm" />
      </button>

      {expanded && (
        <div id={contentId} className="mt-0.5 min-h-0 overflow-y-auto pb-1" role="tree">
          {tree.length ? (
            tree.map((item) => (
              <CategoryItem
                key={item.path || "__root__"}
                item={item}
                depth={0}
                selectedCategory={selectedCategory}
                openFolders={openFolders}
                onSelect={selectCategory}
              />
            ))
          ) : (
            <p className="text-muted m-0 px-7 py-1 text-[11px]">{t("nav.notesEmpty")}</p>
          )}
        </div>
      )}
    </div>
  );
}

interface CategoryItemProps {
  item: CategoryTreeItem;
  depth: number;
  selectedCategory: string | null;
  openFolders: Set<string>;
  onSelect: (item: CategoryTreeItem) => void;
}

function CategoryItem({ item, depth, selectedCategory, openFolders, onSelect }: CategoryItemProps) {
  const { t } = useTranslation();
  const selected = selectedCategory === item.path;
  const hasChildren = item.children.length > 0;
  const open = hasChildren && openFolders.has(item.path);
  const name = item.path ? item.name : t("nav.rootNotes");
  const paddingLeft = 10 + depth * 12;

  return (
    <div role="treeitem" aria-expanded={hasChildren ? open : undefined} aria-selected={selected}>
      <button
        type="button"
        title={t("nav.categoryCount", { name, count: item.count })}
        aria-current={selected ? "page" : undefined}
        onClick={() => onSelect(item)}
        className={`flex w-full cursor-pointer items-center gap-1 rounded-md border-none bg-transparent py-1 pr-2 text-left text-[12px] ${
          selected ? "bg-sel text-ink" : "text-muted hover:bg-sel/60 hover:text-ink"
        }`}
        style={{ paddingLeft }}
      >
        {hasChildren ? (
          <Icon as={open ? ChevronDown : ChevronRight} size="sm" className="flex-none" />
        ) : (
          <span className="inline-block size-3.5 flex-none" aria-hidden="true" />
        )}
        <Icon as={open ? FolderOpen : Folder} size="sm" className="flex-none" />
        <span className="min-w-0 flex-1 truncate">{name}</span>
        <span className="text-muted ml-1 flex-none text-[11px] tabular-nums">{item.count}</span>
      </button>
      {open && (
        <div role="group">
          {item.children.map((child) => (
            <CategoryItem
              key={child.path}
              item={child}
              depth={depth + 1}
              selectedCategory={selectedCategory}
              openFolders={openFolders}
              onSelect={onSelect}
            />
          ))}
        </div>
      )}
    </div>
  );
}
