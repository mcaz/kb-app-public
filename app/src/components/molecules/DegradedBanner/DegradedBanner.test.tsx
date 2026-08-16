import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { DegradedBanner } from "./DegradedBanner";

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, values: Record<string, string>) =>
      `${key}:${values.note ?? ""}:${values.detail ?? ""}`,
  }),
}));

describe("DegradedBanner", () => {
  it("Note IDを翻訳へ渡し、同じ劣化を重複表示しない", () => {
    const item = {
      code: "index_parse" as const,
      note: "notes/broken",
      detail: "fixture",
    };
    render(<DegradedBanner items={[item, item]} variant="inline" />);

    expect(screen.getByRole("status")).toHaveTextContent(
      "degradation.index_parse:notes/broken:fixture",
    );
    expect(screen.getByRole("status").textContent?.match(/notes\/broken/g)).toHaveLength(1);
  });
});
