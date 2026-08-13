import type { AppError } from "@/lib/bindings";

export type { AppError };

/**
 * コアが返したエラー。`detail.code` で種類が分かるので、画面はそれを訳す。
 *
 * Error を継承しているのは TanStack Query が例外としてエラー状態に載せるため。
 * message には日本語の文言(コア由来)が入るが、表示にはできるだけ
 * `code` からの訳を使う — message はまだ日本語固定のため(ADR-0002 の限界)。
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
    case "backup_failed":
    case "embed_failed":
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
