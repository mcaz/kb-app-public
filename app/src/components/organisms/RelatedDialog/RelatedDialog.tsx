import { Link, Sparkles, Waypoints } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";
import { Button } from "@/components/atoms/ui/button";
import { Command, CommandGroup, CommandItem, CommandList } from "@/components/atoms/ui/command";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/atoms/ui/dialog";
import { DegradedBanner } from "@/components/molecules/DegradedBanner";
import { NotePreview } from "@/components/organisms/NotePreview";
import { useMediaQuery } from "@/hooks/useMediaQuery";
import { useNote } from "@/lib/queries";
import { effectiveSearchPane, resolveSearchSelection, type SearchPane } from "@/lib/searchDialog";

import { relatedItemVariants } from "./variants";

export interface RelatedDialogProps {
  noteId: string | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onOpenNote: (id: string) => void;
  onOpenGraph: (id: string) => void;
}

type Tone = "linked" | "similar";

interface RelatedEntry {
  /** つながりと近いノートに同じノートが現れても選択が二重にならないよう、面ごとに分ける。 */
  key: string;
  id: string;
  title: string;
  distance: number | null;
  tone: Tone;
}

/** つながりの一覧と、選んだノートの本文を左右に並べて読む。 */
export function RelatedDialog({
  noteId,
  open,
  onOpenChange,
  onOpenNote,
  onOpenGraph,
}: RelatedDialogProps) {
  const { t } = useTranslation(["notes", "common"]);
  const { data: note } = useNote(noteId);
  const compact = useMediaQuery("(max-width: 759px)");
  const [compactPane, setCompactPane] = useState<SearchPane>("results");
  const [selectedKey, setSelectedKey] = useState<string | null>(null);

  const linked: RelatedEntry[] = (note?.related ?? []).map(([id, title]) => ({
    key: `linked:${id}`,
    id,
    title: title ?? id,
    distance: null,
    tone: "linked",
  }));
  const similar: RelatedEntry[] = (note?.similar ?? []).map(([id, title, distance]) => ({
    key: `similar:${id}`,
    id,
    title: title ?? id,
    distance,
    tone: "similar",
  }));
  const entries = [...linked, ...similar];

  const effectiveSelectedKey = resolveSearchSelection(
    selectedKey,
    entries.map((entry) => entry.key),
  );
  const selected = entries.find((entry) => entry.key === effectiveSelectedKey) ?? null;
  const effectiveCompactPane = effectiveSearchPane(compact, compactPane);
  const showList = !compact || effectiveCompactPane === "results";
  const showPreview = !compact || effectiveCompactPane === "preview";

  const close = () => {
    setCompactPane("results");
    onOpenChange(false);
  };

  const openNote = (id: string) => {
    onOpenNote(id);
    close();
  };

  const select = (entry: RelatedEntry) => {
    setSelectedKey(entry.key);
    if (compact) setCompactPane("preview");
    else openNote(entry.id);
  };

  const group = (tone: Tone, list: RelatedEntry[], head: string, empty: string) => (
    <CommandGroup
      className="px-1 pb-2"
      heading={
        <span className="text-muted flex items-center gap-1.5 text-[11.5px] tracking-[0.08em]">
          <Icon as={tone === "linked" ? Link : Sparkles} size="sm" />
          {head}
          <span>({list.length})</span>
        </span>
      }
    >
      {list.length === 0 ? (
        <div className="text-muted px-2 py-1 text-xs">{empty}</div>
      ) : (
        list.map((entry) => (
          <CommandItem
            key={entry.key}
            value={entry.key}
            onMouseMove={() => setSelectedKey(entry.key)}
            onSelect={() => select(entry)}
            className={relatedItemVariants({ tone })}
          >
            <span className="min-w-0 flex-1 truncate">{entry.title}</span>
            {entry.distance != null && (
              <span className="shrink-0 text-[10.5px] opacity-75">{entry.distance.toFixed(2)}</span>
            )}
          </CommandItem>
        ))
      )}
    </CommandGroup>
  );

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) setCompactPane("results");
        onOpenChange(next);
      }}
    >
      <DialogContent
        showCloseButton={false}
        className="h-[80vh] max-h-[760px] w-[calc(100%-1rem)] max-w-[1180px] gap-0 overflow-hidden p-0 sm:max-w-[1180px]"
        onKeyDown={(event) => {
          if (!compact || effectiveCompactPane !== "preview" || event.key !== "Enter") return;
          if (event.nativeEvent.isComposing || !selected) return;
          event.preventDefault();
          openNote(selected.id);
        }}
      >
        <DialogTitle className="sr-only">{t("notes:related.dialogTitle")}</DialogTitle>
        <DialogDescription className="sr-only">
          {t("notes:related.dialogDescription")}
        </DialogDescription>

        <Command
          shouldFilter={false}
          value={effectiveSelectedKey ?? ""}
          onValueChange={(value) => setSelectedKey(value || null)}
          className="bg-ground rounded-none"
        >
          {note && (
            <div className="border-line flex flex-none items-center gap-2 border-b px-3 py-2.5">
              <div className="min-w-0 flex-1">
                <div className="text-muted text-[11px] tracking-wide">
                  {t("notes:related.dialogTitle")}
                </div>
                <div className="truncate text-sm font-semibold">{note.title}</div>
              </div>
              <Button
                size="sm"
                onClick={() => {
                  onOpenGraph(note.id);
                  close();
                }}
              >
                <Icon as={Waypoints} size="sm" />
                {t("notes:related.openGraph")}
              </Button>
            </div>
          )}

          <div className="grid min-h-0 flex-1 grid-cols-[minmax(320px,42%)_minmax(0,1fr)] max-[759px]:grid-cols-1">
            {showList && (
              <section className="border-line flex min-h-0 min-w-0 flex-col border-r max-[759px]:border-r-0">
                <DegradedBanner items={note?.degraded ?? []} variant="inline" />
                <CommandList className="max-h-none min-h-0 flex-1 px-2 py-2">
                  {group(
                    "linked",
                    linked,
                    t("notes:related.linked"),
                    t("notes:related.linkedEmpty"),
                  )}
                  {group(
                    "similar",
                    similar,
                    t("notes:related.similar"),
                    t("notes:related.similarEmpty"),
                  )}
                </CommandList>
              </section>
            )}

            {showPreview && (
              <section className="bg-panel min-h-0 min-w-0">
                <NotePreview
                  noteId={selected?.id ?? null}
                  compact={compact}
                  emptyLabel={t("notes:related.previewEmpty")}
                  backLabel={t("notes:related.backToList")}
                  openLabel={t("notes:related.openAsMain")}
                  onBack={() => setCompactPane("results")}
                  onOpen={openNote}
                  onOpenLink={openNote}
                />
              </section>
            )}
          </div>

          <footer className="border-line text-muted flex h-10 flex-none items-center gap-4 border-t px-3 text-[11px] max-[560px]:hidden">
            {showList && <span>{t("notes:related.keyMove")}</span>}
            <span>{t("notes:related.keyOpen")}</span>
            <span>{t("notes:related.keyClose")}</span>
          </footer>
        </Command>
      </DialogContent>
    </Dialog>
  );
}
