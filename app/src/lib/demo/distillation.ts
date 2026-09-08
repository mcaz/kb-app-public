import type {
  DistillationAiSettings,
  DistillationAiProvider,
  DistillationAiProviderStatus,
  DistillationModelCatalog,
  DistillationQueueView,
  Settings,
  ImmediateDistillationScope,
  ImmediateDistillationResult,
} from "@/lib/api/types";
import { KbError } from "@/lib/api/error";

let settings: DistillationAiSettings = {
  enabled: false,
  provider: null,
  model: null,
  reasoning_effort: null,
  timeout_seconds: 300,
  periodic_hours: 168,
};

let waiting = 2;
let pending = 3;
let completed = 18;
const measuredAt = Date.now() - 90_000;

export const demoDistillation = {
  distillationSettingsGet: (): Promise<DistillationAiSettings> => Promise.resolve(settings),
  distillationSettingsSet: (next: DistillationAiSettings): Promise<DistillationAiSettings> => {
    settings = { ...next };
    return Promise.resolve(settings);
  },
  distillationProviders: (): Promise<DistillationAiProviderStatus[]> =>
    Promise.resolve([
      { provider: "claude_code", installed: true, unavailable_reason: null },
      { provider: "codex", installed: true, unavailable_reason: null },
    ]),
  distillationModels: (provider: DistillationAiProvider): Promise<DistillationModelCatalog> =>
    Promise.resolve({
      provider,
      unavailable_reason: null,
      models:
        provider === "codex"
          ? [
              {
                model: "gpt-6-astra",
                display_name: "Astra",
                supported_reasoning_efforts: ["low", "medium", "high", "xhigh", "max", "ultra"],
                default_reasoning_effort: "medium",
                is_default: true,
              },
            ]
          : [
              {
                model: "claude-demo",
                display_name: "Claude Demo",
                supported_reasoning_efforts: ["low", "medium", "high", "max"],
                default_reasoning_effort: null,
                is_default: false,
              },
            ],
    }),
  distillationQueueStatus: (kb: Settings): Promise<DistillationQueueView> => {
    const paused =
      !kb.ai_kb_enabled ||
      settings.provider == null ||
      (settings.provider === "claude_code" ? !kb.claude_kb_enabled : !kb.gpt_kb_enabled);
    return Promise.resolve({
      paused,
      metrics: paused
        ? null
        : {
            available: true,
            runs: [
              {
                run_id: "demo-no-change",
                provider: settings.provider ?? null,
                model: settings.model ?? null,
                reasoning_effort: settings.reasoning_effort ?? null,
                attempt: 1,
                generation: 1,
                batch_size: 6,
                completed_notes: 6,
                input_bytes: 48_000,
                started_at_ms: measuredAt,
                finished_at_ms: measuredAt + 42_000,
                elapsed_ms: 42_000,
                elapsed_is_estimate: false,
                outcome: "no_change",
                failure: null,
                stages: [
                  {
                    stage: "prepare",
                    round: 0,
                    started_at_ms: measuredAt,
                    finished_at_ms: measuredAt + 350,
                    elapsed_ms: 350,
                    elapsed_is_estimate: false,
                    succeeded: true,
                  },
                  {
                    stage: "cli_check",
                    round: 1,
                    started_at_ms: measuredAt + 350,
                    finished_at_ms: measuredAt + 900,
                    elapsed_ms: 550,
                    elapsed_is_estimate: false,
                    succeeded: true,
                  },
                  {
                    stage: "ai_response",
                    round: 1,
                    started_at_ms: measuredAt + 900,
                    finished_at_ms: measuredAt + 40_000,
                    elapsed_ms: 39_100,
                    elapsed_is_estimate: false,
                    succeeded: true,
                  },
                  {
                    stage: "validate",
                    round: 0,
                    started_at_ms: measuredAt + 40_000,
                    finished_at_ms: measuredAt + 41_800,
                    elapsed_ms: 1800,
                    elapsed_is_estimate: false,
                    succeeded: true,
                  },
                  {
                    stage: "commit",
                    round: 0,
                    started_at_ms: measuredAt + 41_800,
                    finished_at_ms: measuredAt + 41_900,
                    elapsed_ms: 100,
                    elapsed_is_estimate: false,
                    succeeded: true,
                  },
                ],
              },
            ],
          },
      issues:
        paused || waiting === 0
          ? []
          : [
              {
                note: "notes/引っ越し手続きメモ",
                title: "引っ越し手続きメモ",
                state: "retry_wait",
                error: { code: "ai", kind: "timed_out" },
                available_at: Math.floor(Date.now() / 1000) + 300,
                attempt: 1,
              },
              {
                note: "notes/確定申告の準備",
                title: "確定申告の準備",
                state: "retry_wait",
                error: { code: "ai", kind: "process_failed" },
                available_at: Math.floor(Date.now() / 1000) + 600,
                attempt: 2,
              },
            ],
      jobs: paused
        ? null
        : {
            pending,
            running: 0,
            retry_wait: waiting,
            blocked: 0,
            completed,
            oldest_pending_at: null,
          },
    });
  },
  distillationRetryFailed: (kb: Settings): Promise<DistillationQueueView> => {
    if (kb.ai_kb_enabled) {
      pending += waiting;
      waiting = 0;
    }
    return demoDistillation.distillationQueueStatus(kb);
  },
  distillationRequestNow: async (
    scope: ImmediateDistillationScope,
    kb: Settings,
  ): Promise<ImmediateDistillationResult> => {
    const before = await demoDistillation.distillationQueueStatus(kb);
    if (!settings.enabled || before.paused || !before.jobs) {
      throw new KbError({ code: "core_failed", kind: "configuration" });
    }
    const requeued = waiting + (scope === "all" ? completed : 0);
    pending += requeued;
    waiting = 0;
    if (scope === "all") completed = 0;
    return {
      registered: 0,
      requeued,
      expedited: 0,
      jobs: { ...before.jobs, pending, completed, retry_wait: 0, blocked: 0 },
    };
  },
};
