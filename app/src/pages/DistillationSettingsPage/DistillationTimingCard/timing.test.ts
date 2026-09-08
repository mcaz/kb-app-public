import { describe, expect, it } from "vitest";

import { failureCode, formatDuration, stageSummary, type Run } from "./timing";

const run: Run = {
  run_id: "test-run",
  provider: "codex",
  model: "test-model",
  reasoning_effort: "ultra",
  attempt: 1,
  generation: 1,
  batch_size: 6,
  completed_notes: 6,
  input_bytes: 48_000,
  started_at_ms: 1000,
  finished_at_ms: 8000,
  elapsed_ms: 7000,
  elapsed_is_estimate: false,
  outcome: "no_change",
  failure: null,
  stages: [
    {
      stage: "ai_response",
      round: 1,
      started_at_ms: 2000,
      finished_at_ms: 3500,
      elapsed_ms: 1500,
      elapsed_is_estimate: false,
      succeeded: true,
    },
    {
      stage: "ai_response",
      round: 2,
      started_at_ms: 5000,
      finished_at_ms: 7000,
      elapsed_ms: 2000,
      elapsed_is_estimate: false,
      succeeded: true,
    },
  ],
};

describe("distillation timing presentation", () => {
  it("未測定と0を区別し、所要時間に応じた単位で表示する", () => {
    const format = (ms: number | null | undefined) => formatDuration(ms, "en", "unknown");
    expect(format(null)).toBe("unknown");
    expect(format(undefined)).toBe("unknown");
    expect(format(Number.NaN)).toBe("unknown");
    expect(format(-1)).toBe("unknown");
    expect(format(0)).toBe("0 ms");
    expect(format(980)).toBe("980 ms");
    expect(format(1250)).toBe("1.3 sec");
    expect(format(60_000)).toBe("1 min 0 sec");
    expect(format(3_661_000)).toBe("1 hr 1 min 1 sec");
    expect(formatDuration(60_000, "ja", "不明")).toBe("1 分 0 秒");
  });

  it("追加のAI呼び出しを集計し、実行していない工程を0にしない", () => {
    expect(stageSummary(run, "ai_response")).toEqual({
      calls: 2,
      elapsed: 3500,
      estimated: false,
      active: false,
      interrupted: false,
      failed: false,
    });
    expect(stageSummary(run, "search").elapsed).toBeNull();
    expect(run.elapsed_ms).toBe(7000);
  });

  it("同じ終了未記録の工程でも実行中と中断を分ける", () => {
    const active: Run = {
      ...run,
      outcome: null,
      finished_at_ms: null,
      stages: run.stages.map((entry) => ({
        ...entry,
        finished_at_ms: null,
        elapsed_is_estimate: true,
        succeeded: null,
      })),
    };
    expect(stageSummary(active, "ai_response").active).toBe(true);
    expect(stageSummary(active, "ai_response").interrupted).toBe(false);
    expect(stageSummary({ ...active, outcome: "interrupted" }, "ai_response").active).toBe(false);
    expect(stageSummary({ ...active, outcome: "interrupted" }, "ai_response").interrupted).toBe(
      true,
    );
  });

  it("原因コードに自由文を含めない", () => {
    expect(failureCode(null)).toBeNull();
    expect(failureCode({ code: "ai", kind: "timed_out" })).toBe("ai.timed_out");
    expect(failureCode({ code: "snapshot_changed" })).toBe("snapshot_changed");
  });

  // 2026-09-07: 中断時の未終了工程に入る0msを、計測済み時間として扱わない。
  it("中断した工程の未計測時間を除き、同工程の完了した回だけを集計する", () => {
    const interrupted: Run = {
      ...run,
      outcome: "interrupted",
      finished_at_ms: null,
      stages: run.stages.map((entry) => ({
        ...entry,
        finished_at_ms: null,
        elapsed_ms: 0,
        elapsed_is_estimate: false,
        succeeded: null,
      })),
    };
    expect(stageSummary(interrupted, "ai_response")).toMatchObject({
      elapsed: null,
      estimated: false,
      interrupted: true,
    });
    expect(
      stageSummary(
        { ...interrupted, stages: [...run.stages, ...interrupted.stages] },
        "ai_response",
      ),
    ).toMatchObject({ elapsed: 3500, estimated: false, interrupted: true });
    expect(
      stageSummary(
        {
          ...interrupted,
          outcome: null,
          stages: interrupted.stages.map((entry) => ({
            ...entry,
            elapsed_ms: 500,
            elapsed_is_estimate: true,
          })),
        },
        "ai_response",
      ),
    ).toMatchObject({ elapsed: 1000, estimated: true, interrupted: false });
  });
});
