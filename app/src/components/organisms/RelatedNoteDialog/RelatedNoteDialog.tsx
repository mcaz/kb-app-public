import { useTranslation } from "react-i18next";

import { Button } from "@/components/atoms/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/atoms/ui/dialog";
import { NotePane } from "@/components/organisms/NotePane";

export interface RelatedNoteDialogProps {
  noteId: string | null;
  onOpenChange: (open: boolean) => void;
  onOpenNote: (id: string) => void;
  onPromote: (id: string) => void;
}

/** 関連リストから選んだノートを、現在の本文を保ったまま重ねて読む。 */
export function RelatedNoteDialog({
  noteId,
  onOpenChange,
  onOpenNote,
  onPromote,
}: RelatedNoteDialogProps) {
  const { t } = useTranslation("notes");

  return (
    <Dialog open={noteId != null} onOpenChange={onOpenChange}>
      <DialogContent className="h-[85vh] max-h-[860px] w-[calc(100%-1rem)] max-w-[960px] grid-rows-[auto_minmax(0,1fr)] gap-0 overflow-hidden p-0 sm:max-w-[960px]">
        <DialogTitle className="sr-only">{t("related.noteDialogTitle")}</DialogTitle>
        <DialogDescription className="sr-only">
          {t("related.noteDialogDescription")}
        </DialogDescription>

        <div className="border-line flex justify-end border-b px-12 py-2">
          {noteId && (
            <Button variant="quiet" size="sm" onClick={() => onPromote(noteId)}>
              {t("related.openAsMain")}
            </Button>
          )}
        </div>
        {noteId && <NotePane noteId={noteId} onOpenNote={onOpenNote} />}
      </DialogContent>
    </Dialog>
  );
}
