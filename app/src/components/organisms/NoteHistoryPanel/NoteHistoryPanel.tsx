import { useState } from "react";
import { useTranslation } from "react-i18next";

import { formatDateTime } from "@/lib/format";
import { useNoteHistory, useNoteProvenance } from "@/lib/queries";

const HISTORY_LIMIT = 20;

/** removeだけ破壊的操作として赤で目立たせる(design-system: 緑=正常・琥珀=提案・赤=破壊)。 */
const KIND_TONE: Record<string, string> = { remove: "text-danger" };

/** unified diffの1行の色。+はgrow、-はdanger、それ以外(見出し行含む)はmuted。 */
function diffLineClass(line: string): string {
  if (line.startsWith("+") && !line.startsWith("+++")) return "text-grow";
  if (line.startsWith("-") && !line.startsWith("---")) return "text-danger";
  return "text-muted";
}

/** 見出し文字列(`## 決定` 等)から表示用の見出し語だけを取り出す。 */
function headingLabel(heading: string): string {
  return heading.replace(/^#+\s*/, "") || heading;
}

export interface NoteHistoryPanelProps {
  noteId: string;
}

/** ノート詳細の履歴パネル(ADR-0023 Phase 3)。折りたたみ、行を開くと差分を見せる。 */
export function NoteHistoryPanel({ noteId }: NoteHistoryPanelProps) {
  const { t, i18n } = useTranslation(["notes", "common"]);
  const { data: provenance } = useNoteProvenance(noteId);
  // 差分は「差分を表示」を押すまでwith_diff=falseのまま取得する(契約20: 台帳を毎回運ばない)。
  const [diffRequested, setDiffRequested] = useState(false);
  const { data: events } = useNoteHistory(noteId, HISTORY_LIMIT, diffRequested);
  const [openEventId, setOpenEventId] = useState<string | null>(null);

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

  const toggleEvent = (id: string) => setOpenEventId((current) => (current === id ? null : id));
  const showDiff = (id: string) => {
    setDiffRequested(true);
    setOpenEventId(id);
  };

  return (
    <details className="border-line bg-panel mb-3 max-w-[46em] min-w-0 rounded-xl border">
      <summary className="text-ink cursor-pointer list-none px-4 py-3 text-sm font-medium">
        {t("notes:history.head")}
      </summary>
      <div className="border-line border-t px-4 py-3 text-[13px]">
        {events === undefined ? null : events.length === 0 ? (
          <p className="text-muted text-sm">{t("notes:history.empty")}</p>
        ) : (
          <>
            {provenance && provenance.section_authors.length > 0 && (
              <div className="mb-3.5">
                <div className="text-muted mb-1.5 text-xs">
                  {t("notes:history.sectionAuthorsHead")}
                </div>
                <ul className="flex flex-col gap-1">
                  {provenance.section_authors.map((author) => (
                    <li
                      key={author.heading}
                      className="text-muted flex flex-wrap items-baseline gap-x-1.5 text-[11px]"
                    >
                      <span className="text-ink">{headingLabel(author.heading)}</span>
                      <span>· {author.actor_label}</span>
                      <span>· {at(author.at)}</span>
                    </li>
                  ))}
                </ul>
              </div>
            )}

            <ul className="flex flex-col gap-2">
              {events.map((event) => {
                const isOpen = openEventId === event.event_id;
                return (
                  <li key={event.event_id} className="border-line rounded-lg border">
                    <button
                      type="button"
                      className="flex w-full flex-col gap-1 px-3 py-2 text-left"
                      aria-expanded={isOpen}
                      onClick={() => toggleEvent(event.event_id)}
                    >
                      <span className="text-muted flex flex-wrap items-center gap-x-2 text-[11px]">
                        <span>{at(event.at)}</span>
                        <span>{event.actor_label}</span>
                        <span className={KIND_TONE[event.kind]}>
                          {operationLabels[event.operation] ?? event.operation} ·{" "}
                          {kindLabels[event.kind] ?? event.kind}
                        </span>
                      </span>
                      <span className="text-ink">
                        {event.summary ?? t("notes:history.noSummary")}
                      </span>
                      {event.sections.length > 0 && (
                        <span className="flex flex-wrap gap-1">
                          {event.sections.map((heading) => (
                            <span
                              key={heading}
                              className="border-line bg-panel-2 text-muted rounded-full border px-2 py-px text-[11px]"
                            >
                              {headingLabel(heading)}
                            </span>
                          ))}
                        </span>
                      )}
                    </button>

                    {isOpen && (
                      <div className="border-line border-t px-3 py-2">
                        {event.reason && (
                          <p className="text-muted mb-1 text-[11px]">
                            {t("notes:history.reason", { reason: event.reason })}
                          </p>
                        )}
                        {event.origin_claim && (
                          <p className="text-muted mb-1 text-[11px]">
                            {t("notes:history.originClaim", { origin: event.origin_claim })}
                          </p>
                        )}
                        {event.changes.length > 0 && (
                          <div className="mb-2">
                            <div className="text-muted mb-1 text-[11px]">
                              {t("notes:history.changesHead")}
                            </div>
                            <ul className="flex flex-col gap-0.5">
                              {event.changes.map((change) => (
                                <li key={change.field} className="text-muted text-[11px]">
                                  {t("notes:history.changeItem", {
                                    field: change.field,
                                    from: change.from,
                                    to: change.to,
                                  })}
                                </li>
                              ))}
                            </ul>
                          </div>
                        )}
                        {event.body_diff ? (
                          <pre className="bg-panel-2 max-w-full overflow-x-auto rounded-md p-2 text-[11px] leading-relaxed">
                            {event.body_diff.split("\n").map((line, index) => (
                              // 差分の行は追加・削除で入れ替わるため、安定キーは行番号しか無い。
                              <div
                                key={`${event.event_id}-${index}`}
                                className={diffLineClass(line)}
                              >
                                {line}
                              </div>
                            ))}
                          </pre>
                        ) : (
                          <button
                            type="button"
                            className="text-grow cursor-pointer border-none bg-transparent p-0 text-[11.5px] underline"
                            onClick={() => showDiff(event.event_id)}
                          >
                            {t("notes:history.showDiff")}
                          </button>
                        )}
                        {event.diff_truncated && (
                          <p className="text-prop mt-1 text-[11px]">
                            {t("notes:history.diffTruncated")}
                          </p>
                        )}
                      </div>
                    )}
                  </li>
                );
              })}
            </ul>
          </>
        )}
      </div>
    </details>
  );
}
