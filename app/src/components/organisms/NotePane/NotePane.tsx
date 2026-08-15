import { Link, MessageSquare } from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Icon } from "@/components/atoms/Icon";
import { StatusPill } from "@/components/atoms/StatusPill";
import { TagChip } from "@/components/atoms/TagChip";
import { Button } from "@/components/atoms/ui/button";
import { CareBar } from "@/components/molecules/CareBar";
import { FilePanel } from "@/components/organisms/FilePanel";
import { MarkdownView } from "@/components/molecules/MarkdownView";
import { useErrorText } from "@/hooks/useErrorText";
import { IN_TAURI } from "@/lib/api";
import { formatDateTime } from "@/lib/format";
import { useCareDismiss, useHomeState, useLaunchAi, useNote } from "@/lib/queries";
import { useSession } from "@/lib/stores/session";

export interface NotePaneProps {
  noteId: string;
  /** 並べて開いた副ペインなら true(閉じる/主にするが出る)。 */
  secondary?: boolean;
  /** 右ペインを置けない幅で、関連情報を Modal として開く。 */
  onOpenRelated?: () => void;
}

/** 1ノート分の表示(本文+操作)。 */
export function NotePane({ noteId, secondary = false, onOpenRelated }: NotePaneProps) {
  const { t, i18n } = useTranslation(["notes", "common"]);
  const { data: note } = useNote(noteId);
  const { data: home } = useHomeState();
  const openNote = useSession((s) => s.openNote);
  const addTag = useSession((s) => s.addTag);
  const closeSecondary = useSession((s) => s.closeSecondary);
  const promoteSecondary = useSession((s) => s.promoteSecondary);
  const errorText = useErrorText();
  const careDismiss = useCareDismiss();
  const launchAi = useLaunchAi();

  if (!note) return null;
  const care = (home?.care ?? []).filter((c) => c.a === note.id || c.b === note.id);
  const at = (iso: string | null) => formatDateTime(iso, i18n.language, t("common:date.unknown"));

  return (
    <article
      className={`flex min-w-0 flex-1 flex-col overflow-y-auto px-[22px] py-[18px] max-[1040px]:px-4 ${
        secondary ? "bg-panel min-w-[220px]" : ""
      }`}
    >
      <div className="mb-1 flex flex-none items-center gap-2.5">
        <h1 className="min-w-0 flex-1 text-lg font-bold">{note.title}</h1>
        {!secondary && onOpenRelated && (
          <Button variant="quiet" size="sm" className="min-[1280px]:hidden" onClick={onOpenRelated}>
            <Icon as={Link} size="sm" />
            {t("related.open")}
          </Button>
        )}
        {secondary && (
          <>
            <Button variant="quiet" size="sm" onClick={promoteSecondary}>
              {t("selection.makePrimary")}
            </Button>
            <Button
              variant="quiet"
              size="sm"
              aria-label={t("common:action.close")}
              onClick={closeSecondary}
            >
              ×
            </Button>
          </>
        )}
      </div>

      <div className="text-muted mb-3.5 flex flex-none flex-wrap items-center gap-2.5 text-xs">
        <span title={t("note.datesTitle")}>
          {t("common:date.createdUpdated", {
            created: at(note.created_at),
            updated: at(note.generated_at),
          })}
        </span>
        {note.status === "deprecated" && <StatusPill>{t("note.deprecated")}</StatusPill>}
        {note.tags.map((tag) => (
          <TagChip key={tag} tag={tag} onClick={() => addTag(tag)} />
        ))}
      </div>

      {care.map((proposal) => (
        <CareBar
          key={proposal.key}
          proposal={proposal}
          confirmLabel={t("note.careConfirm")}
          onDismiss={() => {
            careDismiss.mutate(proposal.key, {
              onSuccess: () => toast(t("note.careDismissed")),
            });
          }}
        />
      ))}

      <div className="my-0.5 mb-3 flex-none">
        <Button
          size="sm"
          onClick={() => {
            launchAi.mutate(note.id, {
              onSuccess: () => toast(t("note.opened")),
              onError: (e) => toast(errorText(e)),
            });
          }}
        >
          <Icon as={MessageSquare} size="sm" />
          {t("note.talk")}
        </Button>
      </div>

      <FilePanel noteId={note.id} />

      <MarkdownView
        body={note.body}
        vaultRoot={note.vault_root}
        inTauri={IN_TAURI}
        onOpenNote={openNote}
      />
    </article>
  );
}
