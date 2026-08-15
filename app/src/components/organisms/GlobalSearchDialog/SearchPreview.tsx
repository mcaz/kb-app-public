import { ArrowLeft, ExternalLink } from "lucide-react";
import { useTranslation } from "react-i18next";

import { StatusPill } from "@/components/atoms/StatusPill";
import { TagChip } from "@/components/atoms/TagChip";
import { Button } from "@/components/atoms/ui/button";
import { MarkdownView } from "@/components/molecules/MarkdownView";
import { IN_TAURI } from "@/lib/api";
import { formatDateTime } from "@/lib/format";
import { useNote } from "@/lib/queries";

interface SearchPreviewProps {
  noteId: string | null;
  compact: boolean;
  onBack: () => void;
  onOpen: (id: string) => void;
}

export function SearchPreview({ noteId, compact, onBack, onOpen }: SearchPreviewProps) {
  const { t, i18n } = useTranslation(["notes", "common"]);
  const { data: note, isPending } = useNote(noteId);
  const at = (iso: string | null) => formatDateTime(iso, i18n.language, t("common:date.unknown"));

  if (!noteId) {
    return (
      <div className="text-muted flex h-full items-center justify-center p-8 text-sm">
        {t("notes:search.previewEmpty")}
      </div>
    );
  }

  if (isPending || !note) {
    return (
      <div className="text-muted flex h-full items-center justify-center p-8 text-sm">
        {t("common:state.loading")}
      </div>
    );
  }

  return (
    <article className="flex h-full min-w-0 flex-col overflow-hidden">
      <div className="border-line flex flex-none items-center gap-2 border-b px-4 py-2.5">
        {compact && (
          <Button variant="quiet" size="sm" onClick={onBack}>
            <ArrowLeft className="size-4" />
            {t("notes:search.backToResults")}
          </Button>
        )}
        <span className="min-w-0 flex-1" />
        <Button size="sm" onClick={() => onOpen(note.id)}>
          <ExternalLink className="size-3.5" />
          {t("notes:search.openNote")}
        </Button>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-6 py-5 max-[760px]:px-4">
        <div className="text-muted mb-2 flex flex-wrap items-center gap-2 text-xs">
          <span>
            {t("common:date.createdUpdated", {
              created: at(note.created_at),
              updated: at(note.generated_at),
            })}
          </span>
          {note.status === "deprecated" && <StatusPill>{t("notes:note.deprecated")}</StatusPill>}
        </div>
        <h2 className="mb-2 text-2xl leading-tight font-bold">{note.title}</h2>
        {note.description && (
          <p className="text-muted mb-3 text-sm leading-relaxed">{note.description}</p>
        )}
        <div className="mb-5 flex flex-wrap gap-1.5">
          {note.tags.map((tag) => (
            <TagChip key={tag} tag={tag} />
          ))}
        </div>
        <MarkdownView
          body={note.body}
          vaultRoot={note.vault_root}
          inTauri={IN_TAURI}
          onOpenNote={onOpen}
        />
      </div>
    </article>
  );
}
