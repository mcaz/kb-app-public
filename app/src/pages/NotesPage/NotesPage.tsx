import { ArrowLeft } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";
import { Button } from "@/components/atoms/ui/button";
import { CategoryNoteList } from "@/components/organisms/CategoryNoteList";
import { NotePane } from "@/components/organisms/NotePane";
import { RelatedDialog } from "@/components/organisms/RelatedDialog";
import { RelatedNoteDialog } from "@/components/organisms/RelatedNoteDialog";
import { NotesLayout } from "@/components/templates/NotesLayout";
import { useMediaQuery } from "@/hooks/useMediaQuery";
import { useSession } from "@/lib/stores/session";

export interface NotesPageProps {
  onOpenSearch: () => void;
}

/** カテゴリ → 一覧 → 本文。狭い画面では一覧と本文を1段ずつ進む。 */
export function NotesPage({ onOpenSearch }: NotesPageProps) {
  const { t } = useTranslation("notes");
  const selectedId = useSession((s) => s.selectedId);
  const selectedCategory = useSession((s) => s.selectedCategory);
  const browsePane = useSession((s) => s.browsePane);
  const openNote = useSession((s) => s.openNote);
  const openListedNote = useSession((s) => s.openListedNote);
  const showCategoryList = useSession((s) => s.showCategoryList);
  const focusGraph = useSession((s) => s.focusGraph);
  const showSplitBrowse = useMediaQuery("(min-width: 960px)");
  const [relatedOpen, setRelatedOpen] = useState(false);
  const [relatedNoteId, setRelatedNoteId] = useState<string | null>(null);

  const list = selectedCategory !== null && (
    <CategoryNoteList
      category={selectedCategory}
      selectedId={selectedId}
      compact={!showSplitBrowse}
      onOpenNote={openListedNote}
    />
  );
  const showList = selectedCategory !== null && !showSplitBrowse && browsePane === "list";

  return (
    <NotesLayout browser={showSplitBrowse ? list : undefined}>
      {showList ? (
        list
      ) : selectedId ? (
        <div className="flex min-w-0 flex-1 flex-col overflow-hidden">
          {!showSplitBrowse && selectedCategory !== null && (
            <div className="border-line flex-none border-b px-3 py-2">
              <Button variant="quiet" size="sm" onClick={showCategoryList}>
                <Icon as={ArrowLeft} size="sm" />
                {t("browse.back")}
              </Button>
            </div>
          )}
          <NotePane noteId={selectedId} onOpenRelated={() => setRelatedOpen(true)} />
        </div>
      ) : (
        <div className="text-muted flex flex-1 flex-col items-center justify-center gap-3 p-[18px] text-center">
          <p>{selectedCategory === null ? t("browse.chooseCategory") : t("list.placeholder")}</p>
          <Button onClick={onOpenSearch}>{t("search.open")}</Button>
        </div>
      )}
      <RelatedDialog
        noteId={selectedId}
        open={relatedOpen}
        onOpenChange={setRelatedOpen}
        onOpenNote={setRelatedNoteId}
        onOpenGraph={focusGraph}
      />
      <RelatedNoteDialog
        noteId={relatedNoteId}
        onOpenChange={(open) => {
          if (!open) setRelatedNoteId(null);
        }}
        onOpenNote={setRelatedNoteId}
        onPromote={(id) => {
          openNote(id);
          setRelatedNoteId(null);
        }}
      />
    </NotesLayout>
  );
}
