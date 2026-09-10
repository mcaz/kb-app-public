import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { toast } from "sonner";

import { setupI18n } from "@/i18n";
import {
  api,
  KbError,
  type ConnectState,
  type RuntimeDiagnosticsReport,
  type RuntimeRecoveryPlan,
} from "@/lib/api";
import { queryKeys } from "@/lib/queries";

import { ConnectPage } from "./ConnectPage";

vi.mock(import("@/lib/api"), async (importOriginal) => {
  const actual = await importOriginal();
  return {
    ...actual,
    api: {
      ...actual.api,
      connectState: vi.fn(),
      connectClientRegistrations: vi.fn(),
      connectClientDiagnostics: vi.fn(),
      settingsAiGuardStatus: vi.fn(),
      githubAuthState: vi.fn(),
      inspectRuntimeStorage: vi.fn(),
      planRuntimeRecovery: vi.fn(),
    },
  };
});
vi.mock("@/components/organisms/GitHubAuthPanel", () => ({ GitHubAuthPanel: () => null }));
vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { loading: vi.fn(), success: vi.fn(), error: vi.fn() }),
}));

const connected: ConnectState = {
  desktop: "connected",
  backup: { remote: "https://github.com/example/notes.git", pending: 0 },
  sync_error: null,
  sync_error_kind: null,
  smart_search: { state: "not_installed", embedded: 0, total: 12 },
};

const diagnostics: RuntimeDiagnosticsReport = {
  format_version: 1,
  read_only: true,
  recovery_performed: false,
  database_snapshot_complete: false,
  declared_schema: null,
  runtime_store: null,
  schema_fingerprint: null,
  notes_columns: null,
  unrecognized_notes_columns: null,
  durable_tables: [{ table: "notes", present: null, rows: null }],
  exports: {
    upserts: null,
    deletes: null,
    unknown_operations: null,
    invalid_upsert_documents: null,
    latest_upserts_matching_db: null,
    latest_upserts_differing_from_db: null,
    latest_upserts_missing_from_db: null,
    latest_deletes_still_in_db: null,
  },
  jobs: {
    notes: null,
    missing_from_db: null,
    missing_from_db_with_valid_markdown: null,
    missing_from_db_with_valid_pending_upsert: null,
    missing_from_db_without_valid_markdown: null,
    missing_from_db_without_valid_markdown_or_pending_upsert: null,
    missing_from_db_without_valid_markdown_ids: [],
    missing_ids_truncated: false,
  },
  markdown: {
    atomic_snapshot: false,
    scan_complete: false,
    files: null,
    parsed: null,
    invalid: null,
    unreadable: null,
    database_only: null,
    markdown_only: null,
    matching_documents: null,
    differing_documents: null,
  },
  findings: [],
  issues: [{ code: "database_open_failed", table: null, note: null }],
  issue_count: 1,
  issues_truncated: false,
};

const recoveryPlan: RuntimeRecoveryPlan = {
  format_version: 1,
  read_only: true,
  recovery_performed: false,
  declared_schema: null,
  supported_reset_shape: false,
  atomic_snapshot: false,
  snapshot_complete: false,
  plan_digest: null,
  existing_markdown_coverage_complete: false,
  latest_state_proven: false,
  markdown_notes: null,
  job_notes: null,
  history_runs: null,
  summary: null,
  notes: [],
  note_count: 0,
  notes_truncated: false,
  issues: [{ code: "database_open_failed", blocking: true, note: null }],
  issue_count: 1,
  blocking_issue_count: 1,
  issues_truncated: false,
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((success, failure) => {
    resolve = success;
    reject = failure;
  });
  return { promise, resolve, reject };
}

