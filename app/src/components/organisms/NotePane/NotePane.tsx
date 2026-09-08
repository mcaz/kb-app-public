import { ArrowLeft, ChevronRight, Code, Eye, Link, MessageSquare } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Icon } from "@/components/atoms/Icon";
import { StatusPill } from "@/components/atoms/StatusPill";
import { TagChip } from "@/components/atoms/TagChip";
import { Button } from "@/components/atoms/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/atoms/ui/tooltip";
import { CareBar } from "@/components/molecules/CareBar";
import { DegradedBanner } from "@/components/molecules/DegradedBanner";
import { FilePanel } from "@/components/organisms/FilePanel";
import { MarkdownView } from "@/components/molecules/MarkdownView";
import { useCurrentNote } from "@/hooks/useCurrentNote";
import { useErrorText } from "@/hooks/useErrorText";
import { IN_TAURI } from "@/lib/api";
import { categoryAncestorPaths } from "@/lib/categoryTree";
import { formatDateTime } from "@/lib/format";
import { useCareDismiss, useHomeState, useLaunchAi, useNote } from "@/lib/queries";
import { useSession } from "@/lib/stores/session";

import { notePaneVariants } from "./variants";

export interface NotePaneProps {
  noteId: string;
  onBack?: () => void;
  /** 右ペインを置けない幅で、関連情報を Modal として開く。 */
  onOpenRelated?: () => void;
  /** Modal 内で Wikilink を辿る場合など、既定の画面遷移を差し替える。 */
  onOpenNote?: (id: string) => void;
}

/** 1ノート分の表示(本文+操作)。 */
export function NotePane({ noteId, onBack, onOpenRelated, onOpenNote }: NotePaneProps) {
  const [showSource, setShowSource] = useState(false);
  const styles = notePaneVariants();
  const { t, i18n } = useTranslation(["notes", "common"]);
  const { data: note } = useNote(noteId);
  const currentNote = useCurrentNote(note?.id ?? null);
  const { data: home } = useHomeState();
  const openMainNote = useSession((s) => s.openNote);
  const addTag = useSession((s) => s.addTag);
  const selectCategory = useSession((s) => s.selectCategory);
  const errorText = useErrorText();
  const careDismiss = useCareDismiss();
  const launchAi = useLaunchAi();

  if (!note) return null;
  const care = (home?.care ?? []).filter((c) => c.a === note.id || c.b === note.id);
  const at = (iso: string | null) => formatDateTime(iso, i18n.language, t("common:date.unknown"));

  const sourceActionLabel = showSource ? t("note.showPreview") : t("note.showSource");
  const categories = categoryAncestorPaths(note.id);

  return (
    <article className={styles.root()}>
      <header className={styles.header()}>
        <div className={styles.toolbar()}>
          {onBack && (
            <Button
              variant="quiet"
              size="icon"
              onClick={onBack}
              aria-label={t("browse.back")}
              title={t("browse.back")}
            >
              <Icon as={ArrowLeft} />
            </Button>
          )}
          <div className={styles.breadcrumb()}>
            {categories.map((category) => (
              <div key={category} className={styles.categoryItem()}>
                <Tooltip>
                  <TooltipTrigger asChild>
                    <button
                      type="button"
                      className={styles.category()}
                      onClick={() => selectCategory(category)}
                      aria-label={t("browse.panelLabel", { name: category })}
                    >
                      {category.slice(category.lastIndexOf("/") + 1)}
                    </button>
                  </TooltipTrigger>
                  <TooltipContent side="bottom" sideOffset={6}>
                    {t("browse.panelLabel", { name: category })}
                  </TooltipContent>
                </Tooltip>
                <Icon as={ChevronRight} size="sm" className="text-muted shrink-0" />
              </div>
            ))}
            <span className={styles.title()} title={note.title}>
              {note.title}
            </span>
          </div>
          <div className={styles.actions()}>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  variant="quiet"
                  size="icon"
                  aria-label={sourceActionLabel}
                  onClick={() => setShowSource((value) => !value)}
                >
                  <Icon as={showSource ? Eye : Code} size="sm" />
                </Button>
              </TooltipTrigger>
              <TooltipContent side="bottom" sideOffset={6}>
                {sourceActionLabel}
              </TooltipContent>
            </Tooltip>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  variant="quiet"
                  size="icon"
                  aria-label={t("note.talk")}
                  onClick={() => {
                    launchAi.mutate(note.id, {
                      onSuccess: () => toast(t("note.opened")),
                      onError: (e) => toast(errorText(e)),
                    });
                  }}
                >
                  <Icon as={MessageSquare} size="sm" />
                </Button>
              </TooltipTrigger>
              <TooltipContent side="bottom" sideOffset={6}>
                {t("note.talk")}
              </TooltipContent>
            </Tooltip>
            {onOpenRelated && (
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    variant="quiet"
                    size="icon"
                    aria-label={t("related.open")}
                    onClick={onOpenRelated}
                  >
                    <Icon as={Link} size="sm" />
                  </Button>
                </TooltipTrigger>
                <TooltipContent side="bottom" sideOffset={6}>
                  {t("related.open")}
                </TooltipContent>
              </Tooltip>
            )}
          </div>
        </div>
        <div className={styles.metadataScroll()}>
          <details open className={styles.metadata()}>
            <summary className={styles.summary()}>
              <Icon
                as={ChevronRight}
                size="sm"
                className="transition-transform group-open:rotate-90"
              />
              {t("note.metadata")}
            </summary>
            <dl className={styles.fields()}>
              <dt className={styles.label()}>{t("note.titleLabel")}</dt>
              <dd className={styles.value()}>
                <h1 className="text-base font-medium">{note.title}</h1>
              </dd>
              {note.description && (
                <>
                  <dt className={styles.label()}>{t("note.descriptionLabel")}</dt>
                  <dd className={styles.value()}>{note.description}</dd>
                </>
              )}
              <dt className={styles.label()}>{t("note.tagsLabel")}</dt>
              <dd className={styles.tags()}>
                {note.tags.map((tag) => (
                  <TagChip key={tag} tag={tag} onClick={() => addTag(tag)} />
                ))}
                {note.status === "deprecated" && <StatusPill>{t("note.deprecated")}</StatusPill>}
              </dd>
              <dt className={styles.label()}>{t("note.createdLabel")}</dt>
              <dd className={styles.value()}>{at(note.created_at)}</dd>
              <dt className={styles.label()}>{t("note.updatedLabel")}</dt>
              <dd className={styles.value()}>{at(note.generated_at)}</dd>
            </dl>
          </details>
        </div>
      </header>

      <div className={styles.scroll()}>
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

        <DegradedBanner items={[...note.degraded, ...currentNote.degraded]} variant="card" />
        {currentNote.error && <p role="alert">{errorText(currentNote.error)}</p>}

        <FilePanel noteId={note.id} />

        {showSource ? (
          <pre className={styles.source()}>
            <code>{note.body}</code>
          </pre>
        ) : (
          <div className={styles.body()}>
            <MarkdownView
              body={note.body}
              vaultRoot={note.vault_root}
              inTauri={IN_TAURI}
              onOpenNote={onOpenNote ?? openMainNote}
            />
          </div>
        )}
      </div>
    </article>
  );
}
