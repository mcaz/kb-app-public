import { useState } from "react";

import { Button } from "@/components/atoms/ui/button";
import { DialogFooter } from "@/components/atoms/ui/dialog";

export interface NameFormProps {
  initialValue: string;
  ariaLabel: string;
  labels: { save: string; cancel: string };
  onSubmit: (name: string) => void;
  onCancel: () => void;
}

/**
 * NamePromptDialog 私物。開くたびに新しくマウントされるので、
 * 初期値の同期を effect でやらなくて済む(状態の巻き戻しが要らない形にする)。
 */
export function NameForm({ initialValue, ariaLabel, labels, onSubmit, onCancel }: NameFormProps) {
  const [value, setValue] = useState(initialValue);
  const submit = () => {
    const name = value.trim();
    if (name) onSubmit(name);
  };

  return (
    <>
      <input
        className="border-line bg-chip text-ink rounded-md border px-2.5 py-1.5"
        value={value}
        aria-label={ariaLabel}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            submit();
          }
        }}
      />
      <DialogFooter>
        <Button variant="quiet" onClick={onCancel}>
          {labels.cancel}
        </Button>
        <Button variant="primary" disabled={value.trim() === ""} onClick={submit}>
          {labels.save}
        </Button>
      </DialogFooter>
    </>
  );
}
