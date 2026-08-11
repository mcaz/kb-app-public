import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/atoms/ui/dialog";

import { NameForm } from "./NameForm";

export interface NamePromptDialogProps {
  open: boolean;
  title: string;
  description?: string;
  initialValue: string;
  labels: { save: string; cancel: string };
  onSubmit: (name: string) => void;
  onOpenChange: (open: boolean) => void;
}

/**
 * 名前入力のモーダル(WKWebView では window.prompt が使えないため自前)。
 * ESC・オーバーレイクリックで閉じる、フォーカストラップ、閉じたあとの
 * フォーカス復帰は Dialog 側が持つ — 旧実装ではどれも無かった。
 */
export function NamePromptDialog({
  open,
  title,
  description,
  initialValue,
  labels,
  onSubmit,
  onOpenChange,
}: NamePromptDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="min-w-[300px]">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          {description ? <DialogDescription>{description}</DialogDescription> : null}
        </DialogHeader>
        <NameForm
          initialValue={initialValue}
          ariaLabel={title}
          labels={labels}
          onSubmit={onSubmit}
          onCancel={() => onOpenChange(false)}
        />
      </DialogContent>
    </Dialog>
  );
}
