import { convertFileSrc } from "@tauri-apps/api/core";
import { Download, FileQuestion, NotebookText, Paperclip } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Icon } from "@/components/atoms/Icon";
import { Button } from "@/components/atoms/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/atoms/ui/dialog";
import { MarkdownView } from "@/components/molecules/MarkdownView";
import { NotePreview } from "@/components/organisms/NotePreview";
import { useErrorText } from "@/hooks/useErrorText";
import { IN_TAURI } from "@/lib/api";
import { formatDateTime, formatSize } from "@/lib/format";
import { useFileDownload, useFileFetch, useFileOpen, useFilePreview } from "@/lib/queries";
import { useSession } from "@/lib/stores/session";

import { fileKind } from "./fileKind";
import { filePaneTabVariants, relatedItemVariants } from "./variants";

import type { FileCard } from "@/lib/api";

export interface FilePreviewDialogProps {
  file: FileCard | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

type Pane = "file" | "notes";

/**
 * ファイルの中身と、そのファイルを持っているノートを左右に並べて読む。
 *
 * 形は関連 Modal に合わせてある(左にタブと一覧、右に詳細)。「元のノート」で
 * 先頭1件だけを開く作りでは、2件以上が持つファイルの2件目以降へ行けなかった。
 */
export function FilePreviewDialog({ file, open, onOpenChange }: FilePreviewDialogProps) {
  const { t, i18n } = useTranslation("files");
  const errorText = useErrorText();
  const goToNote = useSession((state) => state.openNote);
  const [pane, setPane] = useState<Pane>("file");
  const [selectedNote, setSelectedNote] = useState<string | null>(null);
  const previewId = open && file?.availability === "local" ? file.id : null;
  const preview = useFilePreview(previewId);
  const external = useFileOpen();
  const download = useFileDownload();
  const fetch = useFileFetch();
  const kind = file ? fileKind(file) : "other";
  const isMarkdown = file ? /\.(md|markdown)$/iu.test(file.name) : false;
  const isHtml = file
    ? file.media_type.toLowerCase() === "text/html" || /\.html?$/iu.test(file.name)
    : false;
  const src = preview.data?.path ? convertFileSrc(preview.data.path) : null;

  // 別のファイルを開いたら面と選択を戻す。前のファイルのノートが残らないように。
  // effect ではなく描画中に調整する(React の「prop が変わったときの state 調整」)
  const [shownId, setShownId] = useState<string | null>(file?.id ?? null);
  if ((file?.id ?? null) !== shownId) {
    setShownId(file?.id ?? null);
    setPane("file");
    setSelectedNote(file?.notes[0]?.id ?? null);
  }

  const openNote = (id: string) => {
    onOpenChange(false);
    goToNote(id);
  };

  const body = !file ? null : file.availability === "missing" ? (
    <div className="flex flex-col items-center gap-3 text-center">
      <Icon as={FileQuestion} size="lg" className="text-muted" />
      <p className="text-muted text-sm">{t("missing")}</p>
      {file.can_fetch && (
        <Button disabled={fetch.isPending} onClick={() => fetch.mutate({ id: file.id })}>
          {t("fetch")}
        </Button>
      )}
    </div>
  ) : file.availability === "unavailable_by_policy" ? (
    <p className="text-muted text-sm">{t("unavailable")}</p>
  ) : preview.isPending ? (
    <p className="text-muted text-sm">{t("previewLoading")}</p>
  ) : preview.error ? (
    <p className="text-danger text-sm">{errorText(preview.error)}</p>
  ) : IN_TAURI && src && kind === "image" ? (
    <img
      src={src}
      alt={file.name}
      className="max-h-full max-w-full rounded-md object-contain shadow-lg"
    />
  ) : typeof preview.data?.text === "string" ? (
    <div className="border-line bg-background h-full w-full overflow-auto rounded-md border p-6">
      {isMarkdown ? (
        <MarkdownView body={preview.data.text} vaultRoot="" inTauri={false} onOpenNote={goToNote} />
      ) : (
        <pre className="text-ink min-w-full font-mono text-[13px] leading-relaxed break-words whitespace-pre-wrap">
          {preview.data.text}
        </pre>
      )}
    </div>
  ) : IN_TAURI && src && (kind === "pdf" || isHtml) ? (
    <iframe
      src={src}
      title={t("previewDescription", { name: file.name })}
      className="border-line bg-background h-full min-h-[480px] w-full rounded-md border"
      sandbox="allow-same-origin"
    />
  ) : (
    <div className="flex max-w-sm flex-col items-center gap-3 text-center">
      <Icon as={FileQuestion} size="lg" className="text-muted" />
      <p className="text-muted text-sm">{t("previewUnavailable")}</p>
    </div>
  );

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className="h-[min(780px,calc(100%-2rem))] max-w-[min(1080px,calc(100%-2rem))] grid-rows-[auto_minmax(0,1fr)] gap-0 overflow-hidden p-0"
        showCloseButton
      >
        {file && (
          <>
            <DialogHeader className="border-line min-w-0 border-b px-5 py-3 pr-12">
              <DialogTitle className="truncate text-[15px]">{file.name}</DialogTitle>
              <DialogDescription className="truncate text-xs">
                {file.availability === "local"
                  ? `${t(`filter.${kind}`)} · ${formatSize(file.size)}`
                  : t(`filter.${kind}`)}
              </DialogDescription>
            </DialogHeader>

            <div className="grid min-h-0 grid-cols-[minmax(280px,38%)_minmax(0,1fr)] max-[759px]:grid-cols-1">
              <section className="border-line flex min-h-0 min-w-0 flex-col border-r max-[759px]:border-r-0">
                <div className="border-line flex flex-none items-end gap-1 border-b px-2 pt-2">
                  <button
                    type="button"
                    aria-pressed={pane === "file"}
                    onClick={() => setPane("file")}
                    className={filePaneTabVariants({ active: pane === "file" })}
                  >
                    <Icon as={Paperclip} size="sm" />
                    {t("panes.file")}
                  </button>
                  <button
                    type="button"
                    aria-pressed={pane === "notes"}
                    onClick={() => setPane("notes")}
                    className={filePaneTabVariants({ active: pane === "notes" })}
                  >
                    <Icon as={NotebookText} size="sm" />
                    {t("panes.notes")}
                    <span className="opacity-70">({file.notes.length})</span>
                  </button>
                </div>

                {pane === "file" ? (
                  <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto p-3">
                    <dl className="flex flex-col gap-1.5 text-xs">
                      <div className="flex justify-between gap-2">
                        <dt className="text-muted">{t("details.kind")}</dt>
                        <dd className="min-w-0 truncate">{t(`filter.${kind}`)}</dd>
                      </div>
                      <div className="flex justify-between gap-2">
                        <dt className="text-muted">{t("details.size")}</dt>
                        <dd className="tabular-nums">{formatSize(file.size)}</dd>
                      </div>
                      <div className="flex justify-between gap-2">
                        <dt className="text-muted">{t("details.added")}</dt>
                        <dd className="tabular-nums">
                          {formatDateTime(file.added_at, i18n.language, "—").slice(0, 10)}
                        </dd>
                      </div>
                    </dl>

                    {file.availability === "local" && (
                      <div className="flex flex-col gap-1.5">
                        <Button
                          variant="quiet"
                          size="sm"
                          disabled={download.isPending}
                          onClick={() =>
                            download.mutate(file.id, {
                              onSuccess: (saved) => {
                                if (saved) toast(t("downloaded"));
                              },
                              onError: (error) => toast(errorText(error)),
                            })
                          }
                        >
                          <Download className="size-4" />
                          {t("download")}
                        </Button>
                        <Button
                          size="sm"
                          disabled={external.isPending}
                          onClick={() => external.mutate(file.id)}
                        >
                          {t("openExternal")}
                        </Button>
                      </div>
                    )}
                  </div>
                ) : file.notes.length === 0 ? (
                  <div className="text-muted px-3 py-2 text-xs">{t("references.none")}</div>
                ) : (
                  <ul className="min-h-0 flex-1 list-none overflow-y-auto p-2">
                    {file.notes.map((note) => (
                      <li key={note.id}>
                        <button
                          type="button"
                          onClick={() => setSelectedNote(note.id)}
                          className={relatedItemVariants({
                            active: selectedNote === note.id,
                          })}
                        >
                          <span className="line-clamp-2 min-w-0 font-semibold">{note.title}</span>
                        </button>
                      </li>
                    ))}
                  </ul>
                )}
              </section>

              <section className="bg-panel flex min-h-0 min-w-0 items-center justify-center overflow-auto">
                {pane === "file" ? (
                  <div className="flex h-full w-full items-center justify-center overflow-auto p-5">
                    {body}
                  </div>
                ) : (
                  <NotePreview
                    noteId={selectedNote}
                    // 狭い幅でも2段に積んで両方見せるので、戻る導線は要らない
                    compact={false}
                    emptyLabel={
                      file.notes.length === 0 ? t("references.none") : t("references.pickNote")
                    }
                    backLabel={t("references.backToList")}
                    openLabel={t("references.openAsMain")}
                    showOpenLabel={false}
                    onBack={() => setPane("file")}
                    onOpen={openNote}
                    onOpenLink={openNote}
                  />
                )}
              </section>
            </div>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}
