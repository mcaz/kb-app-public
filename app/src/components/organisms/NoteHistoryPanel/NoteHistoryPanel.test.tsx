import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { NoteHistoryPanel } from "./NoteHistoryPanel";

import type { NoteEventView, ProvenanceView } from "@/lib/api";

const provenanceQuery = vi.hoisted(() => ({ data: undefined as ProvenanceView | undefined }));
const historyQuery = vi.hoisted(() => ({ data: undefined as NoteEventView[] | undefined }));

vi.mock("@/lib/queries", () => ({
  useNoteProvenance: () => provenanceQuery,
  useNoteHistory: () => historyQuery,
}));

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, values?: Record<string, unknown>) =>
      values ? `${key}:${Object.values(values).map(String).join(":")}` : key,
    i18n: { language: "ja" },
  }),
}));

const event = (over: Partial<NoteEventView> = {}): NoteEventView => ({
  event_id: "ev-1",
  at: "2026-08-10T05:00:00Z",
  operation: "update",
  kind: "amend",
  actor_label: "codex-cli/gpt-5.6-sol(設定値)",
  model_basis: "config",
  summary: "解約期限を追記",
  reason: null,
  origin_claim: null,
  sections: ["(前文)"],
  changes: [],
  body_diff: null,
  diff_truncated: false,
  ...over,
});

describe("NoteHistoryPanel", () => {
  afterEach(() => {
    cleanup();
    provenanceQuery.data = undefined;
    historyQuery.data = undefined;
  });

  it("1件のイベントを表示する", () => {
    historyQuery.data = [event()];
    render(<NoteHistoryPanel noteId="notes/x" />);

    expect(screen.getByText("notes:history.head")).toBeInTheDocument();
    expect(screen.getByText("解約期限を追記")).toBeInTheDocument();
    expect(screen.getByText("codex-cli/gpt-5.6-sol(設定値)")).toBeInTheDocument();
  });

  it("イベントが無いときは空状態を示す", () => {
    historyQuery.data = [];
    render(<NoteHistoryPanel noteId="notes/x" />);

    expect(screen.getByText("notes:history.empty")).toBeInTheDocument();
    expect(screen.queryByText("解約期限を追記")).not.toBeInTheDocument();
  });

  /** 追加行はgrow、削除行はdanger、それ以外(見出し・文脈行)はmutedにする(design-system)。 */
  it("行を開くと差分の行を追加・削除・その他で色分けする", () => {
    historyQuery.data = [
      event({
        body_diff: "@@ -1,2 +1,3 @@\n context\n-old line\n+new line\n",
      }),
    ];
    render(<NoteHistoryPanel noteId="notes/x" />);

    fireEvent.click(screen.getByText("解約期限を追記"));

    expect(screen.getByText("-old line")).toHaveClass("text-danger");
    expect(screen.getByText("+new line")).toHaveClass("text-grow");
    expect(screen.getByText("context")).toHaveClass("text-muted");
  });

  it("diff_truncatedのときは省略の案内を出す", () => {
    historyQuery.data = [event({ diff_truncated: true })];
    render(<NoteHistoryPanel noteId="notes/x" />);

    fireEvent.click(screen.getByText("解約期限を追記"));

    expect(screen.getByText("notes:history.diffTruncated")).toBeInTheDocument();
  });

  /** removeは破壊的操作としてdangerにする(design-system: 緑=正常・琥珀=提案・赤=破壊)。 */
  it("kindがremoveの行はdangerで示す", () => {
    historyQuery.data = [
      event({ event_id: "ev-remove", kind: "remove", summary: "統合により削除" }),
    ];
    const { container } = render(<NoteHistoryPanel noteId="notes/x" />);

    const dangerElements = container.querySelectorAll(".text-danger");
    expect(dangerElements).toHaveLength(1);
    expect(dangerElements[0]).toHaveTextContent("notes:history.kind.remove");
  });
});
