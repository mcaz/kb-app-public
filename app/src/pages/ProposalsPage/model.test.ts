import { describe, expect, it } from "vitest";

import { demoProposalTickets } from "@/lib/demo/proposals";

import {
  canDecide,
  createDecisionConfirmation,
  decisionConfirmationIsCurrent,
  decisionFormReady,
  filterProposals,
  proposalUpdatedAt,
} from "./model";

describe("proposal decision availability", () => {
  it("古い版だけのレビューでは採用・不採用を選べない", () => {
    const ticket = demoProposalTickets()[0]!;
    ticket.reviews = ticket.reviews.filter((review) => review.revision !== ticket.current_revision);
    expect(canDecide(ticket, "approve")).toBe(false);
    expect(canDecide(ticket, "reject")).toBe(false);
    expect(canDecide(ticket, "hold")).toBe(true);
  });

  it("保留は現在のレビュー後に判断でき、決定済みの版は再判断できない", () => {
    const tickets = demoProposalTickets();
    const held = tickets.find((ticket) => ticket.status === "held")!;
    expect(canDecide(held, "approve")).toBe(true);
    held.reviews = [];
    expect(canDecide(held, "approve")).toBe(false);
    for (const ticket of tickets.filter(
      (item) => item.status === "approved" || item.status === "rejected",
    )) {
      expect(canDecide(ticket, "hold")).toBe(false);
    }
  });

  it("保留には次の行動・担当・再検討条件を揃える", () => {
    expect(decisionFormReady("hold", "次の行動", " ", "来週")).toBe(false);
    expect(decisionFormReady("hold", "次の行動", "自分", "来週")).toBe(true);
    expect(decisionFormReady("approve", "", "", "")).toBe(true);
    expect(decisionFormReady("reject", "", "", "")).toBe(true);
  });

  it("状態と検索を同時に適用し、元の提案を変更しない", () => {
    const tickets = demoProposalTickets();
    const filtered = filterProposals(tickets, "held", " 週次 ");
    expect(filtered).toHaveLength(1);
    expect(filtered[0]!.status).toBe("held");
    expect(tickets).toHaveLength(5);
  });

  it("一覧の更新時刻には改訂後のレビューや判断も含む", () => {
    const tickets = demoProposalTickets();
    expect(proposalUpdatedAt(tickets[0]!)).toBe("2026-09-06T01:10:00Z");
    expect(proposalUpdatedAt(tickets[3]!)).toBe("2026-09-06T00:40:00Z");
  });

  it("確認した入力を固定し、別の提案・版・更新後の確定を認めない", () => {
    const ticket = demoProposalTickets()[0]!;
    const input = { outcome: "approve" as const, reason: "", next_action: "" };
    const confirmation = createDecisionConfirmation(ticket, input);
    input.reason = "確認後に入力を変更";
    expect(confirmation.input.reason).toBe("");
    expect(decisionConfirmationIsCurrent(confirmation, ticket)).toBe(true);
    expect(decisionConfirmationIsCurrent(confirmation, { ...ticket, etag: "new-etag" })).toBe(
      false,
    );
    expect(decisionConfirmationIsCurrent(confirmation, { ...ticket, current_revision: 3 })).toBe(
      false,
    );
    expect(decisionConfirmationIsCurrent(confirmation, { ...ticket, note_uid: "different" })).toBe(
      false,
    );
    expect(
      decisionConfirmationIsCurrent(confirmation, { ...ticket, note_id: "notes/different" }),
    ).toBe(false);
  });
});
