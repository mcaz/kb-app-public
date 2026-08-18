import type { FileCard } from "@/lib/api";

export type FileKind = "pdf" | "image" | "document" | "other";

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
