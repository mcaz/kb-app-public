import { Link, Sparkles, Waypoints } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";
import { Button } from "@/components/atoms/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/atoms/ui/dialog";
import { DegradedBanner } from "@/components/molecules/DegradedBanner";
import { RelatedList } from "@/components/molecules/RelatedList";
import { useNote } from "@/lib/queries";

export interface RelatedDialogProps {
  noteId: string | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onOpenNote: (id: string) => void;
  onOpenGraph: (id: string) => void;
}

/** 横ペインを置けない幅で、つながりと近いノートを同じ内容のまま見せる。 */
export function RelatedDialog({
  noteId,
  open,
  onOpenChange,
  onOpenNote,
  onOpenGraph,
}: RelatedDialogProps) {
  const { t } = useTranslation("notes");
  const { data: note } = useNote(noteId);

  const openNote = (id: string) => {
    onOpenNote(id);
    onOpenChange(false);
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="w-[calc(100%-1rem)] max-w-[620px] sm:max-w-[620px]">
        <DialogHeader>
          <DialogTitle>{t("related.dialogTitle")}</DialogTitle>
          <DialogDescription className="sr-only">
            {t("related.dialogDescription")}
          </DialogDescription>
        </DialogHeader>

        {note && (
          <>
            <DegradedBanner items={note.degraded} variant="card" />
            <Button
              className="w-full justify-start"
              onClick={() => {
                onOpenGraph(note.id);
                onOpenChange(false);
              }}
            >
              <Icon as={Waypoints} size="sm" />
              {t("related.openGraph")}
            </Button>
            <div className="flex max-h-[65vh] flex-col gap-1 overflow-y-auto">
              <RelatedList
                head={t("related.linked")}
                headIcon={Link}
                tone="linked"
                emptyLabel={t("related.linkedEmpty")}
                openId={null}
                onOpen={openNote}
                entries={note.related.map(([id, title]) => ({ id, title: title ?? id }))}
              />
              <div className="mt-3" />
              <RelatedList
                head={t("related.similar")}
                headIcon={Sparkles}
                tone="similar"
                emptyLabel={t("related.similarEmpty")}
                openId={null}
                onOpen={openNote}
                entries={note.similar.map(([id, title, distance]) => ({
                  id,
                  title: title ?? id,
                  distance,
                }))}
              />
            </div>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}
