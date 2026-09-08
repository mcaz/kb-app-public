import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { setupI18n } from "@/i18n";
import { demoProposalTickets } from "@/lib/demo/proposals";
import type { ProposalMutationData, TicketView } from "@/lib/api";

import { ProposalDetail } from "./ProposalDetail";

const { mutateAsync } = vi.hoisted(() => ({ mutateAsync: vi.fn() }));
vi.mock("@/lib/queries", () => ({
  useDecideProposal: () => ({ mutateAsync, isPending: false, error: null }),
}));
vi.mock("sonner", () => ({ toast: { success: vi.fn(), warning: vi.fn(), error: vi.fn() } }));

beforeEach(() => {
  setupI18n("ja");
  mutateAsync.mockReset();
});
afterEach(cleanup);

const show = (ticket: TicketView) =>
  render(<ProposalDetail ticket={ticket} date={(value) => value} request={() => {}} />);

// 2026-09-06: 確認を開く操作と確定を混同すると本人が未確定の判断を保存してしまう。
describe("proposal decision confirmation boundary", () => {
  it("理由なしで確認できるが、開く・戻る・Escapeでは保存しない", async () => {
    show(demoProposalTickets()[0]!);
    expect(screen.getByRole("textbox", { name: "判断の理由（任意）" })).not.toBeRequired();
    fireEvent.click(screen.getByRole("button", { name: "この判断を記録" }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText("理由の記載なし")).toBeInTheDocument();
    expect(within(dialog).getByText("第2版")).toBeInTheDocument();
    expect(within(dialog).getByText("改善提案のレビューと判断を一か所で追う")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "戻る" })).toHaveFocus();
    expect(mutateAsync).not.toHaveBeenCalled();
    fireEvent.click(within(dialog).getByRole("button", { name: "戻る" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "この判断を記録" }));
    await screen.findByRole("dialog");
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(mutateAsync).not.toHaveBeenCalled();
  });

  it("確定だけが表示済みのsnapshotを一回送信し、送信中の二重確定を防ぐ", async () => {
    const ticket = demoProposalTickets()[0]!;
    let finish: ((value: ProposalMutationData) => void) | undefined;
    const pending = new Promise<ProposalMutationData>((resolve) => {
      finish = resolve;
    });
    mutateAsync.mockReturnValue(pending);
    const view = show(ticket);
    const reason = screen.getByRole("textbox", { name: "判断の理由（任意）" });
    fireEvent.change(reason, { target: { value: "確認した理由" } });
    fireEvent.click(screen.getByRole("button", { name: "この判断を記録" }));
    const dialog = await screen.findByRole("dialog");
    // 確認中の外部入力でも、モーダルに表示済みの値を差し替えない。
    fireEvent.change(reason, { target: { value: "確認後に変わった理由" } });
    const confirm = within(dialog).getByRole("button", { name: "確定して記録" });
    act(() => {
      fireEvent.click(confirm);
      fireEvent.click(confirm);
    });
    expect(mutateAsync).toHaveBeenCalledTimes(1);
    expect(mutateAsync).toHaveBeenCalledWith({
      note: ticket.note_id,
      expectedEtag: ticket.etag,
      input: { outcome: "approve", reason: "確認した理由", next_action: "" },
    });
    expect(within(dialog).getByRole("button", { name: "戻る" })).toBeDisabled();
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    const updated = { ...ticket, etag: "updated-while-saving" };
    view.rerender(<ProposalDetail ticket={updated} date={(value) => value} request={() => {}} />);
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "戻る" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "記録中…" })).toBeDisabled();
    expect(mutateAsync).toHaveBeenCalledTimes(1);
    await act(async () => {
      finish?.({ ticket: updated, export_pending: false, degraded: [] });
      await pending;
    });
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });

  it("確認中にetagが更新されたら確認を解除し、新しい確認を必要とする", async () => {
    const ticket = demoProposalTickets()[0]!;
    const updated = { ...ticket, etag: "changed-by-review" };
    mutateAsync.mockResolvedValue({ ticket: updated, export_pending: false, degraded: [] });
    const view = show(ticket);
    fireEvent.change(screen.getByRole("textbox", { name: "判断の理由（任意）" }), {
      target: { value: "入力中の理由" },
    });
    fireEvent.click(screen.getByRole("button", { name: "この判断を記録" }));
    await screen.findByRole("dialog");
    view.rerender(<ProposalDetail ticket={updated} date={(value) => value} request={() => {}} />);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(mutateAsync).not.toHaveBeenCalled();
    expect(screen.getByRole("textbox", { name: "判断の理由（任意）" })).toHaveValue("入力中の理由");
    expect(screen.getByRole("alert")).toHaveTextContent("提案が更新されました");
    fireEvent.click(screen.getByRole("button", { name: "この判断を記録" }));
    const dialog = await screen.findByRole("dialog");
    expect(mutateAsync).not.toHaveBeenCalled();
    fireEvent.click(within(dialog).getByRole("button", { name: "確定して記録" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(mutateAsync).toHaveBeenCalledExactlyOnceWith({
      note: ticket.note_id,
      expectedEtag: updated.etag,
      input: { outcome: "approve", reason: "入力中の理由", next_action: "" },
    });
  });

  // 2026-09-06: etagをkeyにすると自動更新で理由・保留条件と入力中のフォーカスが失われる。
  it("自動更新では保留の草稿とフォーカスを保ち、別の提案へ移ると初期化する", () => {
    const ticket = demoProposalTickets()[0]!;
    const view = show(ticket);
    fireEvent.click(screen.getByRole("button", { name: "保留" }));
    const fields = [
      ["判断の理由（任意）", "検討中の理由"],
      ["次にすること", "実例を集める"],
      ["担当", "自分"],
      ["再検討する条件・時期", "来週"],
    ] as const;
    for (const [name, value] of fields) {
      fireEvent.change(screen.getByRole("textbox", { name }), { target: { value } });
    }
    const focused = screen.getByRole("textbox", { name: "次にすること" });
    focused.focus();
    view.rerender(
      <ProposalDetail
        ticket={{ ...ticket, etag: "changed-by-review" }}
        date={(value) => value}
        request={() => {}}
      />,
    );
    for (const [name, value] of fields) {
      expect(screen.getByRole("textbox", { name })).toHaveValue(value);
    }
    expect(focused).toHaveFocus();
    expect(screen.getByRole("button", { name: "保留" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("alert")).toHaveTextContent("提案が更新されました");
    view.rerender(
      <ProposalDetail
        ticket={demoProposalTickets()[1]!}
        date={(value) => value}
        request={() => {}}
      />,
    );
    for (const [name] of fields) {
      expect(screen.getByRole("textbox", { name })).toHaveValue("");
    }
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("改訂された提案では古い確認を閉じ、最新版のレビューと新しい確認を必要とする", async () => {
    const ticket = demoProposalTickets()[0]!;
    const view = show(ticket);
    fireEvent.change(screen.getByRole("textbox", { name: "判断の理由（任意）" }), {
      target: { value: "前の版を読んだ理由" },
    });
    fireEvent.click(screen.getByRole("button", { name: "この判断を記録" }));
    await screen.findByRole("dialog");
    const revised: TicketView = {
      ...ticket,
      title: "対象範囲を狭めた提案",
      etag: "changed-by-revision",
      current_revision: 3,
      status: "review_pending",
      revisions: [
        ...ticket.revisions,
        {
          ...ticket.revisions.at(-1)!,
          revision: 3,
          input: { ...ticket.revisions.at(-1)!.input, title: "対象範囲を狭めた提案" },
        },
      ],
    };
    const renderTicket = (value: TicketView) =>
      view.rerender(<ProposalDetail ticket={value} date={(date) => date} request={() => {}} />);
    renderTicket(revised);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "判断の理由（任意）" })).toHaveValue(
      "前の版を読んだ理由",
    );
    expect(screen.getByRole("button", { name: "この判断を記録" })).toBeDisabled();
    const reviewed: TicketView = {
      ...revised,
      etag: "revision-reviewed",
      status: "decision_pending",
      reviews: [...revised.reviews, { ...revised.reviews.at(-1)!, revision: 3 }],
    };
    renderTicket(reviewed);
    expect(screen.getByRole("alert")).toHaveTextContent("提案が更新されました");
    fireEvent.click(screen.getByRole("button", { name: "この判断を記録" }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText("第3版")).toBeInTheDocument();
    expect(within(dialog).getByText("対象範囲を狭めた提案")).toBeInTheDocument();
    expect(within(dialog).getByText("前の版を読んだ理由")).toBeInTheDocument();
    expect(mutateAsync).not.toHaveBeenCalled();
    mutateAsync.mockResolvedValue({ ticket: reviewed, export_pending: false, degraded: [] });
    fireEvent.click(within(dialog).getByRole("button", { name: "確定して記録" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(mutateAsync).toHaveBeenCalledExactlyOnceWith({
      note: ticket.note_id,
      expectedEtag: reviewed.etag,
      input: { outcome: "approve", reason: "前の版を読んだ理由", next_action: "" },
    });
  });

  it.each(["saved", "export_pending", "failed"] as const)(
    "保留の草稿は保存成功時だけ初期化する（%s）",
    async (result) => {
      const ticket = demoProposalTickets()[1]!;
      if (result === "failed") mutateAsync.mockRejectedValue(new Error("save failed"));
      else
        mutateAsync.mockResolvedValue({
          ticket,
          export_pending: result === "export_pending",
          degraded: [],
        });
      show(ticket);
      const fields = [
        ["判断の理由（任意）", "検討中の理由"],
        ["次にすること", "実例を集める"],
        ["担当", "自分"],
        ["再検討する条件・時期", "来週"],
      ] as const;
      for (const [name, value] of fields) {
        fireEvent.change(screen.getByRole("textbox", { name }), { target: { value } });
      }
      fireEvent.click(screen.getByRole("button", { name: "この判断を記録" }));
      const dialog = await screen.findByRole("dialog");
      fireEvent.click(within(dialog).getByRole("button", { name: "確定して記録" }));
      await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
      for (const [name, value] of fields) {
        expect(screen.getByRole("textbox", { name })).toHaveValue(result === "failed" ? value : "");
      }
    },
  );

  it.each(["approve", "reject"] as const)(
    "%sを理由なしで確定すると空の理由を送信する",
    async (outcome) => {
      const ticket = demoProposalTickets()[0]!;
      mutateAsync.mockResolvedValue({ ticket, export_pending: false, degraded: [] });
      show(ticket);
      if (outcome === "reject") fireEvent.click(screen.getByRole("button", { name: "不採用" }));
      fireEvent.click(screen.getByRole("button", { name: "この判断を記録" }));
      const dialog = await screen.findByRole("dialog");
      expect(mutateAsync).not.toHaveBeenCalled();
      fireEvent.click(within(dialog).getByRole("button", { name: "確定して記録" }));
      await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
      expect(mutateAsync).toHaveBeenCalledExactlyOnceWith({
        note: ticket.note_id,
        expectedEtag: ticket.etag,
        input: { outcome, reason: "", next_action: "" },
      });
    },
  );

  it("理由を付けなかった過去の判断も履歴に表示する", () => {
    show(demoProposalTickets()[3]!);
    expect(screen.getByText("理由の記載なし")).toBeInTheDocument();
    expect(mutateAsync).not.toHaveBeenCalled();
  });

  it("保留は理由が空でも次の行動・担当・条件を確認し、欠落時は確認を開かない", async () => {
    show(demoProposalTickets()[1]!);
    const record = screen.getByRole("button", { name: "この判断を記録" });
    expect(record).toBeDisabled();
    fireEvent.change(screen.getByRole("textbox", { name: "次にすること" }), {
      target: { value: "実例を集める" },
    });
    fireEvent.change(screen.getByRole("textbox", { name: "担当" }), {
      target: { value: "自分" },
    });
    expect(record).toBeDisabled();
    fireEvent.change(screen.getByRole("textbox", { name: "再検討する条件・時期" }), {
      target: { value: "来週" },
    });
    fireEvent.click(record);
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText("理由の記載なし")).toBeInTheDocument();
    expect(within(dialog).getByText(/実例を集める/)).toHaveTextContent("担当: 自分");
    expect(within(dialog).getByText(/実例を集める/)).toHaveTextContent(
      "再検討する条件・時期: 来週",
    );
    expect(mutateAsync).not.toHaveBeenCalled();
  });
});
