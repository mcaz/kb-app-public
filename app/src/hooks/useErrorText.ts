import { useCallback } from "react";
import { useTranslation } from "react-i18next";

import { isKbError } from "@/lib/api";

import type { BackupFailureKind } from "@/lib/bindings";

function backupErrorKey(kind: BackupFailureKind) {
  switch (kind) {
    case "authentication":
      return "errors.backupAuthentication";
    case "permission":
      return "errors.backupPermission";
    case "privacy_check":
      return "errors.backupPrivacyCheck";
    case "network":
      return "errors.backupNetwork";
    case "invalid_repository":
      return "errors.backupInvalidRepository";
    case "remote_missing":
      return "errors.backupRemoteMissing";
    case "quota":
      return "errors.backupQuota";
    case "lfs_unavailable":
      return "errors.backupLfsUnavailable";
    case "remote_object_missing":
      return "errors.backupRemoteObjectMissing";
    case "integrity_mismatch":
      return "errors.backupIntegrityMismatch";
    case "invalid_vault":
      return "errors.backupInvalidVault";
    case "workspace_mismatch":
      return "errors.backupWorkspaceMismatch";
    case "destination_exists":
      return "errors.backupDestinationExists";
    case "commit":
      return "errors.backupCommit";
    case "lfs_upload":
      return "errors.backupLfsUpload";
    case "git_push":
      return "errors.backupGitPush";
    case "git_pull":
      return "errors.backupGitPull";
    case "git_conflict":
      return "errors.backupGitConflict";
  }
}

/** 同期sidecarとコマンド失敗に共通の分類を、同じ案内文へ変換する。 */
export function useBackupErrorText(): (kind: BackupFailureKind | null, fallback: string) => string {
  const { t } = useTranslation();
  return useCallback(
    (kind, fallback) =>
      kind ? t(backupErrorKey(kind)) : t("errors.backupFailed", { message: fallback }),
    [t],
  );
}

/**
 * 例外を画面に出す文字列へ。
 *
 * コア由来のエラーは `code` で訳し分ける。`unexpected` だけは訳せないので
 * コアの文言をそのまま出す(英語環境ではここだけ日本語が混じる —
 * コア側のエラーコード化が済むまでの限界。ADR-0002)。
 */
export function useErrorText(): (e: unknown) => string {
  const { t } = useTranslation();
  const backupErrorText = useBackupErrorText();

  return useCallback(
    (e: unknown) => {
      if (!isKbError(e)) return String(e);
      const d = e.detail;
      switch (d.code) {
        case "note_not_found":
          return t("errors.noteNotFound");
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
        case "file_not_here":
          return t("errors.fileNotHere");
        case "clipboard_image_too_large":
          return t("errors.clipboardImageTooLarge");
        case "claude_desktop_not_found":
          return t("errors.claudeDesktopNotFound");
        case "claude_desktop_launch_failed":
          return t("errors.claudeDesktopLaunchFailed");
        case "backup_failed":
          return backupErrorText(d.kind, d.message);
        case "embed_failed":
          return t("errors.embedFailed", { message: d.message });
        case "vault_unavailable":
          return t("errors.vaultUnavailable", { message: d.message });
        default:
          return d.message;
      }
    },
    [backupErrorText, t],
  );
}
