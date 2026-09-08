import { KbError } from "@/lib/api/error";
import type {
  DecisionInput,
  NoteView,
  ProposalDetailData,
  ProposalListData,
  ProposalMutationData,
  TicketStatus,
  TicketView,
} from "@/lib/api/types";

const created = "2026-09-06T00:10:00Z";

function fixture(index: number, title: string, status: TicketStatus): TicketView {
  const noteId = `notes/proposal-demo-${index}`;
  const reviewed = status !== "review_pending";
  return {
    note_id: noteId,
    note_uid: `DEMO_PROPOSAL_${index}`,
    title,
    status,
    etag: `demo-etag-${index}`,
    current_revision: 1,
    revisions: [
      {
        revision: 1,
        author: "claude-code/claude",
        created_at: created,
        input: {
          title,
          problem:
            index === 1
              ? "日次の提案を会話だけで追うと、レビューと本人の判断が別々の場所に残り、次の行動が分かりにくい。"
              : "既存の手順で気づいた改善点を、根拠と確認条件を揃えて検討したい。",
          proposal:
            index === 1
              ? "改善案を提案チケットとして記録する。レビューを添え、本人が採用・不採用・保留を判断できる場所を用意する。"
              : "対象を絞って小さく試し、結果を確認してから適用範囲を決める。",
          impact:
            "既存のノートの運用は維持する。提案とレビューの版を一致させ、改訂された案は再レビューする。",
          acceptance:
            "最新版へのレビューと判断理由を確認できる。保留には次の行動と再検討条件が残る。判断時に内容が更新されていた場合は再確認できる。",
          scope: "kb-app/proposals",
          tags: ["kb-app", "design"],
        },
      },
    ],
    reviews: reviewed
      ? [
          {
            revision: 1,
            reviewer: "codex-cli/gpt",
            created_at: "2026-09-06T00:25:00Z",
            input: {
              summary:
                "判断する対象と、その根拠を一つの提案で追える。対象を絞って導入する方針は妥当。",
              benefits: "会話を読み返さずに、未解決の提案と次の行動を確認できる。",
              risks: "改訂後の古いレビューを、現在の案の根拠として扱わないこと。",
              alternatives: "会話だけで判断する方法もあるが、履歴を横断して確認する手間が残る。",
              recommendation: status === "rejected" ? "reject" : "approve",
            },
          },
        ]
      : [],
    decisions:
      status === "approved" || status === "rejected" || status === "held"
        ? [
            {
              revision: 1,
              review_count: 1,
              decider: "human/kb-app",
              created_at: "2026-09-06T00:40:00Z",
              input: {
                outcome:
                  status === "approved" ? "approve" : status === "rejected" ? "reject" : "hold",
                reason:
                  status === "held"
                    ? "実際の利用例をもう少し確認してから判断したい。"
                    : status === "approved"
                      ? ""
                      : "レビューと対象範囲を確認して判断した。",
                next_action:
                  status === "held"
                    ? "次にすること: 実例を3件集める\n担当: 自分\n再検討: 次回の週次振り返り"
                    : "",
              },
            },
          ]
        : [],
  };
}

export function demoProposalTickets(): TicketView[] {
  const first = fixture(1, "改善提案のレビューと判断を一か所で追う", "decision_pending");
  first.current_revision = 2;
  const revision = structuredClone(first.revisions[0]!);
  revision.revision = 2;
  revision.created_at = "2026-09-06T01:00:00Z";
  revision.input.impact += " 保留には担当と再検討条件を必須とする。";
  first.revisions.push(revision);
  const review = structuredClone(first.reviews[0]!);
  review.revision = 2;
  review.created_at = "2026-09-06T01:10:00Z";
  first.reviews.push(review);
  return [
    first,
    fixture(2, "日次巡回で見つけた更新候補の根拠を揃える", "review_pending"),
    fixture(3, "週次の振り返りに再検討する提案を含める", "held"),
    fixture(4, "検索結果で参照した根拠を確認しやすくする", "approved"),
    fixture(5, "全ノートに一律で期限を設ける", "rejected"),
  ];
}

