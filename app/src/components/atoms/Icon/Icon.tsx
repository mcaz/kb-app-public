import type { LucideIcon } from "lucide-react";
import { type VariantProps } from "tailwind-variants";

import { iconVariants } from "./variants";

export type IconProps = VariantProps<typeof iconVariants> & {
  as: LucideIcon;
  className?: string;
  /** 単体で意味を持つとき(ボタンにラベルが無い等)だけ渡す。既定は装飾扱い。 */
  label?: string;
};

/**
 * lucide のアイコンをこのアプリの見え方に揃える薄い包み。
 *
 * lucide の既定 strokeWidth=2 はこのパレット(紙の地色・細い文字)には太いので
 * 1.75 に落としている。太さとサイズの調整をここ1箇所に閉じ込めるための層。
 */
export function Icon({ as: Glyph, size, className, label }: IconProps) {
  return (
    <Glyph
      className={iconVariants({ size, className })}
      strokeWidth={1.75}
      aria-hidden={label ? undefined : true}
      aria-label={label}
      role={label ? "img" : undefined}
    />
  );
}
