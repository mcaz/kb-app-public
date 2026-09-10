import { BookOpen, ClipboardCheck, Cloud, Link, NotebookText, RefreshCw } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/atoms/ui/button";
import { DegradedBanner } from "@/components/molecules/DegradedBanner";
import { StatTile } from "@/components/molecules/StatTile";
import { ActivityFeed } from "@/components/organisms/ActivityFeed";
import { NoteCountTrendPanel } from "@/components/organisms/NoteCountTrendPanel";
import { ObservationHealthPanel } from "@/components/organisms/ObservationHealthPanel";
import { ObservationTrendPanel } from "@/components/organisms/ObservationTrendPanel";
import { SinglePaneLayout } from "@/components/templates/SinglePaneLayout";
import { useConnectState, useProposals } from "@/lib/queries";
import { useSession } from "@/lib/stores/session";

import { homeRefreshVariants } from "./variants";

import type { Degradation, HomeState } from "@/lib/api";

/** ノート探索は検索モーダルに任せ、ホームは健全性の要約を示す。 */
export function HomePage({
  home,
  onOpenAllNotes,
  onOpenGettingStarted,
  refresh,
}: {
  home: HomeState | undefined;
  onOpenAllNotes: () => void;
  onOpenGettingStarted?: () => void;
  refresh: {
    updatedAt: number;
    isFetching: boolean;
    isError: boolean;
    detectionFailed: boolean;
    retry: () => void;
  };
}) {
  const { t, i18n } = useTranslation(["home", "common", "gettingStarted"]);
  const { data: connect } = useConnectState();
  const proposals = useProposals();
  const openProposal = useSession((s) => s.openProposal);

  const styles = homeRefreshVariants();
  const proposalCount = proposals.isError ? undefined : proposals.data?.tickets.length;

  const backupValue = !connect
    ? "…"
    : !connect.backup.remote
      ? t("tile.backupUnset")
      : connect.backup.pending > 0
        ? t("tile.backupPending", { count: connect.backup.pending })
        : t("tile.backupOk");

  const warnings: Degradation[] = [
    ...(home?.degraded ?? []),
    ...(proposals.data?.degraded ?? []),
    ...(connect?.sync_error && !home?.degraded.some((item) => item.code === "remote_sync")
      ? [{ code: "remote_sync" as const, detail: connect.sync_error }]
      : []),
  ];

  return (
    <SinglePaneLayout>
      <div className="px-6 py-5">
        {onOpenGettingStarted && (
          <div className={styles.guideEntry()}>
            <p>{t("gettingStarted:homeHint")}</p>
            <Button size="sm" onClick={onOpenGettingStarted}>
              <BookOpen className={styles.icon()} />
              {t("gettingStarted:openGuide")}
            </Button>
          </div>
        )}
        <div className={styles.row()}>
          <span className={styles.timestamp()}>
            {refresh.isFetching
              ? t("refresh.loading")
              : refresh.updatedAt > 0
                ? t("refresh.lastRead", {
                    time: new Date(refresh.updatedAt).toLocaleTimeString(i18n.resolvedLanguage),
                  })
                : t("refresh.notRead")}
          </span>
          <Button variant="quiet" size="sm" disabled={refresh.isFetching} onClick={refresh.retry}>
            <RefreshCw className={styles.icon()} />
            {t("refresh.retry")}
          </Button>
        </div>
        {refresh.isError && (
          <p role="alert" className={styles.error()}>
            {t(home ? "refresh.stale" : "refresh.unavailable")}
          </p>
        )}
        {refresh.detectionFailed && (
          <p role="status" className={styles.error()}>
            {t("refresh.detectionFailed")}
          </p>
        )}
        <DegradedBanner items={warnings} variant="card" />

        {home && (
          <div className="grid grid-cols-[repeat(auto-fit,minmax(140px,1fr))] gap-2.5">
            <StatTile
              value={home.note_count}
              label={t("tile.notes")}
              icon={NotebookText}
              onClick={onOpenAllNotes}
            />
            <StatTile
              value={proposals.isError ? "—" : (proposalCount ?? "…")}
              label={t("tile.proposals")}
              icon={ClipboardCheck}
              amber={(proposalCount ?? 0) > 0}
              onClick={() => openProposal(null)}
            />
            <StatTile value={home.stats.links} label={t("tile.links")} icon={Link} />
            <StatTile
              value={backupValue}
              label={t("tile.backup")}
              icon={Cloud}
              amber={Boolean(connect?.sync_error)}
            />
          </div>
        )}
        {proposals.isError && (
          <p role="alert" className="text-danger mt-3 text-sm">
            {t("tile.proposalsUnavailable")}
          </p>
        )}

        <NoteCountTrendPanel />
        <ObservationTrendPanel />
        <ObservationHealthPanel />

        <ActivityFeed />
      </div>
    </SinglePaneLayout>
  );
}
