import { Cloud, Link, NotebookText, Sparkles, Tag, Wrench } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { Icon } from "@/components/atoms/Icon";
import { DegradedBanner } from "@/components/molecules/DegradedBanner";
import { Pager } from "@/components/molecules/Pager";
import { RecentNoteRow } from "@/components/molecules/RecentNoteRow";
import { StatTile } from "@/components/molecules/StatTile";
import { TagRow } from "@/components/molecules/TagRow";
import { SinglePaneLayout } from "@/components/templates/SinglePaneLayout";
import { formatDay } from "@/lib/format";
import { paginate } from "@/lib/hits";
import { useConnectState, useHomeState, useTagOverview } from "@/lib/queries";
import { useSession } from "@/lib/stores/session";

const RECENT_PER_PAGE = 6;
const TAGS_PER_PAGE = 12;

/** ホーム(健全性の要約・タグ・最近のノート)。 */
export function HomePage() {
  const { t, i18n } = useTranslation(["home", "common"]);
  const { data: home } = useHomeState();
  const { data: tagOverview } = useTagOverview();
  const { data: connect } = useConnectState();
  const openNote = useSession((s) => s.openNote);
  const go = useSession((s) => s.go);
  const clearTags = useSession((s) => s.clearTags);
  const addTag = useSession((s) => s.addTag);
  const [recentPage, setRecentPage] = useState(0);
  const [tagPage, setTagPage] = useState(0);

  if (!home) return <SinglePaneLayout>{null}</SinglePaneLayout>;

  const stats = home.stats;
  const recent = paginate(home.notes, RECENT_PER_PAGE, recentPage);
  const tags = paginate(tagOverview?.tags ?? [], TAGS_PER_PAGE, tagPage);

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

        <h2 className="text-muted mt-6 mb-2 flex items-center gap-1.5 text-xs tracking-[0.08em]">
          <Icon as={Tag} size="sm" />
          {t("tags.head")}
        </h2>
        <div className="flex max-w-[60em] flex-col gap-1">
          {tags.total === 0 ? (
            <div className="text-muted text-xs">{t("tags.empty")}</div>
          ) : (
            tags.items.map((tag) => (
              <TagRow
                key={tag.tag}
                tag={tag}
                noDescriptionLabel={t("tags.noDescription")}
                onClick={() => {
                  clearTags();
                  addTag(tag.tag);
                }}
              />
            ))
          )}
          <Pager
            page={tags.page}
            pageCount={tags.pageCount}
            from={tags.from}
            to={tags.to}
            total={tags.total}
            onChange={setTagPage}
            labels={{ previous: "‹", next: "›" }}
            align="start"
          />
          {tagOverview?.glossary_note ? (
            <button
              type="button"
              className="border-line text-muted hover:border-grow hover:text-ink mt-1 cursor-pointer self-start rounded-lg border bg-transparent px-3 py-1 text-xs"
              onClick={() => openNote(tagOverview.glossary_note!)}
            >
              <Icon as={NotebookText} size="sm" className="mr-1 inline-block align-text-bottom" />
              {t("tags.openGlossary")}
            </button>
          ) : (
            <p className="text-muted mt-1 text-xs">{t("tags.hint")}</p>
          )}
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
