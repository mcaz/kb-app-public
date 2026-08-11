import { type VariantProps } from "tailwind-variants";

import { statusPillVariants } from "./variants";

export type StatusPillProps = VariantProps<typeof statusPillVariants> & {
  children: React.ReactNode;
  className?: string;
};

/** 状態を表す小さな丸ピル(旧 .status-pill / .state)。 */
export function StatusPill({ tone, className, children }: StatusPillProps) {
  return <span className={statusPillVariants({ tone, className })}>{children}</span>;
}
