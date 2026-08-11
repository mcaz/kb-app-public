import { Wrench } from "lucide-react";

import { Icon } from "@/components/atoms/Icon";
import { Button } from "@/components/atoms/ui/button";
import type { CareProposal } from "@/lib/api";

export interface CareBarProps {
  proposal: CareProposal;
  confirmLabel: string;
  onDismiss: () => void;
}

/** お手入れの提案(ノートの場で「確認した」を押す — 受信箱を廃した形)。 */
export function CareBar({ proposal, confirmLabel, onDismiss }: CareBarProps) {
  return (
    <div className="border-l-prop bg-prop-soft text-ink mb-2.5 flex max-w-[46em] flex-wrap items-center gap-2.5 rounded-lg border-l-[3px] px-3.5 py-2 text-[13px]">
      <Icon as={Wrench} size="sm" className="text-prop" />
      {proposal.detail}
      <Button variant="quiet" size="sm" onClick={onDismiss}>
        {confirmLabel}
      </Button>
    </div>
  );
}