let client: QueryClient;
const originalClipboard = Object.getOwnPropertyDescriptor(navigator, "clipboard");
const writeText = vi.fn<(text: string) => Promise<void>>();
beforeEach(() => {
  setupI18n("ja");
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  vi.mocked(toast.loading).mockReturnValue("diagnostics-progress");
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
  writeText.mockResolvedValue(undefined);
  vi.mocked(api.githubAuthState).mockResolvedValue({
    configured: false,
    signed_in: false,
    account_login: null,
  });
  vi.mocked(api.connectClientRegistrations).mockResolvedValue({
    vault_name: "test",
    workspace_id: "test-workspace",
    clients: [],
  });
  vi.mocked(api.connectClientDiagnostics).mockResolvedValue({
    observed_at_ms: 0,
    window_days: 30,
    os: "macos",
    workspace: {
      schema: 1,
      workspace_id: "test-workspace",
      vocabulary: {
        source_status: "unconfigured",
        source_note_uid: null,
        source_revision: null,
        source_document_sha256: null,
      },
    },
    clients: [],
  });
  vi.mocked(api.settingsAiGuardStatus).mockResolvedValue({
    ready: false,
    codex: "missing",
    claude: "missing",
    guarded_paths: [],
  });
});
afterEach(() => {
  cleanup();
  client.clear();
  if (originalClipboard) Object.defineProperty(navigator, "clipboard", originalClipboard);
  else Reflect.deleteProperty(navigator, "clipboard");
  vi.resetAllMocks();
});

describe("storage diagnostics", () => {
  it("件数不明と診断上の問題をJSONのままコピーし、コピー完了まで重複要求しない", async () => {
    vi.mocked(api.connectState).mockRejectedValue(
      new KbError({ code: "core_failed", kind: "storage" }),
    );
    vi.mocked(api.inspectRuntimeStorage).mockResolvedValue(diagnostics);
    const clipboard = deferred<void>();
    writeText.mockReturnValue(clipboard.promise);
    show();
    const copyButton = await screen.findByRole("button", { name: "診断結果をコピー" });
    expect(api.inspectRuntimeStorage).not.toHaveBeenCalled();
    fireEvent.click(copyButton);
    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith(JSON.stringify(diagnostics, null, 2)),
    );
    expect(screen.getByRole("button", { name: "診断しています…" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "復元元の照合結果をコピー" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "復元元の照合結果をコピー" }));
    expect(api.planRuntimeRecovery).not.toHaveBeenCalled();
    expect(toast.success).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "診断しています…" }));
    expect(api.inspectRuntimeStorage).toHaveBeenCalledOnce();
    await act(async () => {
      clipboard.resolve();
      await clipboard.promise;
    });
    expect(toast.success).toHaveBeenCalledWith("診断結果をコピーしました", {
      id: "diagnostics-progress",
    });
    expect(toast.error).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "診断結果をコピー" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "復元元の照合結果をコピー" })).toBeEnabled();
  });

  // 2026-09-07: WebViewが待機後のclipboardを拒否しても、再診断のawaitを挟んで拒否を繰り返さない。
  it.each([1, 2])(
    "コピーが%i回拒否されても取得済みJSONを次のクリックでコピーする",
    async (failures) => {
      vi.mocked(api.connectState).mockRejectedValue(
        new KbError({ code: "core_failed", kind: "storage" }),
      );
      vi.mocked(api.inspectRuntimeStorage).mockResolvedValue(diagnostics);
      for (let attempt = 0; attempt < failures; attempt += 1) {
        writeText.mockRejectedValueOnce(new Error("denied"));
      }
      show();
      fireEvent.click(await screen.findByRole("button", { name: "診断結果をコピー" }));
      for (let attempt = 1; attempt <= failures; attempt += 1) {
        await waitFor(() => expect(toast.error).toHaveBeenCalledTimes(attempt));
        expect(toast.error).toHaveBeenLastCalledWith(
          "診断結果は取得済みですが、コピーできませんでした。「取得済みの診断をコピー」を押してください。",
          { id: "diagnostics-progress" },
        );
        expect(toast.success).not.toHaveBeenCalled();
        const retryCopy = screen.getByRole("button", { name: "取得済みの診断をコピー" });
        expect(retryCopy).toBeEnabled();
        fireEvent.click(retryCopy);
        // 次のクリックからclipboard開始まで非同期処理を挟まない。
        expect(writeText).toHaveBeenCalledTimes(attempt + 1);
        expect(api.inspectRuntimeStorage).toHaveBeenCalledOnce();
      }
      await waitFor(() =>
        expect(toast.success).toHaveBeenCalledWith("診断結果をコピーしました", {
          id: "diagnostics-progress",
        }),
      );
      expect(writeText).toHaveBeenLastCalledWith(JSON.stringify(diagnostics, null, 2));
      expect(api.inspectRuntimeStorage).toHaveBeenCalledOnce();
      expect(screen.getByRole("button", { name: "診断結果をコピー" })).toBeEnabled();
    },
  );

  it("診断取得失敗は分類済みエラーを表示し、クリップボードを変更しない", async () => {
    vi.mocked(api.connectState).mockRejectedValue(
      new KbError({ code: "core_failed", kind: "storage" }),
    );
    const inspection = deferred<never>();
    vi.mocked(api.inspectRuntimeStorage).mockReturnValue(inspection.promise);
    show();
    const copyButton = await screen.findByRole("button", { name: "診断結果をコピー" });
    expect(api.inspectRuntimeStorage).not.toHaveBeenCalled();
    fireEvent.click(copyButton);
    await waitFor(() => expect(api.inspectRuntimeStorage).toHaveBeenCalledOnce());
    expect(screen.getByRole("button", { name: "診断しています…" })).toBeDisabled();
    expect(toast.loading).toHaveBeenCalledWith("診断しています…");
    await act(async () => {
      inspection.reject(new KbError({ code: "core_failed", kind: "storage" }));
      await inspection.promise.catch(() => undefined);
    });
    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith(
        "診断できませんでした: 端末上のデータを読み書きできませんでした",
        { id: "diagnostics-progress" },
      ),
    );
    expect(api.inspectRuntimeStorage).toHaveBeenCalledOnce();
    expect(writeText).not.toHaveBeenCalled();
    expect(toast.success).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "診断結果をコピー" })).toBeEnabled();
  });
});

