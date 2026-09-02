import { Link, Sparkles, Waypoints } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";
import { Button } from "@/components/atoms/ui/button";
import { Command, CommandItem, CommandList } from "@/components/atoms/ui/command";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/atoms/ui/dialog";
import { DegradedBanner } from "@/components/molecules/DegradedBanner";
import { NotePreview } from "@/components/organisms/NotePreview";
import { useMediaQuery } from "@/hooks/useMediaQuery";
import { formatDateTime } from "@/lib/format";
import { useHomeState, useNote } from "@/lib/queries";
import { effectiveSearchPane, resolveSearchSelection, type SearchPane } from "@/lib/searchDialog";

import { relatedItemVariants, relatedTabVariants } from "./variants";

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
  /** 一覧の判断材料。NoteView の related/similar は id と title しか持たないので home 側から補う。 */
  snippet: string;
  tags: string[];
  updated: string | null;
}

/** つながりの一覧と、選んだノートの本文を左右に並べて読む。 */
export function RelatedDialog({
  noteId,
  open,
  onOpenChange,
  onOpenNote,
  onOpenGraph,
}: RelatedDialogProps) {
  const { t, i18n } = useTranslation(["notes", "common"]);
  const { data: note } = useNote(noteId);
  const { data: home } = useHomeState();
  const compact = useMediaQuery("(max-width: 759px)");
  const [compactPane, setCompactPane] = useState<SearchPane>("results");
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  // つながりが本命なので既定はこちら。近いノートはまだ関連づいていない候補。
  const [tab, setTab] = useState<Tone>("linked");

  // recent(500) の範囲外だと本文要約・タグ・更新日が付かない。その行はタイトルだけで出す。
  const summaries = new Map((home?.notes ?? []).map((hit) => [hit.id, hit]));
  const entryOf = (id: string, title: string | null, tone: Tone, distance: number | null) => {
    const hit = summaries.get(id);
    return {
      key: `${tone}:${id}`,
      id,
      title: title ?? hit?.title ?? id,
      distance,
      tone,
      snippet: hit?.snippet ?? "",
      tags: hit?.tags ?? [],
      updated: hit?.updated ?? null,
    };
  };
  const linked: RelatedEntry[] = (note?.related ?? []).map(([id, title]) =>
    entryOf(id, title, "linked", null),
  );
  const similar: RelatedEntry[] = (note?.similar ?? []).map(([id, title, distance]) =>
    entryOf(id, title, "similar", distance),
  );
  const entries = tab === "linked" ? linked : similar;

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
    setTab("linked");
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

  const tabs: { tone: Tone; label: string; count: number }[] = [
    { tone: "linked", label: t("notes:related.linked"), count: linked.length },
    { tone: "similar", label: t("notes:related.similar"), count: similar.length },
  ];

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) {
          setCompactPane("results");
          setTab("linked");
        }
        onOpenChange(next);
      }}
    >
      <DialogContent
        showCloseButton={false}
        className="h-[80vh] max-h-[760px] w-[calc(100%-1rem)] max-w-[1180px] gap-0 overflow-hidden p-0 sm:max-w-[1180px]"
        // capture で拾うのは、cmdk が Enter を onSelect に変えるより先に決めたいため。
        onKeyDownCapture={(event) => {
          if (event.nativeEvent.isComposing) return;
          if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
            event.preventDefault();
            setTab((current) => (current === "linked" ? "similar" : "linked"));
            return;
          }
          if (event.key === "Enter" && selected) {
            event.preventDefault();
            event.stopPropagation();
            openNote(selected.id);
          }
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
                size="icon"
                aria-label={t("notes:related.openGraph")}
                title={t("notes:related.openGraph")}
                onClick={() => {
                  onOpenGraph(note.id);
                  close();
                }}
              >
                <Icon as={Waypoints} size="sm" />
              </Button>
            </div>
          )}

          <div className="grid min-h-0 flex-1 grid-cols-[minmax(320px,42%)_minmax(0,1fr)] max-[759px]:grid-cols-1">
            {showList && (
              <section className="border-line flex min-h-0 min-w-0 flex-col border-r max-[759px]:border-r-0">
                <DegradedBanner items={note?.degraded ?? []} variant="inline" />
                <div className="border-line flex flex-none items-end gap-1 border-b px-2 pt-2">
                  {tabs.map((entry) => (
                    <button
                      key={entry.tone}
                      type="button"
                      aria-pressed={tab === entry.tone}
                      onClick={() => setTab(entry.tone)}
                      className={relatedTabVariants({
                        tone: entry.tone,
                        active: tab === entry.tone,
                      })}
                    >
                      <Icon as={entry.tone === "linked" ? Link : Sparkles} size="sm" />
                      {entry.label}
                      <span className="opacity-70">({entry.count})</span>
                    </button>
                  ))}
                </div>
                <CommandList className="max-h-none min-h-0 flex-1 px-2 py-2">
                  {entries.length === 0 ? (
                    <div className="text-muted px-3 py-2 text-xs">
                      {tab === "linked"
                        ? t("notes:related.linkedEmpty")
                        : t("notes:related.similarEmpty")}
                    </div>
                  ) : (
                    entries.map((entry) => (
                      <CommandItem
                        key={entry.key}
                        value={entry.key}
                        onMouseMove={() => setSelectedKey(entry.key)}
                        onSelect={() => select(entry)}
                        className={relatedItemVariants()}
                      >
                        <div className="flex min-w-0 items-start gap-2">
                          <span className="line-clamp-2 min-w-0 flex-1 font-semibold">
                            {entry.title}
                          </span>
                          {entry.updated && (
                            <span className="text-muted shrink-0 text-[10px]">
                              {formatDateTime(
                                entry.updated,
                                i18n.language,
                                t("common:date.unknown"),
                              )}
                            </span>
                          )}
                        </div>
                        {entry.snippet && (
                          <div className="text-muted line-clamp-2 text-xs">{entry.snippet}</div>
                        )}
                        {(entry.tags.length > 0 || entry.distance != null) && (
                          <div className="text-muted flex items-center gap-2 text-[10.5px]">
                            <span className="min-w-0 truncate">{entry.tags.join(" · ")}</span>
                            {entry.distance != null && (
                              <span className="ml-auto shrink-0">{entry.distance.toFixed(2)}</span>
                            )}
                          </div>
                        )}
                      </CommandItem>
                    ))
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
                  showOpenLabel={false}
                  onBack={() => setCompactPane("results")}
                  onOpen={openNote}
                  onOpenLink={openNote}
                />
              </section>
            )}
          </div>
        </Command>
      </DialogContent>
    </Dialog>
  );
}
