import type {
  ObservationHealth,
  ObservationSurfaceHealth,
  Settings,
  SurfaceSummary,
} from "@/lib/api/types";

const emptyCounts = (surface: SurfaceSummary["surface"]): SurfaceSummary => ({
  surface,
  hook_groups: { actual_sessions: 0, daily_fallback_days: 0 },
  write_groups: { actual_sessions: 0, daily_fallback_days: 0 },
  hook_output_prepared: 0,
  hook_output_emitted: 0,
  hook_stdout_failed: 0,
  hook_filtered: 0,
  hook_errors: 0,
  emitted_chars: 0,
  emitted_bytes: 0,
  emitted_utf16_units: 0,
  emitted_documents: 0,
  trimmed_documents: 0,
  capped_outputs: 0,
  propose_successes: 0,
  propose_errors: 0,
  update_successes: 0,
  update_errors: 0,
  last_successful_propose_at_ms: null,
  write_rejections: [],
  unclassified_propose_errors: 0,
  unclassified_update_errors: 0,
});

/** 実台帳へ触れず、欠測・OFF・未帰属・過去だけの記録をブラウザで確認する見本。 */
export function demoObservationHealth(phase: string | null, settings: Settings): ObservationHealth {
  const base: ObservationHealth = {
    status: "available",
    period_days: 14,
    retention_days: 90,
    surfaces: [],
    unassigned: [],
  };
  const claudeEnabled = settings.claude_kb_enabled ?? true;
  const gptEnabled = settings.gpt_kb_enabled ?? true;
  if (phase === "disabled" || settings.ai_kb_enabled === false || (!claudeEnabled && !gptEnabled)) {
    return { ...base, status: "disabled" };
  }
  if (phase === "unavailable") return { ...base, status: "unavailable" };
  if (phase === "empty") return { ...base, status: "no_observations" };

  const codex: ObservationSurfaceHealth = {
    surface: "codex_cli",
    kb_enabled: gptEnabled,
    counts: {
      ...emptyCounts("codex_cli"),
      hook_groups: { actual_sessions: 5, daily_fallback_days: 0 },
      write_groups: { actual_sessions: 0, daily_fallback_days: 4 },
      hook_output_prepared: 1,
      hook_output_emitted: 18,
      hook_stdout_failed: 1,
      hook_errors: 1,
      emitted_chars: 21_000,
      emitted_bytes: 49_000,
      emitted_utf16_units: 21_000,
      emitted_documents: 32,
      trimmed_documents: 6,
      capped_outputs: 3,
      propose_successes: 4,
      propose_errors: 2,
      update_successes: 7,
      update_errors: 1,
      last_successful_propose_at_ms: Date.now() - 2 * 86_400_000,
      write_rejections: [
        { code: "tag_vocabulary", propose: 1, update: 0 },
        { code: "authority_scope", propose: 0, update: 1 },
      ],
      unclassified_propose_errors: 1,
    },
    last_propose_days: 2,
  };
  const claude: ObservationSurfaceHealth = {
    surface: "claude_code",
    kb_enabled: phase === "partial-off" ? false : claudeEnabled,
    counts: {
      ...emptyCounts("claude_code"),
      hook_groups: { actual_sessions: 2, daily_fallback_days: 0 },
      write_groups: { actual_sessions: 1, daily_fallback_days: 0 },
      hook_output_emitted: 8,
      emitted_chars: 13_000,
      emitted_bytes: 30_000,
      emitted_utf16_units: 13_000,
      emitted_documents: 12,
      propose_successes: 1,
      update_successes: 2,
      last_successful_propose_at_ms: Date.now(),
    },
    last_propose_days: 0,
  };
  const unassigned: ObservationSurfaceHealth[] = [
    {
      surface: "codex_cli",
      kb_enabled: gptEnabled,
      counts: {
        ...emptyCounts("codex_cli"),
        hook_groups: { actual_sessions: 1, daily_fallback_days: 0 },
        hook_filtered: 3,
        hook_errors: 1,
      },
      last_propose_days: null,
    },
  ];
  if (phase === "unassigned") return { ...base, unassigned };
  if (phase === "historical") {
    return {
      ...base,
      surfaces: [{ ...codex, counts: emptyCounts("codex_cli"), last_propose_days: 30 }],
    };
  }
  return { ...base, surfaces: [codex, claude], unassigned };
}