describe("recovery source comparison", () => {
  it("明示操作で照合結果をコピーし、照合とコピーが終わるまで両方の診断を無効にする", async () => {
    vi.mocked(api.connectState).mockRejectedValue(
      new KbError({ code: "core_failed", kind: "storage" }),
    );
    const comparison = deferred<RuntimeRecoveryPlan>();
    const clipboard = deferred<void>();
    vi.mocked(api.planRuntimeRecovery).mockReturnValue(comparison.promise);
    writeText.mockReturnValue(clipboard.promise);
    show();
    const copyButton = await screen.findByRole("button", { name: "復元元の照合結果をコピー" });
    expect(copyButton).toHaveAccessibleDescription(
      "残っている本文と蒸留履歴を読み取り、復元元を照合します。データは変更しません。",
    );
    expect(api.planRuntimeRecovery).not.toHaveBeenCalled();
    fireEvent.click(copyButton);
    await waitFor(() => expect(api.planRuntimeRecovery).toHaveBeenCalledOnce());
    expect(toast.loading).toHaveBeenCalledWith("照合しています…");
    expect(screen.getByRole("button", { name: "照合しています…" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "診断結果をコピー" })).toBeDisabled();
    expect(writeText).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "診断結果をコピー" }));
    expect(api.inspectRuntimeStorage).not.toHaveBeenCalled();
    await act(async () => {
      comparison.resolve(recoveryPlan);
      await comparison.promise;
    });
    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith(JSON.stringify(recoveryPlan, null, 2)),
    );
    expect(screen.getByRole("button", { name: "照合しています…" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "診断結果をコピー" })).toBeDisabled();
    expect(toast.success).not.toHaveBeenCalled();
    await act(async () => {
      clipboard.resolve();
      await clipboard.promise;
    });
    expect(toast.success).toHaveBeenCalledWith("復元元の照合結果をコピーしました", {
      id: "diagnostics-progress",
    });
    expect(api.planRuntimeRecovery).toHaveBeenCalledOnce();
    expect(toast.error).not.toHaveBeenCalled();
    expect(screen.queryByText("database_open_failed")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "復元元の照合結果をコピー" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "診断結果をコピー" })).toBeEnabled();
  });

  // 2026-09-07: 照合後のclipboard拒否も、次のクリックで再照合せずにコピーできるようにする。
  it("コピー拒否時は取得済みの照合結果をクリック直後にコピーし、照合を繰り返さない", async () => {
    vi.mocked(api.connectState).mockRejectedValue(
      new KbError({ code: "core_failed", kind: "storage" }),
    );
    vi.mocked(api.planRuntimeRecovery).mockResolvedValue(recoveryPlan);
    writeText.mockRejectedValueOnce(new Error("denied"));
    show();
    fireEvent.click(await screen.findByRole("button", { name: "復元元の照合結果をコピー" }));
    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith(
        "照合結果は取得済みですが、コピーできませんでした。「取得済みの照合結果をコピー」を押してください。",
        { id: "diagnostics-progress" },
      ),
    );
    expect(toast.success).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "取得済みの照合結果をコピー" }));
    expect(writeText).toHaveBeenCalledTimes(2);
    expect(writeText).toHaveBeenLastCalledWith(JSON.stringify(recoveryPlan, null, 2));
    expect(api.planRuntimeRecovery).toHaveBeenCalledOnce();
    await waitFor(() =>
      expect(toast.success).toHaveBeenCalledWith("復元元の照合結果をコピーしました", {
        id: "diagnostics-progress",
      }),
    );
    expect(screen.getByRole("button", { name: "復元元の照合結果をコピー" })).toBeEnabled();
  });

  it("照合取得失敗は分類済みエラーを表示し、自動再試行やコピーを行わない", async () => {
    vi.mocked(api.connectState).mockRejectedValue(
      new KbError({ code: "core_failed", kind: "storage" }),
    );
    vi.mocked(api.planRuntimeRecovery).mockRejectedValue(
      new KbError({ code: "core_failed", kind: "storage" }),
    );
    show();
    fireEvent.click(await screen.findByRole("button", { name: "復元元の照合結果をコピー" }));
    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith(
        "復元元を照合できませんでした: 端末上のデータを読み書きできませんでした",
        { id: "diagnostics-progress" },
      ),
    );
    expect(api.planRuntimeRecovery).toHaveBeenCalledOnce();
    expect(writeText).not.toHaveBeenCalled();
    expect(toast.success).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "復元元の照合結果をコピー" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "診断結果をコピー" })).toBeEnabled();
  });
});

