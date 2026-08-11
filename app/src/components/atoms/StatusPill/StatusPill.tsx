import { tv, type VariantProps } from "tailwind-variants";

const pill = tv({
  base: "rounded-full px-2.5 py-px text-[11px] whitespace-nowrap",
  variants: {
    tone: {
      muted: "border border-line bg-panel-2 text-muted",
      prop: "bg-prop-soft text-prop",
      grow: "bg-grow-soft text-grow",
    },
  },
  defaultVariants: { tone: "muted" },
});

export type StatusPillProps = VariantProps<typeof pill> & {
  children: React.ReactNode;
  className?: string;
};

/** 状態を表す小さな丸ピル(旧 .status-pill / .state)。 */
export function StatusPill({ tone, className, children }: StatusPillProps) {
  return <span className={pill({ tone, className })}>{children}</span>;
}
