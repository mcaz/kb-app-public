import type { FileCard } from "@/lib/api";

export type FileKind = "pdf" | "image" | "document" | "other";

export interface FileSummary {
  totalCount: number;
  totalBytes: number;
  missingCount: number;
  byKind: Record<FileKind, number>;
}

export function fileKind(file: Pick<FileCard, "media_type" | "name">): FileKind {
  const media = file.media_type.toLowerCase();
  const name = file.name.toLowerCase();
  if (media === "application/pdf" || name.endsWith(".pdf")) return "pdf";
  if (media.startsWith("image/")) return "image";
  if (
    media.startsWith("text/") ||
    media === "application/json" ||
    /\.(md|markdown|txt|json|csv|tsv|yaml|yml|xml|html|css|js|ts|tsx|jsx)$/u.test(name)
  ) {
    return "document";
  }
  return "other";
}

export function filterFiles(files: FileCard[], query: string, kind: FileKind | "all") {
  const needle = query.trim().toLocaleLowerCase();
  return files.filter((file) => {
    if (kind !== "all" && fileKind(file) !== kind) return false;
    if (!needle) return true;
    return [file.name, ...file.notes.flatMap((note) => [note.title, note.id])].some((value) =>
      value.toLocaleLowerCase().includes(needle),
    );
  });
}

/** 一覧と同じ4分類を使い、ファイル画面の俯瞰値を一度で集計する。 */
export function summarizeFiles(files: FileCard[]): FileSummary {
  const summary: FileSummary = {
    totalCount: files.length,
    totalBytes: 0,
    missingCount: 0,
    byKind: { pdf: 0, image: 0, document: 0, other: 0 },
  };

  for (const file of files) {
    summary.totalBytes += file.size;
    summary.byKind[fileKind(file)] += 1;
    if (file.availability === "missing") summary.missingCount += 1;
  }

  return summary;
}

export function formatBytes(bytes: number, locale: string): string {
  if (bytes < 1024) return `${bytes} B`;

  const units = ["KB", "MB", "GB", "TB"] as const;
  const unitIndex = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)) - 1, units.length - 1);
  const value = bytes / 1024 ** (unitIndex + 1);
  return `${new Intl.NumberFormat(locale, {
    maximumFractionDigits: value >= 10 ? 0 : 1,
  }).format(value)} ${units[unitIndex]}`;
}
