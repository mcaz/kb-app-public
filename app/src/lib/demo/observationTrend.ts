import type { ObservationTrend, ObservationTrendFilter, Settings } from "@/lib/api/types";

/** 日付だけを現在に合わせた合成データ。実台帳やノートは読まない。 */
export function demoObservationTrend(
  dayBoundariesMs: number[],
  phase: string | null,
  settings: Settings,
  filter: ObservationTrendFilter,
): ObservationTrend {
  if (
    phase === "disabled" ||
    settings.ai_kb_enabled === false ||
    (settings.claude_kb_enabled === false && settings.gpt_kb_enabled === false)
  ) {
    return { status: "disabled", days: [] };
  }
  if (phase === "unavailable") return { status: "unavailable", days: [] };
  if (
    ["empty", "historical", "unassigned"].includes(phase ?? "") ||
    (phase === "claude-only" && filter === "gpt") ||
    (phase === "gpt-only" && filter === "claude") ||
    (phase === "other-only" && filter !== "all")
  ) {
    return { status: "no_observations", days: [] };
  }
  const outputs = [2, 0, 3, 1, 0, 2, 4, 1, 2, 0, 3, 2, 5, 1];
  return {
    status: "available",
    days: dayBoundariesMs.slice(0, -1).map((start, index) => {
      const claude =
        filter !== "gpt" && phase !== "gpt-only" && phase !== "other-only" && phase !== "pending";
      const gpt =
        filter !== "claude" &&
        phase !== "claude-only" &&
        phase !== "other-only" &&
        phase !== "pending";
      const other =
        filter === "all" && !["claude-only", "gpt-only", "pending"].includes(phase ?? "");
      return {
        start_ms: start,
        end_ms: Math.min(dayBoundariesMs[index + 1]!, Date.now()),
        hook_output_emitted:
          (claude ? (outputs[index] ?? 0) : 0) + (gpt ? (index % 4 === 0 ? 2 : 1) : 0),
        propose_successes:
          Number(claude && index % 3 === 0) +
          Number(gpt && index % 4 === 1) +
          Number(other && index === 5),
        update_successes: Number(claude && index % 2 === 0) + Number(gpt && index % 3 === 1),
        errors: Number(claude && index % 5 === 0) + Number(gpt && index === 11),
      };
    }),
  };
}
