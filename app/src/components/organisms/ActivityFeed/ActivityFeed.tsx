import { Activity, HelpCircle, Users } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { StatTile } from "@/components/molecules/StatTile";
import { formatDateTime } from "@/lib/format";
import { useActivityFeed } from "@/lib/queries";
import { useSession } from "@/lib/stores/session";

import type { ActivityFilter } from "@/lib/api";

const LIMIT = 30;
type Period = "7" | "30" | "all";
const KINDS = ["create", "amend", "reverse", "correct", "normalize", "remove", "unknown"] as const;

const selectClass = "border-line bg-panel-2 text-ink rounded-md border px-2 py-1 text-xs";

function isoDaysAgo(days: number): string {
  return new Date(Date.now() - days * 24 * 60 * 60 * 1000).toISOString();
}

/** ホームの活動ビュー(ADR-0023 Phase 3)。ノートを跨いだ書込の時系列一覧。 */
export function ActivityFeed() {
  const { t, i18n } = useTranslation(["home", "notes", "common"]);
  const openNote = useSession((s) => s.openNote);
  const [client, setClient] = useState("");
  const [model, setModel] = useState("");
  const [kind, setKind] = useState("");
  const [period, setPeriod] = useState<Period>("all");

  const filter: ActivityFilter = {
    client: client || null,
    model: model || null,
    kind: kind || null,
    since: period === "all" ? null : isoDaysAgo(Number(period)),
  };
  const { data } = useActivityFeed(LIMIT, filter);

  const at = (iso: string) => formatDateTime(iso, i18n.language, t("common:date.unknown"));
  const operationLabels: Record<string, string> = {
    propose: t("notes:history.operation.propose"),
    update: t("notes:history.operation.update"),
    remove: t("notes:history.operation.remove"),
    distill: t("notes:history.operation.distill"),
    closure: t("notes:history.operation.closure"),
    import: t("notes:history.operation.import"),
    human_edit: t("notes:history.operation.human_edit"),
    other: t("notes:history.operation.other"),
  };
  const kindLabels: Record<string, string> = {
    create: t("notes:history.kind.create"),
    amend: t("notes:history.kind.amend"),
    reverse: t("notes:history.kind.reverse"),
    correct: t("notes:history.kind.correct"),
    normalize: t("notes:history.kind.normalize"),
    remove: t("notes:history.kind.remove"),
    unknown: t("notes:history.kind.unknown"),
  };

  const unknownRatio = data?.summary.unknown_model_ratio ?? 0;

  return (
    <section className="mt-7" aria-labelledby="activity-heading">
      <h2 id="activity-heading" className="text-muted mb-3 text-xl font-medium tracking-[0.04em]">
        {t("home:activity.head")}
      </h2>

      <div className="mb-3 grid grid-cols-[repeat(auto-fit,minmax(140px,1fr))] gap-2.5">
        <StatTile
          value={data?.summary.last_7_days ?? "…"}
          label={t("home:activity.tile.last7Days")}
          icon={Activity}
        />
        <StatTile
          value={data?.summary.distinct_actors ?? "…"}
          label={t("home:activity.tile.distinctActors")}
          icon={Users}
        />
        <StatTile
          value={data ? `${Math.round(unknownRatio * 100)}%` : "…"}
          label={t("home:activity.tile.unknownModel")}
          icon={HelpCircle}
          amber={unknownRatio > 0}
        />
      </div>

      <div className="mb-3 flex flex-wrap items-center gap-3">
        <label className="text-muted flex items-center gap-1.5 text-xs">
          {t("home:activity.filter.client")}
          <select
            className={selectClass}
            aria-label={t("home:activity.filter.client")}
            value={client}
            onChange={(event) => setClient(event.target.value)}
          >
            <option value="">{t("home:activity.filter.clientAll")}</option>
            {(data?.clients ?? []).map((value) => (
              <option key={value} value={value}>
                {value}
              </option>
            ))}
          </select>
        </label>

        <label className="text-muted flex items-center gap-1.5 text-xs">
          {t("home:activity.filter.model")}
          <select
            className={selectClass}
            aria-label={t("home:activity.filter.model")}
            value={model}
            onChange={(event) => setModel(event.target.value)}
          >
            <option value="">{t("home:activity.filter.modelAll")}</option>
            {(data?.models ?? []).map((value) => (
              <option key={value} value={value}>
                {value}
              </option>
            ))}
          </select>
        </label>

        <label className="text-muted flex items-center gap-1.5 text-xs">
          {t("home:activity.filter.kind")}
          <select
            className={selectClass}
            aria-label={t("home:activity.filter.kind")}
            value={kind}
            onChange={(event) => setKind(event.target.value)}
          >
            <option value="">{t("home:activity.filter.kindAll")}</option>
            {KINDS.map((value) => (
              <option key={value} value={value}>
                {kindLabels[value]}
              </option>
            ))}
          </select>
        </label>

        <label className="text-muted flex items-center gap-1.5 text-xs">
          {t("home:activity.filter.period")}
          <select
            className={selectClass}
            aria-label={t("home:activity.filter.period")}
            value={period}
            onChange={(event) => setPeriod(event.target.value as Period)}
          >
            <option value="7">{t("home:activity.filter.period7")}</option>
            <option value="30">{t("home:activity.filter.period30")}</option>
            <option value="all">{t("home:activity.filter.periodAll")}</option>
          </select>
        </label>
      </div>

      {!data || data.rows.length === 0 ? (
        <p className="text-muted text-sm">{t("home:activity.empty")}</p>
      ) : (
        <ul className="flex flex-col gap-1.5">
          {data.rows.map((row) => (
            <li key={row.event_id}>
              <button
                type="button"
                className="border-line hover:border-grow bg-panel flex w-full min-w-0 flex-wrap items-center gap-x-2.5 gap-y-1 rounded-lg border px-3 py-2 text-left text-xs"
                onClick={() => openNote(row.note_id)}
              >
                <span className="text-muted shrink-0">{at(row.at)}</span>
                <span className="text-ink font-medium">{row.title}</span>
                <span className="text-muted">{row.actor_label}</span>
                <span className={row.kind === "remove" ? "text-danger" : "text-muted"}>
                  {operationLabels[row.operation] ?? row.operation} ·{" "}
                  {kindLabels[row.kind] ?? row.kind}
                </span>
                {row.summary && <span className="text-muted min-w-0 truncate">{row.summary}</span>}
                <span className="text-muted ml-auto shrink-0">
                  {t("home:activity.row.sections", { count: row.section_count })}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
