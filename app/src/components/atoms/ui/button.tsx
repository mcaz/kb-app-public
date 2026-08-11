import { Slot } from "radix-ui";
import * as React from "react";
import { tv, type VariantProps } from "tailwind-variants";

/**
 * shadcn/ui の Button を土台に、旧 style.css の button 定義へ合わせたもの
 * (移行で見た目は変えない — ADR-0002)。
 *   button         → variant="default" (枠線 + chip 地)
 *   button.primary → variant="primary" (育つ緑)
 *   button.quiet   → variant="quiet"   (枠線なし・muted)
 *   button.danger  → variant="danger"
 *   button.small   → size="sm"
 */
const buttonVariants = tv({
  base: [
    "inline-flex shrink-0 cursor-pointer items-center justify-center gap-2 whitespace-nowrap",
    "rounded-lg text-[13px] transition-colors outline-none",
    "disabled:pointer-events-none disabled:opacity-35",
    "[&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4",
  ],
  variants: {
    variant: {
      default: "border border-line bg-chip text-ink hover:border-grow",
      primary: "border border-grow bg-grow text-on-accent hover:opacity-90",
      quiet: "border border-transparent bg-transparent text-muted hover:text-ink",
      danger: "border border-danger bg-danger text-white hover:opacity-90",
      ghost: "border border-transparent bg-transparent text-ink hover:bg-sel",
    },
    size: {
      default: "px-3.5 py-1.5",
      sm: "px-2.5 py-[3px] text-xs",
      icon: "size-8 p-0",
    },
  },
  defaultVariants: { variant: "default", size: "default" },
});

export type ButtonProps = React.ComponentProps<"button"> &
  VariantProps<typeof buttonVariants> & { asChild?: boolean };

function Button({ className, variant, size, asChild = false, ...props }: ButtonProps) {
  const Comp = asChild ? Slot.Root : "button";
  return (
    <Comp data-slot="button" className={buttonVariants({ variant, size, className })} {...props} />
  );
}

export { Button, buttonVariants };
