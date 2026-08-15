import { Cloud, Link, NotebookText, Sparkles, Wrench } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { DegradedBanner } from "@/components/molecules/DegradedBanner";
import { Pager } from "@/components/molecules/Pager";
import { RecentNoteRow } from "@/components/molecules/RecentNoteRow";
import { StatTile } from "@/components/molecules/StatTile";
import { SinglePaneLayout } from "@/components/templates/SinglePaneLayout";
import { formatDay } from "@/lib/format";
import { paginate } from "@/lib/hits";
import { useConnectState, useHomeState } from "@/lib/queries";
import { useSession } from "@/lib/stores/session";

const RECENT_PER_PAGE = 6;

/** ホーム(健全性の要約・最近のノート)。 */
export function HomePage() {
  const { t, i18n } = useTranslation(["home", "common"]);
  const { data: home } = useHomeState();
  const { data: connect } = useConnectState();
  const openNote = useSession((s) => s.openNote);
  const go = useSession((s) => s.go);
  const clearTags = useSession((s) => s.clearTags);
  const [recentPage, setRecentPage] = useState(0);

  if (!home) return <SinglePaneLayout>{null}</SinglePaneLayout>;

  const stats = home.stats;
  const recent = paginate(home.notes, RECENT_PER_PAGE, recentPage);

  const backupValue = !connect
    ? "…"
    : !connect.backup.remote
      ? t("tile.backupUnset")
      : connect.backup.pending > 0
        ? t("tile.backupPending", { count: connect.backup.pending })
        : t("tile.backupOk");

  const warnings = [
    ...home.degraded,
    ...(connect?.sync_error ? [t("warning.syncError", { error: connect.sync_error })] : []),
  ];

  return (
    <SinglePaneLayout>
      <div className="px-6 py-5">
        <DegradedBanner messages={warnings} variant="card" />

        <div className="grid grid-cols-[repeat(auto-fit,minmax(140px,1fr))] gap-2.5">
          <StatTile
            value={stats.total - stats.deprecated}
            label={t("tile.notes")}
            icon={NotebookText}
            onClick={() => {
              clearTags();
              go("notes");
            }}
          />
          <StatTile
            value={home.care.length}
            label={t("tile.care")}
            icon={Wrench}
            amber={home.care.length > 0}
          />
          <StatTile value={stats.links} label={t("tile.links")} icon={Link} />
          <StatTile
            value={
              stats.embed_enabled ? `${stats.embedded}/${stats.total}` : t("tile.smartSearchOff")
            }
            label={t("tile.smartSearch")}
            icon={Sparkles}
          />
          <StatTile
            value={backupValue}
            label={t("tile.backup")}
            icon={Cloud}
            amber={Boolean(connect?.sync_error)}
          />
        </div>

        <h2 className="text-muted mt-6 mb-2 text-xs tracking-[0.08em]">{t("recent.head")}</h2>
        <div className="flex max-w-[46em] flex-col gap-1.5">
          {recent.items.map((hit) => (
            <RecentNoteRow
              key={hit.id}
              hit={hit}
              datesLabel={t("common:date.createdUpdated", {
                created: formatDay(hit.created, i18n.language),
                updated: formatDay(hit.updated, i18n.language),
              })}
              onOpen={() => openNote(hit.id)}
            />
          ))}
          <Pager
            page={recent.page}
            pageCount={recent.pageCount}
            from={recent.from}
            to={recent.to}
            total={recent.total}
            onChange={setRecentPage}
            labels={{ previous: "‹", next: "›" }}
            align="start"
          />
        </div>
      </div>
    </SinglePaneLayout>
  );
}
