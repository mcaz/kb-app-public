import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { PurgeDialog } from "./PurgeDialog";

import type { PurgePlan } from "@/lib/api";

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, values?: Record<string, unknown>) =>
      values && "count" in values ? `${key}:${String(values.count)}` : key,
  }),
}));

const plan = (over: Partial<PurgePlan> = {}): PurgePlan => ({
  token: "t",
  id: "01M0EW60589ZHSZ0HJ6S1VWMYQ",
  display_name: "壊れた.xls",
  hash: "0".repeat(64),
  size: 17920,
  origin: "picker",
  notes: [],
  shares_object_with: [],
  drops_object: true,
  needs_confirmation: false,
  superseded_by: [],
  reason: "画面から取り除いた",
  ...over,
});

describe("PurgeDialog", () => {
  const noop = () => {};

  it("下見が無ければ何も描かない", () => {
    const { container } = render(<PurgeDialog plan={null} onConfirm={noop} onOpenChange={noop} />);
    expect(container).toBeEmptyDOMElement();
  });

  /** 履歴には残るので、回収できる範囲だけを言う(ADR kb-app/artifact-deletion)。 */
  it("取り除ける範囲を必ず示す", () => {
    render(<PurgeDialog plan={plan()} onConfirm={noop} onOpenChange={noop} />);
    expect(screen.getByText("file.purgeScope")).toBeInTheDocument();
  });

  it("実体を道連れにしないときは共有している件数を出す", () => {
    render(
      <PurgeDialog
        plan={plan({ drops_object: false, shares_object_with: ["a", "b"] })}
        onConfirm={noop}
        onOpenChange={noop}
      />,
    );
    expect(screen.getByText("file.purgeShared:2")).toBeInTheDocument();
    expect(screen.queryByText("file.purgeOnlyCopy")).not.toBeInTheDocument();
  });

  /** 会話から受け取った実体は原本が無い。戻せないことを先に言う。 */
  it("原本の無い実体は戻せないと警告する", () => {
    render(
      <PurgeDialog
        plan={plan({ origin: "mcp-content:claude-code/claude", needs_confirmation: true })}
        onConfirm={noop}
        onOpenChange={noop}
      />,
    );
    expect(screen.getByText("file.purgeOnlyCopy")).toBeInTheDocument();
  });

  it("差し替え元として参照されていることを伝える", () => {
    render(
      <PurgeDialog
        plan={plan({ superseded_by: ["01M0EW60589ZHSZ0HJ6S1VWMYR"] })}
        onConfirm={noop}
        onOpenChange={noop}
      />,
    );
    expect(screen.getByText("file.purgeSuperseded")).toBeInTheDocument();
  });
});
