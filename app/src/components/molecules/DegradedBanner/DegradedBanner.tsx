import { TriangleAlert } from "lucide-react";

import { Icon } from "@/components/atoms/Icon";

export interface DegradedBannerProps {
  messages: string[];
  variant?: "bar" | "card";
}

/** 劣化の告知(原則4: 壊れたら見える)。 */
export function DegradedBanner({ messages, variant = "bar" }: DegradedBannerProps) {
  if (messages.length === 0) return null;
  if (variant === "bar") {
    return (
      <div className="border-line bg-prop-soft text-prop flex items-center gap-2 border-b px-3.5 py-1.5 text-[12.5px]">
        <Icon as={TriangleAlert} size="sm" />
        {messages.join(" / ")}
      </div>
    );
  }
  return (
    <div className="mb-3.5 flex flex-col gap-2">
      {messages.map((m) => (
        <div
          key={m}
          className="bg-prop-soft text-prop flex items-center gap-2 rounded-lg px-3.5 py-2 text-[12.5px]"
        >
          <Icon as={TriangleAlert} size="sm" />
          {m}
        </div>
      ))}
    </div>
  );
}
