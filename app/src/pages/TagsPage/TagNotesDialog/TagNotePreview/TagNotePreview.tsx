import { ArrowLeft } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/atoms/ui/button";
import { NotePreview } from "@/components/organisms/NotePreview";
import { useNote } from "@/lib/queries";

import { tagNotePreviewVariants } from "./variants";

interface TagNotePreviewProps {
  noteId: string | null;
  compact: boolean;
  onBack: () => void;
  onOpen: (id: string) => void;
}

/** 本文取得の失敗でも、狭幅の戻る操作と再試行を残す。 */
export function TagNotePreview({ noteId, compact, onBack, onOpen }: TagNotePreviewProps) {
  const { t } = useTranslation("tags");
  const styles = tagNotePreviewVariants();
  const note = useNote(noteId);

  return (
    <div className={styles.root()}>
      {compact && !note.data && (
        <div className={styles.header()}>
          <Button
            type="button"
            variant="quiet"
            size="sm"
            className={styles.button()}
            onClick={onBack}
          >
            <ArrowLeft />
            {t("notes.back")}
          </Button>
        </div>
      )}
      {note.isError && (
        <div role="alert" className={styles.alert()}>
          <p>{t(note.data ? "notes.previewRefreshError" : "notes.previewError")}</p>
          <Button
            type="button"
            size="sm"
            className={styles.button()}
            disabled={note.isFetching}
            onClick={() => void note.refetch()}
          >
            {t("retry")}
          </Button>
        </div>
      )}
      {(!note.isError || note.data) && (
        <div className={styles.body()}>
          <NotePreview
            noteId={noteId}
            compact={compact}
            emptyLabel={t("notes.previewEmpty")}
            backLabel={t("notes.back")}
            openLabel={t("notes.open")}
            showOpenLabel={false}
            onBack={onBack}
            onOpen={onOpen}
          />
        </div>
      )}
    </div>
  );
}
