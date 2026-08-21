import { Search } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { DegradedBanner } from "@/components/molecules/DegradedBanner";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/atoms/ui/select";
import { SinglePaneLayout } from "@/components/templates/SinglePaneLayout";
import { useFiles } from "@/lib/queries";

import { FileCard } from "./FileCard";
import { FilePreviewDialog } from "./FilePreviewDialog";
import { filterFiles, formatBytes, summarizeFiles, type FileKind } from "./fileKind";

const FILE_KINDS = ["pdf", "image", "document", "other"] as const;

/** ノートを横断して、現行版のファイルを探して開く一覧。 */
export function FilesPage() {
  const { t, i18n } = useTranslation("files");
  const { data, isPending } = useFiles();
  const [query, setQuery] = useState("");
  const [kind, setKind] = useState<FileKind | "all">("all");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const files = useMemo(() => filterFiles(data?.files ?? [], query, kind), [data, query, kind]);
  const summary = useMemo(() => summarizeFiles(data?.files ?? []), [data]);
  const selected = data?.files.find((file) => file.id === selectedId) ?? null;

  return (
    <SinglePaneLayout>
      <DegradedBanner items={data?.degraded ?? []} />
      <div className="mx-auto w-full max-w-[1180px] px-7 py-7 max-[720px]:px-4 max-[720px]:py-5">
        <h1 className="text-[28px] leading-tight font-medium">{t("title")}</h1>

        <section
          aria-label={t("summary.label")}
          className="border-line bg-panel-2/60 mt-6 overflow-hidden rounded-xl border"
        >
          <dl className="divide-line grid grid-cols-3 divide-x max-[560px]:grid-cols-1 max-[560px]:divide-x-0 max-[560px]:divide-y">
            <div className="px-4 py-3.5">
              <dt className="text-muted text-[11px] font-medium tracking-wide uppercase">
                {t("summary.total")}
              </dt>
              <dd className="mt-1 text-xl font-semibold tabular-nums">
                {t("count", { count: summary.totalCount })}
              </dd>
            </div>
            <div className="px-4 py-3.5">
              <dt className="text-muted text-[11px] font-medium tracking-wide uppercase">
                {t("summary.size")}
              </dt>
              <dd className="mt-1 text-xl font-semibold tabular-nums">
                {formatBytes(summary.totalBytes, i18n.resolvedLanguage ?? i18n.language)}
              </dd>
            </div>
            <div className="px-4 py-3.5">
              <dt className="text-muted text-[11px] font-medium tracking-wide uppercase">
                {t("summary.missing")}
              </dt>
              <dd className="mt-1 text-xl font-semibold tabular-nums">{summary.missingCount}</dd>
            </div>
          </dl>
          <dl className="border-line flex flex-wrap gap-x-5 gap-y-1 border-t px-4 py-2.5">
            {FILE_KINDS.map((value) => (
              <div key={value} className="flex items-baseline gap-1.5 text-xs">
                <dt className="text-muted">{t(`filter.${value}`)}</dt>
                <dd className="font-semibold tabular-nums">{summary.byKind[value]}</dd>
              </div>
            ))}
          </dl>
        </section>

        <div className="mt-4 grid grid-cols-[minmax(0,1fr)_auto] gap-2.5 max-[560px]:grid-cols-1">
          <label className="relative min-w-0">
            <span className="sr-only">{t("search")}</span>
            <Search className="text-muted pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2" />
            <input
              type="search"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder={t("search")}
              className="border-line bg-panel-2 text-ink w-full rounded-lg border py-2.5 pr-3 pl-10 text-[13px]"
            />
          </label>
          <Select value={kind} onValueChange={(value) => setKind(value as FileKind | "all")}>
            <SelectTrigger aria-label={t("filter.label")} className="bg-panel-2 w-40">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {(["all", ...FILE_KINDS] as const).map((value) => (
                <SelectItem key={value} value={value}>
                  {t(`filter.${value}`)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>

        <p className="text-muted mt-4 mb-3 text-xs">{t("count", { count: files.length })}</p>

        {!isPending && data?.files.length === 0 ? (
          <p className="text-muted py-14 text-center text-sm">{t("noFiles")}</p>
        ) : files.length === 0 ? (
          <p className="text-muted py-14 text-center text-sm">{t("empty")}</p>
        ) : (
          <div className="grid grid-cols-3 gap-4 max-[980px]:grid-cols-2 max-[560px]:grid-cols-1">
            {files.map((file) => (
              <FileCard key={file.id} file={file} onOpen={() => setSelectedId(file.id)} />
            ))}
          </div>
        )}
      </div>

      <FilePreviewDialog
        file={selected}
        open={selected !== null}
        onOpenChange={(open) => {
          if (!open) setSelectedId(null);
        }}
      />
    </SinglePaneLayout>
  );
}
