import { FilePlus, Paperclip } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";
import { StatusPill } from "@/components/atoms/StatusPill";
import { Button } from "@/components/atoms/ui/button";
import { formatSize } from "@/lib/format";
import { useNoteFiles } from "@/lib/queries";

import { FileRow } from "./FileRow";
import { useFileActions } from "./useFileActions";
import { fileRowVariants } from "./variants";

export interface FilePanelProps {
  noteId: string;
}

/**
 * ノート内のファイル欄(旧 AttachmentBar)。
 *
 * 「削除」という語は使わない — 実体を消す仕組みが無いので、消えると言えば嘘になる
 * (ADR-0003 決定7)。行の操作は「このノートから外す」までにしてある。
 */
export function FilePanel({ noteId }: FilePanelProps) {
  const { t } = useTranslation("notes");
  const { data } = useNoteFiles(noteId);
  const { addPicked, detachFile, fetchFile, openFile, openLegacyFile, busy } =
    useFileActions(noteId);

  const files = data?.files ?? [];
  const legacy = data?.legacy ?? [];

  return (
    // 本文と同じ 46em の段に収める。ノートの列は内容の幅で決まる(横スクロールする
    // レイアウト)ので、上限を置かないと行のボタンの分だけ列が広がる。
    // em の基準を本文と揃えるため font-size もここで決める
    <section className="mb-3 flex max-w-[46em] min-w-0 flex-col gap-1.5 text-[13px]">
      {(files.length > 0 || legacy.length > 0) && (
        <span className="text-muted text-xs">{t("file.label")}</span>
      )}

      <ul className="flex min-w-0 list-none flex-col gap-1 p-0">
        {files.map((file) => (
          <FileRow
            key={file.id}
            file={file}
            busy={busy}
            onDetach={() => void detachFile(file)}
            onReplace={() => void addPicked(file.id)}
            onFetch={() => void fetchFile(file)}
            onOpen={() => void openFile(file)}
          />
        ))}

        {/*
          移行前の添付。台帳に載っていないので状態も版も無く、操作は出さない
          (読み取り専用の legacy transport — 決定4)
        */}
        {legacy.map((file) => (
          <li key={file.name} className={fileRowVariants({ state: "legacy" })}>
            <Icon as={Paperclip} size="sm" className="text-muted" />
            <button
              type="button"
              className="hover:text-grow min-w-0 cursor-pointer border-none bg-transparent p-0 text-left break-all"
              onClick={() => void openLegacyFile(file.name)}
            >
              {file.name}
            </button>
            <i className="text-muted text-[11px] not-italic">{formatSize(file.size)}</i>
            <StatusPill>{t("file.legacy")}</StatusPill>
          </li>
        ))}
      </ul>

      <div>
        <Button variant="quiet" size="sm" disabled={busy} onClick={() => void addPicked()}>
          <Icon as={FilePlus} size="sm" />
          {t("file.add")}
        </Button>
      </div>
    </section>
  );
}
