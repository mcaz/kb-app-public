import { QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { setupI18n } from "@/i18n";
import { api, type RuntimeRecoveryPlan, type RuntimeRecoveryReceipt } from "@/lib/api";
import { createQueryClient } from "@/lib/queryClient";

import { StorageRecoveryPage } from "./StorageRecoveryPage";

vi.mock(import("@/lib/api"), async (original) => {
  const actual = await original();
  return {
    ...actual,
    api: {
      ...actual.api,
      recoveryPlan: vi.fn(),
      recoveryApply: vi.fn(),
      recoveryExit: vi.fn(),
      setupState: vi.fn(),
      maintenanceRefresh: vi.fn(),
      homeState: vi.fn(),
    },
  };
});

const plan: RuntimeRecoveryPlan = {
  format_version: 1,
  read_only: true,
  recovery_performed: false,
  atomic_snapshot: false,
  declared_schema: 7,
  supported_reset_shape: true,
  snapshot_complete: true,
  plan_digest: "sha256:fixture-plan",
  existing_markdown_coverage_complete: true,
  latest_state_proven: false,
  markdown_notes: 174,
  job_notes: 173,
  history_runs: 331,
  summary: {
    markdown_with_job: 173,
    markdown_without_job: 1,
    eligible_markdown: 173,
    eligible_markdown_without_job: 0,
    jobs_without_markdown: 0,
    completed_review_matches: 159,
    completed_review_conflicts: 0,
    previous_review_matches: 0,
    history_after_matches: 159,
    history_before_matches: 0,
    no_matching_history: 15,
    latest_state_unproven: 15,
    current_review_available_in_history: 0,
    historical_only_notes: 0,
  },
  notes: Array.from({ length: 15 }, (_, index) => ({
    note: `notes/unverified-${index + 1}`,
    markdown_present: true,
    eligible: index < 14,
    job_present: index < 14,
    job_state: index < 14 ? "blocked" : null,
    reviewed_hash_matches_markdown: null,
    history_after_matches_markdown: false,
    history_before_matches_markdown: false,
    history_after_versions: 0,
    history_before_versions: 0,
    current_review_source: "unproven",
    freshness: "no_latest_version_evidence",
  })),
  note_count: 174,
  notes_truncated: false,
  issues: [],
  issue_count: 0,
  blocking_issue_count: 0,
  issues_truncated: false,
};
const receipt: RuntimeRecoveryReceipt = {
  backup_id: "recovery-fixture",
  plan_digest: "sha256:fixture-plan",
  restored_notes: 174,
  verified_review_notes: 159,
  unproven_notes: 15,
  backup_verified: true,
  preserved_ledgers: [
    { table: "distillation_job_runs", rows: 331, sha256: "sha256:fixture-ledger" },
  ],
  automatic_processing_resumed: false,
  restart_required: true,
};
const writeText = vi.fn<(text: string) => Promise<void>>();

beforeEach(() => {
  setupI18n("ja");
  vi.resetAllMocks();
  vi.mocked(api.recoveryPlan).mockResolvedValue(plan);
  vi.mocked(api.recoveryApply).mockResolvedValue(receipt);
  writeText.mockResolvedValue();
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
});
afterEach(cleanup);

function show() {
  return render(
    <QueryClientProvider client={createQueryClient()}>
      <StorageRecoveryPage />
    </QueryClientProvider>,
  );
}
async function compare() {
  fireEvent.click(screen.getByRole("button", { name: "復元元を照合" }));
  await screen.findByRole("heading", { name: "照合結果" });
}

describe("isolated recovery workflow", () => {
  // 2026-09-07: 専用画面の起動自体からDB照合や通常保守を開始しない。
  it("明示照合まで処理せず、未証明ノートの確認後に表示したdigestだけを適用する", async () => {
    show();
    expect(api.recoveryPlan).not.toHaveBeenCalled();
    expect(api.recoveryApply).not.toHaveBeenCalled();
    expect(api.setupState).not.toHaveBeenCalled();
    expect(api.homeState).not.toHaveBeenCalled();
    expect(api.maintenanceRefresh).not.toHaveBeenCalled();
    await compare();
    expect(screen.getByText("159件")).toBeInTheDocument();
    expect(screen.getByText("15件")).toBeInTheDocument();
    expect(screen.getByText("331件")).toBeInTheDocument();
    expect(screen.getByText("0件")).toBeInTheDocument();
    expect(screen.getByText("notes/unverified-15")).toBeInTheDocument();
    const apply = screen.getByRole("button", { name: "退避して174件を復旧" });
    expect(apply).toBeDisabled();
    fireEvent.click(screen.getByRole("checkbox"));
    expect(apply).toBeEnabled();
    fireEvent.click(apply);
    expect(await screen.findByRole("heading", { name: "復旧が完了しました" })).toBeInTheDocument();
    expect(api.recoveryApply).toHaveBeenCalledOnce();
    expect(vi.mocked(api.recoveryApply).mock.calls[0]?.[0]).toEqual({
      expected_plan_digest: "sha256:fixture-plan",
      acknowledge_unproven: true,
    });
    expect(screen.queryByRole("button", { name: /退避して.*件を復旧/ })).not.toBeInTheDocument();
    expect(api.setupState).not.toHaveBeenCalled();
    expect(api.maintenanceRefresh).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "結果をコピー" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith(JSON.stringify(receipt, null, 2)));
    await screen.findByText("結果をコピーしました");
    fireEvent.click(screen.getByRole("button", { name: "終了" }));
    await waitFor(() => expect(api.recoveryExit).toHaveBeenCalledOnce());
  });

  // 2026-09-07: apply結果が不明でも同じ計画を連続実行せず、再照合と再確認を要求する。
  it("実行中は重複実行や終了を止め、失敗後は再照合まで再適用しない", async () => {
    let reject!: (error: Error) => void;
    const pending = new Promise<RuntimeRecoveryReceipt>((_resolve, fail) => {
      reject = fail;
    });
    vi.mocked(api.recoveryApply).mockReturnValueOnce(pending);
    show();
    await compare();
    fireEvent.click(screen.getByRole("checkbox"));
    fireEvent.click(screen.getByRole("button", { name: "退避して174件を復旧" }));
    expect(screen.getByRole("button", { name: "退避して復旧しています…" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "もう一度照合" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "終了" })).toBeDisabled();
    await act(async () => {
      reject(new Error("private failure"));
      await pending.catch(() => undefined);
    });
    await screen.findByText(/復旧の完了を確認できませんでした/);
    expect(screen.queryByText(/private failure/)).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "退避して174件を復旧" }));
    expect(api.recoveryApply).toHaveBeenCalledOnce();
    vi.mocked(api.recoveryPlan).mockResolvedValueOnce({
      ...plan,
      plan_digest: "sha256:fresh-plan",
    });
    fireEvent.click(screen.getByRole("button", { name: "もう一度照合" }));
    await screen.findByRole("heading", { name: "照合結果" });
    expect(screen.getByRole("checkbox")).not.toBeChecked();
    expect(screen.getByRole("button", { name: "退避して174件を復旧" })).toBeDisabled();
    fireEvent.click(screen.getByRole("checkbox"));
    fireEvent.click(screen.getByRole("button", { name: "退避して174件を復旧" }));
    await screen.findByRole("heading", { name: "復旧が完了しました" });
    expect(vi.mocked(api.recoveryApply).mock.calls.at(-1)?.[0]).toEqual({
      expected_plan_digest: "sha256:fresh-plan",
      acknowledge_unproven: true,
    });
  });

  it.each([
    { supported_reset_shape: false },
    { snapshot_complete: false },
    { existing_markdown_coverage_complete: false },
    { blocking_issue_count: 1 },
    { plan_digest: null },
  ])("不完全または拒否された照合ではapplyできない: %j", async (override) => {
    vi.mocked(api.recoveryPlan).mockResolvedValueOnce({ ...plan, ...override });
    show();
    await compare();
    expect(screen.getByRole("button", { name: "退避して174件を復旧" })).toBeDisabled();
    expect(screen.getByRole("checkbox")).toBeDisabled();
    expect(api.recoveryApply).not.toHaveBeenCalled();
  });

  it("照合で不明な件数は0にせず未確認と表示する", async () => {
    vi.mocked(api.recoveryPlan).mockResolvedValueOnce({
      ...plan,
      snapshot_complete: false,
      markdown_notes: null,
      history_runs: null,
      summary: null,
      plan_digest: null,
    });
    show();
    await compare();
    expect(screen.getAllByText("未確認")).toHaveLength(4);
    expect(screen.getByRole("button", { name: "退避して復旧" })).toBeDisabled();
    expect(screen.queryByRole("checkbox")).not.toBeInTheDocument();
  });

  it("未証明ノートが0件なら追加checkboxを要求しない", async () => {
    vi.mocked(api.recoveryPlan).mockResolvedValueOnce({
      ...plan,
      latest_state_proven: true,
      summary: { ...plan.summary!, latest_state_unproven: 0, completed_review_matches: 174 },
    });
    show();
    await compare();
    expect(screen.queryByRole("checkbox")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "退避して174件を復旧" })).toBeEnabled();
  });

  it("照合失敗では自動再試行せず結果も表示しない", async () => {
    vi.mocked(api.recoveryPlan).mockRejectedValueOnce(new Error("private detail"));
    show();
    fireEvent.click(screen.getByRole("button", { name: "復元元を照合" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("復元元を照合できませんでした");
    expect(api.recoveryPlan).toHaveBeenCalledOnce();
    expect(screen.queryByRole("heading", { name: "照合結果" })).not.toBeInTheDocument();
    expect(screen.queryByText(/private detail/)).not.toBeInTheDocument();
  });
});
