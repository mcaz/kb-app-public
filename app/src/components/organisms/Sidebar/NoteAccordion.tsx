import { useEffect, useId, useMemo, useState } from "react";
import {
  ChevronDown,
  ChevronRight,
  FileText,
  Folder,
  FolderOpen,
  NotebookText,
} from "lucide-react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";
import { buildNoteTree, noteAncestorPaths, type NoteTreeItem } from "@/lib/noteTree";

import type { Hit } from "@/lib/api";

export interface NoteAccordionProps {
  notes: Hit[];
  active: boolean;
  selectedId: string | null;
  onActivate: () => void;
  onOpenNote: (id: string) => void;
}

export function NoteAccordion({
  notes,
  active,
  selectedId,
  onActivate,
  onOpenNote,
}: NoteAccordionProps) {
  const { t } = useTranslation();
  const contentId = useId();
  const tree = useMemo(() => buildNoteTree(notes), [notes]);
  const [expanded, setExpanded] = useState(true);
  const [openFolders, setOpenFolders] = useState<Set<string>>(() => new Set());

  useEffect(() => {
    if (!selectedId) return;
    let current = true;
    queueMicrotask(() => {
      if (!current) return;
      setExpanded(true);
      setOpenFolders((folders) => {
        const next = new Set(folders);
        let changed = false;
        for (const path of noteAncestorPaths(selectedId)) {
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
  }, [selectedId]);

  const toggleFolder = (path: string) => {
    setOpenFolders((current) => {
      const next = new Set(current);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
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
              <TreeItem
                key={item.kind === "folder" ? item.path : item.id}
                item={item}
                depth={0}
                selectedId={selectedId}
                openFolders={openFolders}
                onToggleFolder={toggleFolder}
                onOpenNote={onOpenNote}
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

interface TreeItemProps {
  item: NoteTreeItem;
  depth: number;
  selectedId: string | null;
  openFolders: Set<string>;
  onToggleFolder: (path: string) => void;
  onOpenNote: (id: string) => void;
}

function TreeItem({
  item,
  depth,
  selectedId,
  openFolders,
  onToggleFolder,
  onOpenNote,
}: TreeItemProps) {
  const paddingLeft = 10 + depth * 12;

  if (item.kind === "note") {
    const selected = selectedId === item.id;
    return (
      <button
        type="button"
        role="treeitem"
        aria-selected={selected}
        aria-current={selected ? "page" : undefined}
        title={item.title}
        onClick={() => onOpenNote(item.id)}
        className={`flex w-full cursor-pointer items-center gap-1.5 rounded-md border-none bg-transparent py-1 pr-2 text-left text-[12px] ${
          selected ? "bg-sel text-ink" : "text-muted hover:bg-sel/60 hover:text-ink"
        }`}
        style={{ paddingLeft }}
      >
        <Icon as={FileText} size="sm" className="flex-none" />
        <span className="min-w-0 flex-1 truncate">{item.title}</span>
      </button>
    );
  }

  const open = openFolders.has(item.path);
  return (
    <div role="treeitem" aria-expanded={open} aria-selected={false}>
      <button
        type="button"
        title={item.name}
        onClick={() => onToggleFolder(item.path)}
        className="text-muted hover:bg-sel/60 hover:text-ink flex w-full cursor-pointer items-center gap-1 rounded-md border-none bg-transparent py-1 pr-2 text-left text-[12px]"
        style={{ paddingLeft }}
      >
        <Icon as={open ? ChevronDown : ChevronRight} size="sm" className="flex-none" />
        <Icon as={open ? FolderOpen : Folder} size="sm" className="flex-none" />
        <span className="min-w-0 flex-1 truncate">{item.name}</span>
      </button>
      {open && (
        <div role="group">
          {item.children.map((child) => (
            <TreeItem
              key={child.kind === "folder" ? child.path : child.id}
              item={child}
              depth={depth + 1}
              selectedId={selectedId}
              openFolders={openFolders}
              onToggleFolder={onToggleFolder}
              onOpenNote={onOpenNote}
            />
          ))}
        </div>
      )}
    </div>
  );
}
