import { NotebookText, Search } from "lucide-react";
import { useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/atoms/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/atoms/ui/tooltip";
import { DegradedBanner } from "@/components/molecules/DegradedBanner";
import { SinglePaneLayout } from "@/components/templates/SinglePaneLayout";
import { useTagOverview } from "@/lib/queries";

import { filterTags } from "./filterTags";
import { TagNotesDialog } from "./TagNotesDialog";
import { tagsPageVariants } from "./variants";

import type { TagInfo } from "@/lib/api";

export function TagsPage() {
  const { t, i18n } = useTranslation("tags");
  const styles = tagsPageVariants();
  const overview = useTagOverview();
  const [query, setQuery] = useState("");
  const [activeTag, setActiveTag] = useState<TagInfo | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const tags = useMemo(() => filterTags(overview.data?.tags ?? [], query), [overview.data, query]);
  const pinned = overview.data?.source_status === "pinned";
  const selected = overview.data?.tags.find((item) => item.tag === activeTag?.tag) ?? activeTag;

  return (
    <SinglePaneLayout>
      <DegradedBanner items={overview.data?.degraded ?? []} />
      <div className={styles.page()}>
        <div className={styles.heading()}>
          <h1 className={styles.title()}>{t("title")}</h1>
          <span className={styles.badge()}>{t("readOnly")}</span>
        </div>
        <p className={styles.intro()}>{t("description")}</p>

        {overview.isError && (
          <div role="alert" className={styles.alert()}>
            <p>{t(overview.data ? "refreshError" : "loadError")}</p>
            <Button
              type="button"
              size="sm"
              className={styles.retry()}
              disabled={overview.isFetching}
              onClick={() => void overview.refetch()}
            >
              {t("retry")}
            </Button>
          </div>
        )}
        {overview.data && overview.data.source_status !== "pinned" && (
          <p role="status" className={styles.alert()}>
            {t(`source.${overview.data.source_status}`)}
          </p>
        )}
        {pinned && overview.data && overview.data.skipped_count > 0 && (
          <p role="status" className={styles.alert()}>
            {t("source.partial")}
          </p>
        )}

        <label className={styles.searchLabel()}>
          <span className={styles.hidden()}>{t("search")}</span>
          <Search className={styles.searchIcon()} />
          <input
            ref={searchRef}
            type="search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder={t("search")}
            className={styles.search()}
          />
        </label>

        <p className={styles.count()} role="status">
          {overview.isPending
            ? t("loading")
            : overview.data
              ? t("count", { count: tags.length, total: overview.data.tags.length })
              : null}
        </p>

        {overview.data && (
          <section className={styles.table()} aria-label={t("title")}>
            <div className={styles.columns()} aria-hidden="true">
              <span>{t("columns.name")}</span>
              <span>{t("columns.role")}</span>
              <span className={styles.countHeading()}>{t("columns.count")}</span>
              <span />
            </div>
            {tags.length === 0 ? (
              <p className={styles.empty()}>{t(query.trim() ? "emptySearch" : "empty")}</p>
            ) : (
              <ul className={styles.list()}>
                {tags.map((item) => (
                  <li key={item.tag} className={styles.row()}>
                    <div className={styles.tag()}>
                      <h2 className={styles.tagName()}>{item.tag}</h2>
                      {pinned && !item.registered && (
                        <p className={styles.unregistered()}>{t("unregistered")}</p>
                      )}
                    </div>
                    {item.description?.trim() ? (
                      <p className={styles.role()}>{item.description}</p>
                    ) : (
                      <p className={styles.missingRole()}>
                        {t(pinned ? "roleMissing" : "roleUnavailable")}
                      </p>
                    )}
                    <dl className={styles.noteCount()}>
                      <dt className={styles.hidden()}>{t("columns.count")}</dt>
                      <dd>{item.count.toLocaleString(i18n.resolvedLanguage)}</dd>
                    </dl>
                    <Tooltip>
                      <TooltipTrigger asChild>
                        <Button
                          type="button"
                          size="icon"
                          className={styles.notesButton()}
                          aria-label={t("notesForTag", { tag: item.tag, count: item.count })}
                          onClick={(event) => {
                            triggerRef.current = event.currentTarget;
                            setActiveTag(item);
                          }}
                        >
                          <NotebookText aria-hidden="true" />
                        </Button>
                      </TooltipTrigger>
                      <TooltipContent>{t("viewNotes")}</TooltipContent>
                    </Tooltip>
                  </li>
                ))}
              </ul>
            )}
          </section>
        )}
      </div>

      {selected && (
        <TagNotesDialog
          key={selected.tag}
          tag={selected}
          onClose={() => setActiveTag(null)}
          onRestoreFocus={() => {
            if (triggerRef.current?.isConnected) triggerRef.current.focus();
            else searchRef.current?.focus();
          }}
        />
      )}
    </SinglePaneLayout>
  );
}