function show() {
  return render(
    <QueryClientProvider client={client}>
      <ConnectPage />
    </QueryClientProvider>,
  );
}

// 2026-09-07: DB open失敗後も !state を読込中扱いし、「確認中」から復帰できなかった。
describe("connection status loading", () => {
  it("初回取得失敗を翻訳して表示し、再試行で接続状態へ復帰する", async () => {
    const first = deferred<ConnectState>();
    vi.mocked(api.connectState).mockReturnValue(first.promise);
    show();
    expect(screen.getByRole("status")).toHaveTextContent("確認中…");
    await act(async () => {
      first.reject(new KbError({ code: "core_failed", kind: "storage" }));
      await first.promise.catch(() => undefined);
    });
    expect(await screen.findByRole("alert")).toHaveTextContent("接続状態を取得できませんでした");
    expect(screen.getByRole("alert")).toHaveTextContent("端末上のデータを読み書きできませんでした");
    expect(screen.queryByText("確認中…")).not.toBeInTheDocument();
    expect(screen.queryByText("core failed: storage")).not.toBeInTheDocument();

    const retry = deferred<ConnectState>();
    vi.mocked(api.connectState).mockReturnValue(retry.promise);
    fireEvent.click(screen.getByRole("button", { name: "再試行" }));
    await waitFor(() => expect(api.connectState).toHaveBeenCalledTimes(2));
    expect(screen.getByText("確認中…")).toBeInTheDocument();
    await act(async () => {
      retry.resolve(connected);
      await retry.promise;
    });
    expect(await screen.findByText("接続済み")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("再取得失敗を隠さず、取得済みの接続表示を保持する", async () => {
    client.setQueryData(queryKeys.connect, connected);
    vi.mocked(api.connectState).mockRejectedValue(
      new KbError({ code: "core_failed", kind: "storage" }),
    );
    show();
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "端末上のデータを読み書きできませんでした",
    );
    expect(screen.getByText("接続済み")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "再試行" })).toBeEnabled();
  });
});
