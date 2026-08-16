import type { AppError } from "@/lib/bindings";

export type { AppError };

/**
 * コアが返したエラー。`detail.code` で種類が分かるので、画面はそれを訳す。
 *
 * Error を継承しているのは TanStack Query が例外としてエラー状態に載せるため。
 * message はログ用。画面は `code` / `kind` から必ず翻訳する。
 */
export class KbError extends Error {
  readonly detail: AppError;

  constructor(detail: AppError) {
    super(describe(detail));
    this.name = "KbError";
    this.detail = detail;
  }
}

/** 訳が無い場面(ログなど)向けの素の説明。 */
function describe(detail: AppError): string {
  switch (detail.code) {
    case "vault_unavailable":
      return "vault unavailable";
    case "backup_failed":
      return `backup failed: ${detail.kind ?? "unknown"}`;
    case "embed_failed":
      return "embedding failed";
    case "core_failed":
      return `core failed: ${detail.kind}`;
    case "unexpected":
      return detail.message;
    case "note_not_found":
      return `note not found: ${detail.id}`;
    case "file_too_large":
      return `file too large: ${detail.size} > ${detail.limit}`;
    case "file_conflict":
      return `stale version: ${detail.expected} != ${detail.current}`;
    default:
      return detail.code;
  }
}

export const isKbError = (e: unknown): e is KbError => e instanceof KbError;
