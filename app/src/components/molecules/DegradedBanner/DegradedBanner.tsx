import { TriangleAlert } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";

import type { Degradation } from "@/lib/api";

export interface DegradedBannerProps {
  items: Degradation[];
  variant?: "bar" | "card" | "inline";
}

/** 劣化の告知(原則4: 壊れたら見える)。 */
export function DegradedBanner({ items, variant = "bar" }: DegradedBannerProps) {
  const { t } = useTranslation("common");
  if (items.length === 0) return null;
  const entries = [
    ...new Map(
      items.map((item) => {
        const key = JSON.stringify(item);
        const text =
          item.code === "embedding_index_pending"
            ? t(`degradation.${item.code}`, { remaining: item.remaining })
            : "note" in item
              ? t(`degradation.${item.code}`, { note: item.note, detail: item.detail })
              : t(`degradation.${item.code}`, { detail: item.detail });
        return [key, { key, text }] as const;
      }),
    ).values(),
  ];
  if (variant === "bar") {
    return (
      <div className="border-line bg-prop-soft text-prop flex items-center gap-2 border-b px-3.5 py-1.5 text-[12.5px]">
        <Icon as={TriangleAlert} size="sm" />
        {entries.map((entry) => entry.text).join(" / ")}
      </div>
    );
  }
  if (variant === "inline") {
    return (
      <div
        className="border-prop bg-prop-soft text-prop mx-3 mt-2 rounded-md border px-2.5 py-1.5 text-xs"
        role="status"
      >
        {entries.map((entry) => entry.text).join(" / ")}
      </div>
    );
  }
  return (
    <div className="mb-3.5 flex flex-col gap-2">
      {entries.map((entry, index) => (
        <div
          key={`${entry.key}:${index}`}
          className="bg-prop-soft text-prop flex items-center gap-2 rounded-lg px-3.5 py-2 text-[12.5px]"
        >
          <Icon as={TriangleAlert} size="sm" />
          {entry.text}
        </div>
      ))}
    </div>
  );
}
