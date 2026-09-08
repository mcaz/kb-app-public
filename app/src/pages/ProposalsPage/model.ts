import type { DecisionInput, DecisionOutcome, TicketStatus, TicketView } from "@/lib/api";

export const proposalStatuses: TicketStatus[] = [
  "review_pending",
  "decision_pending",
  "approved",
  "rejected",
  "held",
];
export type ProposalFilter = TicketStatus | "all";

export function proposalUpdatedAt(ticket: TicketView) {
  return (
    [...ticket.revisions, ...ticket.reviews, ...ticket.decisions]
      .map((entry) => entry.created_at)
      .sort((left, right) => Date.parse(left) - Date.parse(right))
      .at(-1) ?? ""
  );
}

export function filterProposals(tickets: TicketView[], filter: ProposalFilter, query: string) {
  const needle = query.trim().toLocaleLowerCase();
  return tickets.filter((ticket) => {
    const current = ticket.revisions.at(-1)?.input;
    return (
      (filter === "all" || ticket.status === filter) &&
      (!needle ||
        `${ticket.title} ${current?.problem ?? ""} ${current?.scope ?? ""}`
          .toLocaleLowerCase()
          .includes(needle))
    );
  });
}

export function canDecide(ticket: TicketView, outcome: DecisionOutcome) {
  if (ticket.status === "approved" || ticket.status === "rejected") return false;
  if (outcome === "hold") return true;
  return (
    (ticket.status === "decision_pending" || ticket.status === "held") &&
    ticket.reviews.some((review) => review.revision === ticket.current_revision)
  );
}

export function decisionFormReady(
  outcome: DecisionOutcome,
  nextAction: string,
  owner: string,
  reconsider: string,
) {
  return (
    outcome !== "hold" || [nextAction, owner, reconsider].every((value) => value.trim().length > 0)
  );
}

export interface DecisionConfirmation {
  readonly note: string;
  readonly noteUid: string;
  readonly expectedEtag: string;
  readonly revision: number;
  readonly title: string;
  readonly input: Readonly<DecisionInput>;
}

/** 確認を開いた後のフォーム編集が、表示済みの判断を差し替えないよう複製する。 */
export function createDecisionConfirmation(
  ticket: TicketView,
  input: DecisionInput,
): DecisionConfirmation {
  return Object.freeze({
    note: ticket.note_id,
    noteUid: ticket.note_uid,
    expectedEtag: ticket.etag,
    revision: ticket.current_revision,
    title: ticket.title,
    input: Object.freeze({ ...input }),
  });
}

export function decisionConfirmationIsCurrent(
  confirmation: DecisionConfirmation,
  ticket: TicketView,
) {
  return (
    confirmation.note === ticket.note_id &&
    confirmation.noteUid === ticket.note_uid &&
    confirmation.expectedEtag === ticket.etag &&
    confirmation.revision === ticket.current_revision &&
    canDecide(ticket, confirmation.input.outcome)
  );
}
