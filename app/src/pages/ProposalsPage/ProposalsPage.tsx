import { ArrowLeft, ClipboardCheck, Copy, Plus, RefreshCw, Search, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/atoms/ui/button";
import { DegradedBanner } from "@/components/molecules/DegradedBanner";
import { SinglePaneLayout } from "@/components/templates/SinglePaneLayout";
import { useErrorText } from "@/hooks/useErrorText";
import { useProposal, useProposals } from "@/lib/queries";
import { useSession } from "@/lib/stores/session";
import type { TicketView } from "@/lib/api";

import { ProposalDetail } from "./ProposalDetail";
import { filterProposals, proposalStatuses, proposalUpdatedAt, type ProposalFilter } from "./model";
import { proposalFieldClass, proposalFilterVariants, proposalStatusVariants } from "./variants";

export function ProposalsPage() {
  const { t, i18n } = useTranslation("proposals");
  const errorText = useErrorText();
  const list = useProposals();
  const selectedId = useSession((state) => state.selectedId);
  const openProposal = useSession((state) => state.openProposal);
  const [filter, setFilter] = useState<ProposalFilter>("all");
  const [query, setQuery] = useState("");
  const [prompt, setPrompt] = useState<string | null>(null);
  const selected = list.data?.tickets.find((ticket) => ticket.note_id === selectedId);
  const detail = useProposal(selected?.note_id ?? null);
  const tickets = useMemo(
    () => filterProposals(list.data?.tickets ?? [], filter, query),
    [list.data, filter, query],
  );
  const date = (value: string) =>
    new Date(value).toLocaleString(i18n.resolvedLanguage ?? i18n.language, {
      dateStyle: "medium",
      timeStyle: "short",
    });
  const request = (kind: "review" | "revise", ticket: TicketView) =>
    setPrompt(t(`prompts.${kind}`, { note: ticket.note_id, uid: ticket.note_uid }));

  return (
    <SinglePaneLayout>
      <DegradedBanner items={[...(list.data?.degraded ?? []), ...(detail.data?.degraded ?? [])]} />
      <div className="mx-auto w-full max-w-[1080px] px-7 py-7 max-[720px]:px-4 max-[720px]:py-5">
        {selected ? (
          <>
            <Button variant="quiet" className="mb-5 -ml-3" onClick={() => openProposal(null)}>
              <ArrowLeft />
              {t("back")}
            </Button>
            {detail.error ? (
              <p role="alert" className="text-danger mb-4 text-sm">
                {errorText(detail.error)}
              </p>
            ) : null}
            {detail.data ? (
              <ProposalDetail ticket={detail.data.ticket} date={date} request={request} />
            ) : (
              <p role="status" className="text-muted py-8 text-sm">
                {t("loading")}
              </p>
            )}
          </>
        ) : (
          <>
            <header className="flex flex-wrap items-start justify-between gap-4">
              <div>
                <h1 className="text-[28px] leading-tight font-medium">{t("title")}</h1>
                <p className="text-muted mt-2 text-sm">{t("subtitle")}</p>
              </div>
              <div className="flex flex-wrap gap-2">
                {list.isError && (
                  <Button
                    onClick={() => void list.refetch()}
                    disabled={list.isFetching}
                    aria-label={t("retry")}
                  >
                    <RefreshCw className={list.isFetching ? "animate-spin" : ""} />
                    {t("retry")}
                  </Button>
                )}
                <Button variant="primary" onClick={() => setPrompt(t("prompts.create"))}>
                  <Plus />
                  {t("newProposal")}
                </Button>
              </div>
            </header>
            <div aria-label={t("title")} className="mt-7 flex flex-wrap gap-2">
              {(["all", ...proposalStatuses] as const).map((status) => (
                <button
                  key={status}
                  type="button"
                  aria-pressed={filter === status}
                  className={proposalFilterVariants({ active: filter === status })}
                  onClick={() => setFilter(status)}
                >
                  {status === "all" ? t("all") : t(`status.${status}`)}
                  <span className="ml-2 opacity-60">
                    {
                      (list.data?.tickets ?? []).filter(
                        (ticket) => status === "all" || ticket.status === status,
                      ).length
                    }
                  </span>
                </button>
              ))}
            </div>
            <label className="relative mt-4 block max-w-xl">
              <span className="sr-only">{t("search")}</span>
              <Search className="text-muted pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2" />
              <input
                className={`${proposalFieldClass} pl-10`}
                type="search"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder={t("search")}
              />
            </label>
            <p className="text-muted mt-5 mb-3 text-xs">{t("count", { count: tickets.length })}</p>
            {list.error ? (
              <p role="alert" className="text-danger py-8 text-sm">
                {errorText(list.error)}
              </p>
            ) : list.isPending ? (
              <p role="status" className="text-muted py-8 text-sm">
                {t("loading")}
              </p>
            ) : tickets.length === 0 ? (
              <div className="border-line rounded-xl border border-dashed px-5 py-14 text-center">
                <ClipboardCheck className="text-muted mx-auto mb-4 size-8" />
                <p className="text-sm">{t("empty")}</p>
                <p className="text-muted mx-auto mt-2 max-w-md text-xs leading-relaxed">
                  {t("emptyHint")}
                </p>
              </div>
            ) : (
              <div className="border-line divide-line divide-y overflow-hidden rounded-xl border">
                {tickets.map((ticket) => (
                  <button
                    key={ticket.note_uid}
                    type="button"
                    className="bg-panel hover:bg-panel-2 flex w-full cursor-pointer flex-wrap items-center gap-4 border-0 px-5 py-5 text-left transition-colors"
                    onClick={() => openProposal(ticket.note_id)}
                  >
                    <div className="min-w-0 flex-1 basis-64">
                      <div className="mb-2 flex items-center gap-2">
                        <span className={proposalStatusVariants({ status: ticket.status })}>
                          {t(`status.${ticket.status}`)}
                        </span>
                        <span className="text-muted text-[11px]">
                          {t("revision", { revision: ticket.current_revision })}
                        </span>
                      </div>
                      <h2 className="text-[15px] font-medium break-words">{ticket.title}</h2>
                      <p className="text-muted mt-1.5 line-clamp-2 text-xs leading-relaxed">
                        {ticket.revisions.at(-1)?.input.problem}
                      </p>
                    </div>
                    <span className="text-muted text-xs">
                      {t("updated", { date: date(proposalUpdatedAt(ticket)) })}
                    </span>
                  </button>
                ))}
              </div>
            )}
          </>
        )}
        {prompt !== null && (
          <PromptPanel key={prompt} prompt={prompt} onClose={() => setPrompt(null)} />
        )}
      </div>
    </SinglePaneLayout>
  );
}

function PromptPanel({ prompt, onClose }: { prompt: string; onClose: () => void }) {
  const { t } = useTranslation("proposals");
  const inputRef = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    inputRef.current?.focus();
  }, []);
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">("idle");
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(prompt);
      setCopyState("copied");
    } catch {
      setCopyState("failed");
    }
  };
  return (
    <section
      aria-label={t("promptTitle")}
      className="border-grow/40 bg-panel-2 mt-6 rounded-xl border p-5"
    >
      <div className="flex items-center justify-between gap-3">
        <h2 className="text-sm font-semibold">{t("promptTitle")}</h2>
        <Button variant="quiet" size="icon" aria-label={t("close")} onClick={onClose}>
          <X />
        </Button>
      </div>
      <p className="text-muted mt-1 mb-3 text-xs">{t("promptHint")}</p>
      <textarea
        ref={inputRef}
        readOnly
        rows={6}
        aria-label={t("promptTitle")}
        value={prompt}
        className={`${proposalFieldClass} resize-y leading-relaxed`}
        onFocus={(event) => event.target.select()}
      />
      <div className="mt-3 flex flex-wrap items-center gap-3">
        <Button onClick={() => void copy()}>
          <Copy />
          {copyState === "copied" ? t("copied") : t("copy")}
        </Button>
        <span role="status" className="text-muted text-xs">
          {copyState === "failed" ? t("copyFailed") : copyState === "copied" ? t("copied") : ""}
        </span>
      </div>
    </section>
  );
}
