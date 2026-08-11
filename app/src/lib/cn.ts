import { twMerge } from "tailwind-merge";

/**
 * クラス結合。shadcn 由来の部品が `cn` を前提にしているため用意している。
 * 自前のコンポーネントでは原則 tailwind-variants(tv)を使い、
 * 呼び出し側からの上書きは tv の返り値に渡すこと(ADR-0002)。
 */
export function cn(...inputs: (string | false | null | undefined)[]): string {
  return twMerge(inputs.filter(Boolean).join(" "));
}
