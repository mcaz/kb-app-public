import type { DistillationQueueView } from "@/lib/api";

export type Metrics = NonNullable<DistillationQueueView["metrics"]>;
export type Run = Metrics["runs"][number];
export type Stage = Run["stages"][number]["stage"];

export const stageOrder: Stage[] = [
  "prepare",
  "cli_check",
  "model_catalog",
  "ai_response",
  "search",
  "validate",
  "commit",
  "export",
];

export function formatDuration(ms: number | null | undefined, locale: string, unknown: string) {
  if (ms == null || !Number.isFinite(ms) || ms < 0) return unknown;
  const unit = (value: number, name: string, digits = 0) =>
    new Intl.NumberFormat(locale, {
      style: "unit",
      unit: name,
      unitDisplay: "short",
      maximumFractionDigits: digits,
    }).format(value);
  if (ms < 1000) return unit(Math.round(ms), "millisecond");
  if (ms < 60_000) return unit(ms / 1000, "second", 1);
  const seconds = Math.floor(ms / 1000);
  if (seconds < 3600) {
    return `${unit(Math.floor(seconds / 60), "minute")} ${unit(seconds % 60, "second")}`;
  }
  return `${unit(Math.floor(seconds / 3600), "hour")} ${unit(Math.floor((seconds % 3600) / 60), "minute")} ${unit(seconds % 60, "second")}`;
}

export function stageSummary(run: Run, stage: Stage) {
  const stages = run.stages.filter((entry) => entry.stage === stage);
  const measured = stages.filter(
    (entry) => entry.finished_at_ms !== null || (run.outcome === null && entry.elapsed_is_estimate),
  );
  return {
    calls: stages.length,
    elapsed:
      measured.length === 0 ? null : measured.reduce((sum, entry) => sum + entry.elapsed_ms, 0),
    estimated: measured.some((entry) => entry.elapsed_is_estimate),
    active: run.outcome === null && stages.some((entry) => entry.finished_at_ms === null),
    interrupted:
      run.outcome === "interrupted" && stages.some((entry) => entry.finished_at_ms === null),
    failed: stages.some((entry) => entry.succeeded === false),
  };
}

export function failureCode(failure: Run["failure"]) {
  if (!failure) return null;
  return failure.code === "ai" ? `ai.${failure.kind}` : failure.code;
}
