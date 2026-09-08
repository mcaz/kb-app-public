import { ClipboardCheck, FileText, MessageSquare, Pencil } from "lucide-react";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Button } from "@/components/atoms/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/atoms/ui/dialog";
import { useErrorText } from "@/hooks/useErrorText";
import { useDecideProposal } from "@/lib/queries";
import { useSession } from "@/lib/stores/session";
import type { DecisionOutcome, TicketView } from "@/lib/api";

import {
  canDecide,
  createDecisionConfirmation,
  decisionConfirmationIsCurrent,
  decisionFormReady,
  type DecisionConfirmation,
} from "./model";
import {
  proposalChangedVariants,
  proposalFieldClass,
  proposalFilterVariants,
  proposalStatusVariants,
} from "./variants";

export function ProposalDetail({
  ticket,
  date,
  request,
}: {
  ticket: TicketView;
  date: (value: string) => string;
  request: (kind: "review" | "revise", ticket: TicketView) => void;
}) {
  const { t } = useTranslation("proposals");
  const openNote = useSession((state) => state.openNote);
  const current = ticket.revisions.at(-1);
  const terminal = ticket.status === "approved" || ticket.status === "rejected";
  return (
    <article className="min-w-0">
      <header>
        <div className="mb-3 flex flex-wrap items-center gap-2">
          <span className={proposalStatusVariants({ status: ticket.status })}>
            {t(`status.${ticket.status}`)}
          </span>
          <span className="text-muted text-xs">
            {t("revision", { revision: ticket.current_revision })}
          </span>
        </div>
        <h1 className="text-[26px] leading-snug font-medium break-words">{ticket.title}</h1>
        <p className="text-muted mt-2 text-xs">
          {current ? t("by", { actor: current.author, date: date(current.created_at) }) : ""}
        </p>
        <div className="mt-5 flex flex-wrap gap-2">
          <Button onClick={() => openNote(ticket.note_id)}>
            <FileText />
            {t("openNote")}
          </Button>
          {!terminal && (
            <Button onClick={() => request("review", ticket)}>
              <MessageSquare />
              {t("requestReview")}
            </Button>
          )}
          <Button onClick={() => request("revise", ticket)}>
            <Pencil />
            {t("requestRevision")}
          </Button>
        </div>
      </header>
      <div className="mt-7 grid grid-cols-[minmax(0,1fr)_300px] gap-7 max-[1000px]:grid-cols-1">
        <div className="min-w-0 space-y-7">
          {current && (
            <section className="border-line bg-panel-2/40 space-y-5 rounded-xl border p-5">
              {(["problem", "proposal", "impact", "acceptance", "scope"] as const).map((field) => (
                <div key={field}>
                  <h2 className="text-muted mb-2 text-xs font-semibold">{t(`fields.${field}`)}</h2>
                  <p className="text-sm leading-7 break-words whitespace-pre-wrap">
                    {current.input[field]}
                  </p>
                </div>
              ))}
            </section>
          )}
          <section>
            <h2 className="mb-3 flex items-center gap-2 text-base font-medium">
              <MessageSquare className="size-4" />
              {t("reviews")}
              <span className="text-muted text-xs">{ticket.reviews.length}</span>
            </h2>
            {ticket.reviews.length === 0 ? (
              <p className="text-muted border-line rounded-xl border border-dashed p-5 text-sm">
                {t("noReviews")}
              </p>
            ) : (
              <div className="space-y-3">
                {[...ticket.reviews].reverse().map((review, index) => (
                  <div
                    key={`${review.created_at}:${index}`}
                    className="border-line rounded-xl border p-5"
                  >
                    <div className="flex flex-wrap items-center justify-between gap-2">
                      <span className="text-xs font-semibold">
                        {t(`recommendation.${review.input.recommendation}`)}
                      </span>
                      <span className="text-muted text-[11px]">
                        {t(
                          review.revision === ticket.current_revision
                            ? "currentReview"
                            : "pastReview",
                        )}{" "}
                        · {t("revision", { revision: review.revision })}
                      </span>
                    </div>
                    <p className="text-muted mt-2 text-[11px]">
                      {t("by", { actor: review.reviewer, date: date(review.created_at) })}
                    </p>
                    <dl className="mt-4 space-y-3">
                      {(["summary", "benefits", "risks", "alternatives"] as const).map((field) => (
                        <div key={field}>
                          <dt className="text-muted mb-1 text-xs">{t(`reviewFields.${field}`)}</dt>
                          <dd className="text-sm leading-relaxed break-words whitespace-pre-wrap">
                            {review.input[field]}
                          </dd>
                        </div>
                      ))}
                    </dl>
                  </div>
                ))}
              </div>
            )}
          </section>
          {ticket.decisions.length > 0 && (
            <section>
              <h2 className="mb-3 text-base font-medium">{t("history")}</h2>
              <div className="space-y-3">
                {[...ticket.decisions].reverse().map((decision, index) => (
                  <div
                    key={`${decision.created_at}:${index}`}
                    className="border-line rounded-xl border p-4"
                  >
                    <div className="flex flex-wrap justify-between gap-2">
                      <strong className="text-sm">{t(`outcome.${decision.input.outcome}`)}</strong>
                      <span className="text-muted text-xs">
                        {t("revision", { revision: decision.revision })}
                      </span>
                    </div>
                    <p className="text-muted mt-2 text-[11px]">
                      {t("by", { actor: decision.decider, date: date(decision.created_at) })}
                    </p>
                    <p className="mt-3 text-sm leading-relaxed break-words whitespace-pre-wrap">
                      {decision.input.reason.trim() || t("noReason")}
                    </p>
                    {decision.input.next_action && (
                      <p className="text-muted mt-3 text-xs leading-relaxed break-words whitespace-pre-wrap">
                        {decision.input.next_action}
                      </p>
                    )}
                  </div>
                ))}
              </div>
            </section>
          )}
          <section>
            <h2 className="mb-3 text-base font-medium">{t("revisions")}</h2>
            {[...ticket.revisions].reverse().map((revision) => (
              <details key={revision.revision} className="border-line border-b py-3">
                <summary className="flex cursor-pointer flex-wrap items-center justify-between gap-2 text-xs">
                  <span>
                    {t("revision", { revision: revision.revision })}{" "}
                    {revision.revision === ticket.current_revision ? t("currentRevision") : ""}
                  </span>
                  <span className="text-muted">
                    {t("by", { actor: revision.author, date: date(revision.created_at) })}
                  </span>
                </summary>
                <div className="mt-4 space-y-3">
                  <h3 className="text-sm font-medium">{revision.input.title}</h3>
                  <p className="text-muted text-xs">{revision.input.tags.join(" · ")}</p>
                  {(["problem", "proposal", "impact", "acceptance", "scope"] as const).map(
                    (field) => (
                      <div key={field}>
                        <h3 className="text-muted text-xs">{t(`fields.${field}`)}</h3>
                        <p className="mt-1 text-sm leading-relaxed break-words whitespace-pre-wrap">
                          {revision.input[field]}
                        </p>
                      </div>
                    ),
                  )}
                </div>
              </details>
            ))}
          </section>
        </div>
        <aside className="min-w-0">
          <DecisionForm key={ticket.note_uid} ticket={ticket} />
        </aside>
      </div>
    </article>
  );
}

