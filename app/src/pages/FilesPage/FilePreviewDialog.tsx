import { convertFileSrc } from "@tauri-apps/api/core";
import { Download, FileQuestion } from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Icon } from "@/components/atoms/Icon";
import { Button } from "@/components/atoms/ui/button";
import { MarkdownView } from "@/components/molecules/MarkdownView";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/atoms/ui/dialog";
import { useErrorText } from "@/hooks/useErrorText";
import { IN_TAURI } from "@/lib/api";
import { useFileDownload, useFileFetch, useFileOpen, useFilePreview } from "@/lib/queries";
import { useSession } from "@/lib/stores/session";

import { fileKind } from "./fileKind";

import type { FileCard } from "@/lib/api";

export interface FilePreviewDialogProps {
  file: FileCard | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function FilePreviewDialog({ file, open, onOpenChange }: FilePreviewDialogProps) {
  const { t } = useTranslation("files");
  const errorText = useErrorText();
  const goToNote = useSession((state) => state.openNote);
  const previewId = open && file?.availability === "local" ? file.id : null;
  const preview = useFilePreview(previewId);
  const external = useFileOpen();
  const download = useFileDownload();
  const fetch = useFileFetch();
  const source = file?.notes[0];
  const kind = file ? fileKind(file) : "other";
  const isMarkdown = file ? /\.(md|markdown)$/iu.test(file.name) : false;
  const isHtml = file
    ? file.media_type.toLowerCase() === "text/html" || /\.html?$/iu.test(file.name)
    : false;
  const src = preview.data?.path ? convertFileSrc(preview.data.path) : null;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className="h-[min(780px,calc(100%-2rem))] max-w-[min(980px,calc(100%-2rem))] grid-rows-[auto_minmax(0,1fr)] gap-0 overflow-hidden p-0"
        showCloseButton
      >
        {file && (
          <>
            <DialogHeader className="border-line min-w-0 border-b px-5 py-3 pr-12">
              <div className="flex min-w-0 items-center gap-3">
                <div className="min-w-0 flex-1">
                  <DialogTitle className="truncate text-[15px]">{file.name}</DialogTitle>
                  <DialogDescription className="truncate text-xs">
                    {source?.title ?? t("noNote")}
                  </DialogDescription>
                </div>
                {source && (
                  <Button
                    variant="quiet"
                    size="sm"
                    onClick={() => {
                      onOpenChange(false);
                      goToNote(source.id);
                    }}
                  >
                    {t("openNote")}
                  </Button>
                )}
                {file.availability === "local" && (
                  <>
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
                  </>
                )}
              </div>
            </DialogHeader>

            <div className="bg-panel flex min-h-0 items-center justify-center overflow-auto p-5">
              {file.availability === "missing" ? (
                <div className="flex flex-col items-center gap-3 text-center">
                  <Icon as={FileQuestion} size="lg" className="text-muted" />
                  <p className="text-muted text-sm">{t("missing")}</p>
                  {file.can_fetch && (
                    <Button
                      disabled={fetch.isPending}
                      onClick={() => fetch.mutate({ id: file.id })}
                    >
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
                    <MarkdownView
                      body={preview.data.text}
                      vaultRoot=""
                      inTauri={false}
                      onOpenNote={goToNote}
                    />
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
              )}
            </div>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}
