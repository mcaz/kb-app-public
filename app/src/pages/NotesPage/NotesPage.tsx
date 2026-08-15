import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { Splitter } from "@/components/atoms/Splitter";
import { NotePane } from "@/components/organisms/NotePane";
import { RelatedDialog } from "@/components/organisms/RelatedDialog";
import { RelatedPanel } from "@/components/organisms/RelatedPanel";
import { NotesLayout } from "@/components/templates/NotesLayout";
import { Button } from "@/components/atoms/ui/button";
import { useMediaQuery } from "@/hooks/useMediaQuery";
import { usePrefs } from "@/lib/stores/prefs";
import { useSession } from "@/lib/stores/session";

export interface NotesPageProps {
  onOpenSearch: () => void;
}

/** ノート画面(本文 + 関連)。ノート探索はグローバル検索へ一本化する。 */
export function NotesPage({ onOpenSearch }: NotesPageProps) {
  const { t } = useTranslation("notes");
  const selectedId = useSession((s) => s.selectedId);
  const secondaryId = useSession((s) => s.secondaryId);
  const closeSecondary = useSession((s) => s.closeSecondary);
  const openNote = useSession((s) => s.openNote);
  const focusGraph = useSession((s) => s.focusGraph);
  const prefs = usePrefs();
  const showRelated = useMediaQuery("(min-width: 1280px)");
  const [relatedOpen, setRelatedOpen] = useState(false);

  useEffect(() => {
    if (!showRelated && secondaryId) closeSecondary();
  }, [closeSecondary, secondaryId, showRelated]);

  return (
    <NotesLayout
      related={
        showRelated && selectedId ? (
          <RelatedPanel noteId={selectedId} width={prefs.relWidth} />
        ) : undefined
      }
      relatedSplitter={
        showRelated && selectedId ? (
          <Splitter
            label={t("related.linked")}
            width={prefs.relWidth}
            min={160}
            max={420}
            onChange={(relWidth) => prefs.set({ relWidth })}
          />
        ) : undefined
      }
    >
      {selectedId ? (
        <>
          <div
            className="flex min-w-0 flex-1"
            style={
              showRelated && secondaryId && prefs.mainWidth
                ? { flex: "none", width: prefs.mainWidth }
                : undefined
            }
          >
            <NotePane noteId={selectedId} onOpenRelated={() => setRelatedOpen(true)} />
          </div>
          {showRelated && secondaryId && (
            <>
              <Splitter
                label={t("selection.secondary")}
                width={prefs.mainWidth ?? 640}
                min={320}
                max={1400}
                onChange={(mainWidth) => prefs.set({ mainWidth })}
              />
              <NotePane noteId={secondaryId} secondary />
            </>
          )}
        </>
      ) : (
        <div className="text-muted flex flex-1 flex-col items-center justify-center gap-3 p-[18px] text-center">
          <p>{t("list.placeholder")}</p>
          <Button onClick={onOpenSearch}>{t("search.open")}</Button>
        </div>
      )}
      <RelatedDialog
        noteId={selectedId}
        open={relatedOpen}
        onOpenChange={setRelatedOpen}
        onOpenNote={openNote}
        onOpenGraph={focusGraph}
      />
    </NotesLayout>
  );
}
