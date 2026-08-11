import type { LucideIcon } from "lucide-react";
import { tv, type VariantProps } from "tailwind-variants";

const icon = tv({
  base: "shrink-0",
  variants: {
    size: {
      /** 一覧など密な行(現行の 11.5〜12px の文字に釣り合う) */
      sm: "size-3.5",
      /** ナビ・本文の既定 */
      md: "size-4",
      /** ホームのタイルなど */
      lg: "size-5",
    },
  },
  defaultVariants: { size: "md" },
});

export type IconProps = VariantProps<typeof icon> & {
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
      className={icon({ size, className })}
      strokeWidth={1.75}
      aria-hidden={label ? undefined : true}
      aria-label={label}
      role={label ? "img" : undefined}
    />
  );
}
