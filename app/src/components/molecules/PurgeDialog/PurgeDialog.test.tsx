import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

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
  origin: "picker",
  notes: [],
  refs: [],
  shares_object_with: [],
  needs_confirmation: false,
  superseded_by: [],
  ...over,
});

describe("PurgeDialog", () => {
  const noop = () => {};
  /** 画面が渡す流れの見本。ダイアログは target だけを描く。 */
  const flow = (target: PurgePlan | null) => ({
    target,
    busy: false,
    confirmPurge: noop,
    clear: noop,
  });

  // ダイアログは portal で body へ出る。片付けないと前のテストが次に見える
  afterEach(cleanup);

  it("下見が無ければ何も描かない", () => {
    const { container } = render(<PurgeDialog flow={flow(null)} />);
    expect(container).toBeEmptyDOMElement();
  });

  /** 履歴には残るので、回収できる範囲だけを言う(ADR kb-app/artifact-deletion)。 */
  it("取り除ける範囲を必ず示す", () => {
    render(<PurgeDialog flow={flow(plan())} />);
    expect(screen.getByText("file.purgeScope")).toBeInTheDocument();
  });

  it("実体を道連れにしないときは共有している件数を出す", () => {
    render(<PurgeDialog flow={flow(plan({ shares_object_with: ["a", "b"] }))} />);
    expect(screen.getByText("file.purgeShared:2")).toBeInTheDocument();
    expect(screen.queryByText("file.purgeOnlyCopy")).not.toBeInTheDocument();
  });

  /** 会話から受け取った実体は原本が無い。戻せないことを先に言う。 */
  it("原本の無い実体は戻せないと警告する", () => {
    render(
      <PurgeDialog
        flow={flow(plan({ origin: "mcp-content:claude-code/claude", needs_confirmation: true }))}
      />,
    );
    expect(screen.getByText("file.purgeOnlyCopy")).toBeInTheDocument();
  });

  /** 一覧からは参照中でも押せる。消すとノートからファイルが消えることを先に言う。 */
  it("まだノートが持っているときは件数を出す", () => {
    render(<PurgeDialog flow={flow(plan({ notes: ["notes/a", "notes/b"] }))} />);
    expect(screen.getByText("file.purgeStillUsed:2")).toBeInTheDocument();
  });

  it("どのノートからも外れていれば参照の警告は出さない", () => {
    render(<PurgeDialog flow={flow(plan())} />);
    expect(screen.queryByText(/purgeStillUsed/)).not.toBeInTheDocument();
  });

  it("一緒に外れる参照名を伝える", () => {
    render(<PurgeDialog flow={flow(plan({ refs: ["sheet"] }))} />);
    expect(screen.getByText("file.purgeDropsRefs:1")).toBeInTheDocument();
  });

  it("差し替え元として参照されていることを伝える", () => {
    render(<PurgeDialog flow={flow(plan({ superseded_by: ["01M0EW60589ZHSZ0HJ6S1VWMYR"] }))} />);
    expect(screen.getByText("file.purgeSuperseded")).toBeInTheDocument();
  });
});
