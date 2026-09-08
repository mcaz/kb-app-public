import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { toast } from "sonner";

import { setupI18n } from "@/i18n";

import { DistillationTimingCard } from "./DistillationTimingCard";
import type { Metrics, Run } from "./timing";

vi.mock("sonner", () => ({ toast: vi.fn() }));

const run: Run = {
  run_id: "test-run",
  provider: "codex",
  model: "test-model",
  reasoning_effort: "ultra",
  attempt: 2,
  generation: 3,
  batch_size: 6,
  completed_notes: 6,
  input_bytes: 48_000,
  started_at_ms: 1_700_000_000_000,
  finished_at_ms: 1_700_000_030_000,
  elapsed_ms: 30_000,
  elapsed_is_estimate: false,
  outcome: "no_change",
  failure: null,
  stages: [
    {
      stage: "ai_response",
      round: 1,
      started_at_ms: 1_700_000_005_000,
      finished_at_ms: 1_700_000_010_000,
      elapsed_ms: 5000,
      elapsed_is_estimate: false,
      succeeded: true,
    },
  ],
};

const originalClipboard = Object.getOwnPropertyDescriptor(navigator, "clipboard");
const writeText = vi.fn<(text: string) => Promise<void>>();
beforeEach(() => {
  setupI18n("ja");
  writeText.mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText },
  });
});
afterEach(() => {
  cleanup();
  vi.resetAllMocks();
  if (originalClipboard) Object.defineProperty(navigator, "clipboard", originalClipboard);
  else Reflect.deleteProperty(navigator, "clipboard");
});

const show = (metrics: Metrics | null = { available: true, runs: [run] }) =>
  render(<DistillationTimingCard metrics={metrics} paused={false} failed={false} />);

describe("distillation timing status and sharing", () => {
  it("未計測の過去を0秒の成功として表示しない", () => {
    show({ available: true, runs: [] });
    expect(screen.getByText(/まだ計測記録はありません/)).toBeInTheDocument();
    expect(screen.queryByText("変更なしで完了")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "計測結果をコピー" })).toBeDisabled();
  });

  it("試行全体と工程内訳を別々に表示し、CLI準備をAI応答へ合算しない", () => {
    show();
    const details = screen.getByText("変更なしで完了").closest("details");
    expect(details).not.toBeNull();
    fireEvent.click(screen.getByText("変更なしで完了"));
    expect(details?.querySelector("summary")).toHaveTextContent("30 秒");
    expect(screen.getByText("AI呼び出し").nextElementSibling).toHaveTextContent("5 秒");
    expect(screen.getByText("設定・CLIの確認").nextElementSibling).toHaveTextContent("未計測");
    expect(screen.getByText(/AI呼び出し 1回（開始済み）/)).toBeInTheDocument();
    expect(details?.querySelector("summary")).toHaveTextContent("対象6件");
    expect(screen.getByText(/確認完了: 6 \/ 6件/)).toHaveTextContent("最大入力（バイト）: 48,000");
  });

  it("旧履歴の完了件数と入力サイズをゼロと表示しない", () => {
    show({
      available: true,
      runs: [{ ...run, batch_size: 1, completed_notes: null, input_bytes: null }],
    });
    expect(screen.getByText(/確認完了: 未記録 \/ 1件/)).toHaveTextContent(
      "最大入力（バイト）: 未記録",
    );
    expect(screen.queryByText(/最大入力（バイト）: 0/)).not.toBeInTheDocument();
  });

  it("失敗・中断・進行中を完了から区別し、進行中の工程を更新する", () => {
    const active: Run = {
      ...run,
      run_id: "active",
      outcome: null,
      finished_at_ms: null,
      elapsed_is_estimate: true,
      completed_notes: 0,
      stages: run.stages.map((stage) => ({
        ...stage,
        finished_at_ms: null,
        elapsed_is_estimate: true,
        succeeded: null,
      })),
    };
    const view = show({
      available: true,
      runs: [
        active,
        {
          ...run,
          run_id: "failed",
          outcome: "retry_wait",
          failure: { code: "ai", kind: "timed_out" },
        },
        { ...active, run_id: "interrupted", outcome: "interrupted", elapsed_is_estimate: false },
        run,
      ],
    });
    expect(screen.getByText("現在の工程（記録上）: AI呼び出し")).toBeInTheDocument();
    expect(screen.getByText("失敗・再試行待ち")).toBeInTheDocument();
    expect(screen.getByText("原因コード: ai.timed_out")).toBeInTheDocument();
    expect(screen.getByText("中断・終了未記録")).toBeInTheDocument();
    expect(screen.getByText("最終所要時間は不明（記録済み 30 秒）")).toBeInTheDocument();
    expect(screen.getAllByText("変更なしで完了")).toHaveLength(1);
    view.rerender(
      <DistillationTimingCard
        metrics={{
          available: true,
          runs: [
            {
              ...active,
              elapsed_ms: 33_000,
              stages: active.stages.map((stage) => ({ ...stage, stage: "search" })),
            },
          ],
        }}
        paused={false}
        failed={false}
      />,
    );
    expect(screen.getByText("現在の工程（記録上）: 追加検索")).toBeInTheDocument();
    expect(screen.getByText("約33 秒")).toBeInTheDocument();
  });

  it("取得が劣化した記録には注意を付け、KB停止中には内容を隠す", () => {
    const view = show({ available: false, runs: [run] });
    expect(screen.getByRole("alert")).toHaveTextContent("古い可能性");
    expect(screen.getByText("変更なしで完了")).toBeInTheDocument();
    view.rerender(
      <DistillationTimingCard metrics={{ available: true, runs: [run] }} paused={false} failed />,
    );
    expect(screen.getByRole("alert")).toHaveTextContent("古い可能性");
    view.rerender(
      <DistillationTimingCard metrics={{ available: true, runs: [run] }} paused failed={false} />,
    );
    expect(screen.queryByText("変更なしで完了")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "計測結果をコピー" })).toBeDisabled();
  });

  it("計測のメタデータだけをJSONでコピーする", async () => {
    const metrics = { available: true, runs: [run] };
    show(metrics);
    fireEvent.click(screen.getByRole("button", { name: "計測結果をコピー" }));
    await waitFor(() => expect(toast).toHaveBeenCalledWith("計測結果をコピーしました"));
    expect(writeText).toHaveBeenCalledWith(JSON.stringify(metrics, null, 2));
  });

  it("コピー失敗時は診断の自由文を表示しない", async () => {
    show();
    writeText.mockRejectedValue(new Error("private diagnostic"));
    fireEvent.click(screen.getByRole("button", { name: "計測結果をコピー" }));
    await waitFor(() =>
      expect(toast).toHaveBeenCalledWith("コピーできませんでした。もう一度試してください。"),
    );
    expect(screen.queryByText("private diagnostic")).not.toBeInTheDocument();
  });

  it("最大20試行を表示する", () => {
    show({
      available: true,
      runs: Array.from({ length: 21 }, (_, index) => ({ ...run, run_id: `${index}` })),
    });
    expect(
      within(screen.getByRole("region", { name: "実行時間の内訳" })).getAllByText("変更なしで完了"),
    ).toHaveLength(20);
  });
});