const tickets = demoProposalTickets();

function get(note: string) {
  const ticket = tickets.find((item) => item.note_id === note);
  if (!ticket) throw new KbError({ code: "proposal_failed", kind: "not_found" });
  return ticket;
}

// 提案管理からの本人の明示遷移だけに使い、通常検索用notesへは混ぜない。
export function demoProposalNote(note: string): NoteView | undefined {
  const ticket = tickets.find((item) => item.note_id === note);
  const current = ticket?.revisions.at(-1);
  if (!ticket || !current) return undefined;
  const labels: Record<TicketStatus, string> = {
    review_pending: "レビュー待ち",
    decision_pending: "判断待ち",
    approved: "採用",
    rejected: "不採用",
    held: "保留",
  };
  const input = current.input;
  const body = [
    `# ${ticket.title}\n\n提案チケット / 版${ticket.current_revision} / ${labels[ticket.status]}`,
    `## 課題\n\n${input.problem}`,
    `## 提案\n\n${input.proposal}`,
    `## 影響\n\n${input.impact}`,
    `## 完了条件\n\n${input.acceptance}`,
    "採用はこの版に対する判断です。実装・外部操作の実行や完了を意味しません。",
    ...ticket.reviews.map(
      (review) =>
        `## レビュー（版${review.revision}・${review.reviewer}）\n\n${review.input.summary}\n\n利点: ${review.input.benefits}\n\n懸念: ${review.input.risks}\n\n代替案: ${review.input.alternatives}\n\n推奨: ${review.input.recommendation}`,
    ),
    ...ticket.decisions.map(
      (decision) =>
        `## 本人の採否（版${decision.revision}・${decision.created_at}）\n\n${decision.input.outcome}: ${decision.input.reason}\n\n次の確認: ${decision.input.next_action}`,
    ),
  ].join("\n\n");
  return {
    id: ticket.note_id,
    title: ticket.title,
    description: null,
    body: `${body}\n`,
    status: "stable",
    origin: "agent",
    tags: [...input.tags],
    note_uid: ticket.note_uid,
    authority: { namespace: "decisions", role: "proposal", status: "active", scope: input.scope },
    relations: [],
    created_at: ticket.revisions[0]?.created_at ?? null,
    generated_at:
      [...ticket.revisions, ...ticket.reviews, ...ticket.decisions]
        .map((entry) => entry.created_at)
        .sort()
        .at(-1) ?? null,
    related: [],
    similar: [],
    degraded: [],
    vault_root: "(demo)",
  };
}

export const demoProposals = {
  proposalList: (): ProposalListData => ({ tickets: structuredClone(tickets), degraded: [] }),
  proposalGet: (note: string): ProposalDetailData => ({
    ticket: structuredClone(get(note)),
    degraded: [],
  }),
  proposalDecide: (
    note: string,
    expectedEtag: string,
    input: DecisionInput,
  ): ProposalMutationData => {
    const ticket = get(note);
    if (ticket.etag !== expectedEtag) throw new KbError({ code: "proposal_failed", kind: "stale" });
    if (
      ticket.status === "approved" ||
      ticket.status === "rejected" ||
      (input.outcome !== "hold" &&
        !ticket.reviews.some((review) => review.revision === ticket.current_revision))
    )
      throw new KbError({ code: "proposal_failed", kind: "invalid_state" });
    if (input.outcome === "hold" && !input.next_action.trim())
      throw new KbError({ code: "proposal_failed", kind: "invalid_input" });
    ticket.decisions.push({
      revision: ticket.current_revision,
      review_count: ticket.reviews.length,
      input: structuredClone(input),
      decider: "human/kb-app",
      created_at: new Date().toISOString(),
    });
    ticket.status =
      input.outcome === "approve" ? "approved" : input.outcome === "reject" ? "rejected" : "held";
    ticket.etag = `${ticket.etag}:${ticket.decisions.length}`;
    return { ticket: structuredClone(ticket), export_pending: false, degraded: [] };
  },
};
