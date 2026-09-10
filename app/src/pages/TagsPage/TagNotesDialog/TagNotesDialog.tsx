import { NotebookText, X } from "lucide-react";
import { useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/atoms/ui/button";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  TWO_PANE_DIALOG,
} from "@/components/atoms/ui/dialog";
import { DegradedBanner } from "@/components/molecules/DegradedBanner";
import { NoteResultRow, noteResultItemVariants } from "@/components/molecules/NoteResultRow";
import { useMediaQuery } from "@/hooks/useMediaQuery";
import { useNoteBrowse } from "@/lib/queries";
import { resolveSearchSelection, type SearchPane } from "@/lib/searchDialog";
import { useSession } from "@/lib/stores/session";

import { TagNotePreview } from "./TagNotePreview";
import { tagNotesDialogVariants } from "./variants";

import type { TagInfo } from "@/lib/api";

interface TagNotesDialogProps {
  tag: TagInfo;
  onClose: () => void;
  onRestoreFocus: () => void;
}

/** タグごとにmountし直し、前に開いたタグの選択や検索セッションを持ち込まない。 */
export function TagNotesDialog({ tag, onClose, onRestoreFocus }: TagNotesDialogProps) {
  const { t } = useTranslation("tags");
  const styles = tagNotesDialogVariants();
  const compact = useMediaQuery("(max-width: 759px)");
  const [pane, setPane] = useState<SearchPane>("results");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLUListElement>(null);
  const goToNote = useSession((state) => state.openNote);
  const browse = useNoteBrowse([tag.tag], "all", "updated", true);
  const hits = useMemo(() => browse.data?.pages.flatMap((page) => page.hits) ?? [], [browse.data]);
  const selected = resolveSearchSelection(
    selectedId,
    hits.map((hit) => hit.id),
  );
  const total = browse.data?.pages[0]?.total;
  const showResults = !compact || pane === "results";
  const showPreview = !compact || pane === "preview";

  const openNote = (id: string) => {
    onClose();
    goToNote(id);
  };
  const select = (id: string) => {
    setSelectedId(id);
    if (compact) setPane("preview");
    else openNote(id);
  };
  const backToResults = () => {
    setPane("results");
    requestAnimationFrame(() => {
      const row = listRef.current?.querySelector<HTMLButtonElement>('button[data-selected="true"]');
      (row ?? contentRef.current)?.focus();
    });
  };
  const retry = () => {
    if (browse.isFetchNextPageError) void browse.fetchNextPage();
    else void browse.refetch();
  };

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <DialogContent
        ref={contentRef}
        className={`${TWO_PANE_DIALOG} ${styles.content()}`}
        showCloseButton={false}
        onOpenAutoFocus={(event) => {
          event.preventDefault();
          contentRef.current?.focus();
        }}
        onCloseAutoFocus={(event) => {
          event.preventDefault();
          onRestoreFocus();
        }}
        onEscapeKeyDown={(event) => {
          if (compact && pane === "preview") {
            event.preventDefault();
            backToResults();
          }
        }}
        onKeyDownCapture={(event) => {
          if (event.nativeEvent.isComposing || event.altKey || event.ctrlKey || event.metaKey)
            return;
          const target = event.target instanceof Element ? event.target : null;
          const action = target?.closest(
            "button, a, input, select, textarea, [contenteditable=true]",
          );
          // 追加取得・戻る等のEnterをノートを開く操作へ置き換えない。
          if (action && !action.hasAttribute("data-tag-note-row")) return;
          if (showResults && (event.key === "ArrowDown" || event.key === "ArrowUp")) {
            if (hits.length === 0) return;
            event.preventDefault();
            event.stopPropagation();
            const step = event.key === "ArrowDown" ? 1 : -1;
            const at = hits.findIndex((hit) => hit.id === selected);
            const next = (at + step + hits.length) % hits.length;
            setSelectedId(hits[next]?.id ?? null);
            listRef.current
              ?.querySelectorAll<HTMLButtonElement>("button[data-tag-note-row]")
              [next]?.focus();
          } else if (event.key === "Enter" && selected) {
            event.preventDefault();
            event.stopPropagation();
            if (compact && pane === "preview") openNote(selected);
            else select(selected);
          }
        }}
      >
        <DialogHeader className={styles.header()}>
          <DialogTitle className={styles.title()}>{t("notes.title", { tag: tag.tag })}</DialogTitle>
          <DialogDescription className={styles.description()}>
            {tag.description?.trim() || t("notes.description")}
          </DialogDescription>
        </DialogHeader>
        <DialogClose asChild>
          <Button
            type="button"
            variant="quiet"
            size="icon"
            className={styles.close()}
            aria-label={t("notes.close")}
          >
            <X />
          </Button>
        </DialogClose>

        <div className={styles.grid()}>
          {showResults && (
            <section className={styles.results()} aria-label={t("columns.notes")}>
              <DegradedBanner
                items={browse.data?.pages.flatMap((page) => page.degraded) ?? []}
                variant="inline"
              />
              <div className={styles.heading()}>
                <NotebookText className={styles.icon()} />
                <span>{t("columns.notes")}</span>
                {total !== undefined && <span>({total})</span>}
                {browse.isFetching && <span className={styles.loading()}>{t("loading")}</span>}
              </div>

              <ul
                ref={listRef}
                className={styles.list()}
                aria-label={t("notes.list", { tag: tag.tag })}
                aria-busy={browse.isFetching}
              >
                {hits.map((hit) => (
                  <li key={hit.id}>
                    <button
                      type="button"
                      data-tag-note-row
                      data-selected={selected === hit.id}
                      className={noteResultItemVariants({ class: styles.row() })}
                      onFocus={() => setSelectedId(hit.id)}
                      onMouseMove={() => setSelectedId(hit.id)}
                      onClick={() => select(hit.id)}
                    >
                      <NoteResultRow
                        title={hit.title ?? hit.id}
                        updated={hit.updated}
                        snippet={hit.snippet}
                        tags={hit.tags}
                      />
                    </button>
                  </li>
                ))}
                {browse.isPending && (
                  <li className={styles.empty()} role="status">
                    {t("loading")}
                  </li>
                )}
                {!browse.isPending && !browse.isError && hits.length === 0 && (
                  <li className={styles.empty()}>{t("notes.empty")}</li>
                )}
              </ul>

              {browse.isError && (
                <div role="alert" className={styles.alert()}>
                  <p>
                    {t(
                      browse.isFetchNextPageError
                        ? "notes.nextPageError"
                        : browse.data
                          ? "notes.refreshError"
                          : "notes.loadError",
                    )}
                  </p>
                  <Button
                    type="button"
                    size="sm"
                    className={styles.retry()}
                    disabled={browse.isFetching}
                    onClick={retry}
                  >
                    {t("retry")}
                  </Button>
                </div>
              )}
              {total !== undefined && (
                <div className={styles.pagination()}>
                  <span className={styles.loaded()}>
                    {t("notes.loaded", { loaded: hits.length, total })}
                  </span>
                  {browse.hasNextPage && !browse.isFetchNextPageError && (
                    <Button
                      type="button"
                      size="sm"
                      className={styles.retry()}
                      disabled={browse.isFetching}
                      onClick={() => void browse.fetchNextPage()}
                    >
                      {t("notes.loadMore")}
                    </Button>
                  )}
                </div>
              )}
            </section>
          )}
          {showPreview && (
            <section className={styles.preview()} aria-label={t("notes.preview")}>
              <TagNotePreview
                noteId={selected}
                compact={compact}
                onBack={backToResults}
                onOpen={openNote}
              />
            </section>
          )}
        </div>
        <footer className={styles.footer()}>
          {showResults && <span>{t("notes.keyMove")}</span>}
          <span>{t(compact && pane === "results" ? "notes.keyPreview" : "notes.keyOpen")}</span>
          <span>{t(compact && pane === "preview" ? "notes.keyBack" : "notes.keyClose")}</span>
        </footer>
      </DialogContent>
    </Dialog>
  );
}
