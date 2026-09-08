import { useState } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/atoms/ui/button";
import { CategoryNoteList } from "@/components/organisms/CategoryNoteList";
import { NotePane } from "@/components/organisms/NotePane";
import { RelatedDialog } from "@/components/organisms/RelatedDialog";
import { NotesLayout } from "@/components/templates/NotesLayout";
import { useSession } from "@/lib/stores/session";

export interface NotesPageProps {
  onOpenSearch: () => void;
}

/** カテゴリ → 一覧 → 本文。一覧と本文は同じ主領域を丸ごと使う。 */
export function NotesPage({ onOpenSearch }: NotesPageProps) {
  const { t } = useTranslation("notes");
  const selectedId = useSession((s) => s.selectedId);
  const selectedCategory = useSession((s) => s.selectedCategory);
  const browsePane = useSession((s) => s.browsePane);
  const openNote = useSession((s) => s.openNote);
  const openListedNote = useSession((s) => s.openListedNote);
  const showCategoryList = useSession((s) => s.showCategoryList);
  const focusGraph = useSession((s) => s.focusGraph);
  const [relatedOpen, setRelatedOpen] = useState(false);

  const list = selectedCategory !== null && (
    <CategoryNoteList
      category={selectedCategory}
      selectedId={selectedId}
      onOpenNote={openListedNote}
    />
  );
  const showList = selectedCategory !== null && browsePane === "list";

  return (
    <NotesLayout>
      {showList ? (
        list
      ) : selectedId ? (
        <div className="flex min-w-0 flex-1 flex-col overflow-hidden">
          <NotePane
            key={selectedId}
            noteId={selectedId}
            onBack={selectedCategory !== null ? showCategoryList : undefined}
            onOpenRelated={() => setRelatedOpen(true)}
          />
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
        onOpenNote={openNote}
        onOpenGraph={focusGraph}
      />
    </NotesLayout>
  );
}