function DecisionForm({ ticket }: { ticket: TicketView }) {
  const { t } = useTranslation("proposals");
  const errorText = useErrorText();
  const mutation = useDecideProposal();
  const [outcome, setOutcome] = useState<DecisionOutcome>(
    canDecide(ticket, "approve") ? "approve" : "hold",
  );
  const [reason, setReason] = useState("");
  const [nextAction, setNextAction] = useState("");
  const [owner, setOwner] = useState("");
  const [reconsider, setReconsider] = useState("");
  const [draftEtag, setDraftEtag] = useState(ticket.etag);
  const [confirmation, setConfirmation] = useState<DecisionConfirmation | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const submittingRef = useRef(false);
  const cancelRef = useRef<HTMLButtonElement>(null);
  const terminal = ticket.status === "approved" || ticket.status === "rejected";
  const ready =
    canDecide(ticket, outcome) && decisionFormReady(outcome, nextAction, owner, reconsider);
  // 再取得で草稿を消さず、更新前の版に対する確認だけを無効にする。
  if (confirmation && !submitting && !decisionConfirmationIsCurrent(confirmation, ticket)) {
    setConfirmation(null);
  }
  const openConfirmation = () => {
    if (!ready || submittingRef.current || mutation.isPending) return;
    setDraftEtag(ticket.etag);
    setConfirmation(
      createDecisionConfirmation(ticket, {
        outcome,
        reason: reason.trim(),
        next_action:
          outcome === "hold"
            ? `${t("nextAction")}: ${nextAction.trim()}\n${t("owner")}: ${owner.trim()}\n${t("reconsider")}: ${reconsider.trim()}`
            : "",
      }),
    );
  };
  const confirm = async () => {
    if (!confirmation || submittingRef.current || mutation.isPending) return;
    if (!decisionConfirmationIsCurrent(confirmation, ticket)) {
      setConfirmation(null);
      toast.error(t("confirmationChanged"));
      return;
    }
    // 同じ描画の間に届いた連続クリックも、最初の確定だけを送信する。
    submittingRef.current = true;
    setSubmitting(true);
    try {
      const result = await mutation.mutateAsync({
        note: confirmation.note,
        expectedEtag: confirmation.expectedEtag,
        input: { ...confirmation.input },
      });
      setOutcome(canDecide(result.ticket, "approve") ? "approve" : "hold");
      setReason("");
      setNextAction("");
      setOwner("");
      setReconsider("");
      setDraftEtag(result.ticket.etag);
      if (result.export_pending) toast.warning(t("exportPending"));
      else toast.success(t("decisionDone"));
    } catch (error) {
      toast.error(errorText(error));
    } finally {
      setConfirmation(null);
      submittingRef.current = false;
      setSubmitting(false);
    }
  };
  return (
    <section className="border-line bg-panel-2 rounded-xl border p-5">
      <h2 className="flex items-center gap-2 text-sm font-semibold">
        <ClipboardCheck className="size-4" />
        {t("decision")}
      </h2>
      <p className="text-muted mt-3 text-xs leading-relaxed">
        {terminal ? t("terminal") : t("decisionHint")}
      </p>
      {!terminal && draftEtag !== ticket.etag && (
        <p role="alert" className={proposalChangedVariants()}>
          {t("confirmationChanged")}
        </p>
      )}
      {!terminal && (
        <form
          onSubmit={(event) => {
            event.preventDefault();
            openConfirmation();
          }}
        >
          <div className="mt-4 flex flex-wrap gap-1.5">
            {(["approve", "reject", "hold"] as const).map((value) => (
              <button
                key={value}
                type="button"
                aria-pressed={outcome === value}
                disabled={!canDecide(ticket, value) || mutation.isPending}
                className={proposalFilterVariants({
                  active: outcome === value,
                  className: "disabled:cursor-default disabled:opacity-35",
                })}
                onClick={() => setOutcome(value)}
              >
                {t(`outcome.${value}`)}
              </button>
            ))}
          </div>
          {!canDecide(ticket, "approve") && (
            <p className="text-muted mt-3 text-xs leading-relaxed">{t("reviewRequired")}</p>
          )}
          <label className="mt-5 block text-xs font-medium">
            {t("reason")}
            <textarea
              maxLength={4000}
              rows={4}
              className={`${proposalFieldClass} mt-2 resize-y font-normal`}
              value={reason}
              onChange={(event) => setReason(event.target.value)}
              placeholder={t("reasonPlaceholder")}
              disabled={mutation.isPending}
            />
          </label>
          {outcome === "hold" && (
            <div className="mt-4 space-y-3">
              {(
                [
                  { key: "nextAction", value: nextAction, set: setNextAction },
                  { key: "owner", value: owner, set: setOwner },
                  { key: "reconsider", value: reconsider, set: setReconsider },
                ] as const
              ).map((field) => (
                <label key={field.key} className="block text-xs font-medium">
                  {t(field.key)}
                  <input
                    required
                    maxLength={1000}
                    className={`${proposalFieldClass} mt-1.5 font-normal`}
                    value={field.value}
                    onChange={(event) => field.set(event.target.value)}
                    disabled={mutation.isPending}
                  />
                </label>
              ))}
            </div>
          )}
          <p className="text-muted mt-4 text-[11px] leading-relaxed">{t("required")}</p>
          {mutation.error && (
            <p role="alert" className="text-danger mt-4 text-xs leading-relaxed">
              {errorText(mutation.error)}
            </p>
          )}
          <Button
            type="submit"
            variant="primary"
            className="mt-4 w-full"
            disabled={!ready || mutation.isPending}
          >
            {mutation.isPending ? t("saving") : t("record")}
          </Button>
        </form>
      )}
      <Dialog
        open={confirmation !== null}
        onOpenChange={(open) => {
          if (!open && !submittingRef.current) setConfirmation(null);
        }}
      >
        <DialogContent
          showCloseButton={false}
          className="bg-panel max-h-[85vh] overflow-y-auto"
          onOpenAutoFocus={(event) => {
            event.preventDefault();
            cancelRef.current?.focus();
          }}
          onEscapeKeyDown={(event) => {
            if (submittingRef.current) event.preventDefault();
          }}
          onInteractOutside={(event) => {
            if (submittingRef.current) event.preventDefault();
          }}
        >
          <DialogHeader>
            <DialogTitle>{t("confirmationTitle")}</DialogTitle>
            <DialogDescription>{t("confirmationDescription")}</DialogDescription>
          </DialogHeader>
          {confirmation && (
            <dl className="space-y-4 text-sm">
              <div>
                <dt className="text-muted mb-1 text-xs">{t("confirmationProposal")}</dt>
                <dd className="font-medium break-words">{confirmation.title}</dd>
                <dd className="text-muted mt-1 text-xs">
                  {t("revision", { revision: confirmation.revision })}
                </dd>
              </div>
              <div>
                <dt className="text-muted mb-1 text-xs">{t("decision")}</dt>
                <dd className="font-semibold">{t(`outcome.${confirmation.input.outcome}`)}</dd>
              </div>
              <div>
                <dt className="text-muted mb-1 text-xs">{t("reason")}</dt>
                <dd className="break-words whitespace-pre-wrap">
                  {confirmation.input.reason || t("noReason")}
                </dd>
              </div>
              {confirmation.input.outcome === "hold" && (
                <div>
                  <dt className="text-muted mb-1 text-xs">{t("nextAction")}</dt>
                  <dd className="break-words whitespace-pre-wrap">
                    {confirmation.input.next_action}
                  </dd>
                </div>
              )}
            </dl>
          )}
          <DialogFooter>
            <Button
              ref={cancelRef}
              type="button"
              disabled={submitting}
              onClick={() => setConfirmation(null)}
            >
              {t("confirmationBack")}
            </Button>
            <Button
              type="button"
              variant="primary"
              disabled={submitting || confirmation === null}
              onClick={() => void confirm()}
            >
              {submitting ? t("saving") : t("confirmationConfirm")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </section>
  );
}
