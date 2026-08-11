import { FilePlus, Paperclip } from "lucide-react";
import { useRef } from "react";

import { Icon } from "@/components/atoms/Icon";
import { Button } from "@/components/atoms/ui/button";
import { formatSize } from "@/lib/format";

export interface AttachmentBarProps {
  attachments: [string, number][];
  labels: { label: string; add: string; remove: string };
  onAdd: (files: File[]) => void;
  onRemove: (name: string) => void;
}

/** 添付の一覧と追加(FR-C8)。ドラッグ&ドロップとペーストは画面側が受ける。 */
export function AttachmentBar({ attachments, labels, onAdd, onRemove }: AttachmentBarProps) {
  const fileInput = useRef<HTMLInputElement>(null);

  return (
    <div className="mb-3 flex flex-wrap items-center gap-1.5">
      {attachments.length > 0 && <span className="text-muted text-xs">{labels.label}</span>}
      {attachments.map(([name, size]) => (
        <span
          key={name}
          className="border-line bg-chip inline-flex items-center gap-1.5 rounded-full border px-2 py-0.5 text-xs"
        >
          <Icon as={Paperclip} size="sm" className="text-muted" />
          {name} <i className="text-muted text-[11px] not-italic">{formatSize(size)}</i>
          <button
            type="button"
            className="text-muted hover:text-danger cursor-pointer border-none bg-transparent px-0.5"
            aria-label={`${name} ${labels.remove}`}
            onClick={() => onRemove(name)}
          >
            ×
          </button>
        </span>
      ))}
      <Button variant="quiet" size="sm" onClick={() => fileInput.current?.click()}>
        <Icon as={FilePlus} size="sm" />
        {labels.add}
      </Button>
      <input
        ref={fileInput}
        type="file"
        multiple
        hidden
        onChange={(e) => {
          onAdd(Array.from(e.target.files ?? []));
          e.target.value = "";
        }}
      />
    </div>
  );
}
