import { useCallback } from "react";
import { useTranslation } from "react-i18next";

import { isKbError } from "@/lib/api";

/**
 * 例外を画面に出す文字列へ。
 *
 * コア由来のエラーは `code` で訳し分ける。`unexpected` だけは訳せないので
 * コアの文言をそのまま出す(英語環境ではここだけ日本語が混じる —
 * コア側のエラーコード化が済むまでの限界。ADR-0002)。
 */
export function useErrorText(): (e: unknown) => string {
  const { t } = useTranslation();

  return useCallback(
    (e: unknown) => {
      if (!isKbError(e)) return String(e);
      const d = e.detail;
      switch (d.code) {
        case "note_not_found":
          return t("errors.noteNotFound");
        case "attachment_too_large":
          return t("errors.attachmentTooLarge", { limit: d.limit_mb });
        case "file_too_large":
          return t("errors.fileTooLarge", { limit: Math.round(d.limit / 1024 / 1024) });
        case "file_conflict":
          return t("errors.fileConflict");
        case "file_location_unstable":
          return t("errors.fileLocationUnstable");
        case "file_client_repo_locked":
          return t("errors.fileClientRepoLocked");
        case "file_needs_confirm":
          return t("errors.fileNeedsConfirm");
        case "file_malformed":
          return t("errors.fileMalformed");
        case "clipboard_image_too_large":
          return t("errors.clipboardImageTooLarge");
        case "claude_desktop_not_found":
          return t("errors.claudeDesktopNotFound");
        case "claude_desktop_launch_failed":
          return t("errors.claudeDesktopLaunchFailed");
        case "backup_failed":
          return t("errors.backupFailed", { message: d.message });
        case "embed_failed":
          return t("errors.embedFailed", { message: d.message });
        case "vault_unavailable":
          return t("errors.vaultUnavailable", { message: d.message });
        default:
          return d.message;
      }
    },
    [t],
  );
}
