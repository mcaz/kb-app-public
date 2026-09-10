import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ActivityFeed } from "./ActivityFeed";

import type { ActivityFeedView, ActivityFilter, ActivityRowView } from "@/lib/api";

const rows: ActivityRowView[] = [
  {
    event_id: "ev-a",
    note_id: "notes/a",
    title: "ノートA",
    at: "2026-08-10T05:00:00Z",
    actor_label: "codex-cli/gpt-5.6-sol(設定値)",
    operation: "update",
    kind: "amend",
    summary: "更新1",
    section_count: 1,
  },
  {
    event_id: "ev-b",
    note_id: "notes/b",
    title: "ノートB",
    at: "2026-08-09T05:00:00Z",
    actor_label: "claude-code/claude-fable-5-1(自己申告)",
    operation: "propose",
    kind: "create",
    summary: null,
    section_count: 1,
  },
];

/** 実装(kb_core::provenance::activity_feed)と同じく、絞り込みは行だけに効く。 */
function buildFeed(filter: ActivityFilter): ActivityFeedView {
  const client = (row: ActivityRowView) => row.actor_label.split("/")[0];
  const filtered = rows
    .filter((row) => !filter.client || client(row) === filter.client)
    .filter((row) => !filter.kind || row.kind === filter.kind);
  return {
    rows: filtered,
    summary: { last_7_days: rows.length, distinct_actors: 2, unknown_model_ratio: 0 },
    clients: ["claude-code", "codex-cli"],
    models: ["claude-fable-5-1", "gpt-5.6-sol"],
  };
}

const openNote = vi.fn();

vi.mock("@/lib/stores/session", () => ({
  useSession: (selector: (state: { openNote: typeof openNote }) => unknown) =>
    selector({ openNote }),
}));

vi.mock("@/lib/queries", () => ({
  useActivityFeed: (_limit: number, filter: ActivityFilter) => ({ data: buildFeed(filter) }),
}));

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, values?: Record<string, unknown>) =>
      values ? `${key}:${Object.values(values).map(String).join(":")}` : key,
    i18n: { language: "ja" },
  }),
}));

describe("ActivityFeed", () => {
  afterEach(() => {
    cleanup();
    openNote.mockReset();
  });

  it("既定ではすべての書き手の行を表示する", () => {
    render(<ActivityFeed />);

    expect(screen.getByText("ノートA")).toBeInTheDocument();
    expect(screen.getByText("ノートB")).toBeInTheDocument();
  });

  it("書き手フィルタを選ぶと行が絞れる", () => {
    render(<ActivityFeed />);

    fireEvent.change(screen.getByRole("combobox", { name: "home:activity.filter.client" }), {
      target: { value: "codex-cli" },
    });

    expect(screen.getByText("ノートA")).toBeInTheDocument();
    expect(screen.queryByText("ノートB")).not.toBeInTheDocument();
  });

  it("種別フィルタを選ぶと行が絞れる", () => {
    render(<ActivityFeed />);

    fireEvent.change(screen.getByRole("combobox", { name: "home:activity.filter.kind" }), {
      target: { value: "create" },
    });

    expect(screen.queryByText("ノートA")).not.toBeInTheDocument();
    expect(screen.getByText("ノートB")).toBeInTheDocument();
  });

  it("行クリックでそのノートを開く", () => {
    render(<ActivityFeed />);

    fireEvent.click(screen.getByText("ノートA"));

    expect(openNote).toHaveBeenCalledWith("notes/a");
  });

  it("絞り込んで0件になったら空状態を示す", () => {
    render(<ActivityFeed />);

    fireEvent.change(screen.getByRole("combobox", { name: "home:activity.filter.client" }), {
      target: { value: "codex-cli" },
    });
    fireEvent.change(screen.getByRole("combobox", { name: "home:activity.filter.kind" }), {
      target: { value: "create" },
    });

    expect(screen.getByText("home:activity.empty")).toBeInTheDocument();
  });
});
