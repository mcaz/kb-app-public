import { Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/atoms/ui/button";

export interface PurgeButtonProps {
  onClick: () => void;
  disabled?: boolean;
  className?: string;
}

/**
 * 取り除く操作。**ごみ箱の記号だけで足りる**ので文字は出さない。
 *
 * 名前は `aria-label` と `title` が持つ(読み上げと hover では読める)。
 * 戻せない操作なので危険の色を当てる — 並んでいる他の操作と見分けが付く。
 */
export function PurgeButton({ onClick, disabled = false, className }: PurgeButtonProps) {
  const { t } = useTranslation("files");
  const label = t("purge");

  return (
    <Button
      variant="quiet"
      size="icon"
      aria-label={label}
      title={label}
      disabled={disabled}
      className={`text-danger hover:text-danger ${className ?? ""}`}
      onClick={onClick}
    >
      <Trash2 className="size-4" />
    </Button>
  );
}
