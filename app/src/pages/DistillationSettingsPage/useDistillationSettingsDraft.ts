import { useState } from "react";

import type { DistillationAiSettings } from "@/lib/api";
import { useDistillationModels } from "@/lib/queries";

import { modelChoice } from "./modelChoice";

/** 候補一覧は表示の補助に留め、保存済み設定へ非同期の取得結果を書き戻さない。 */
export function useDistillationSettingsDraft(saved: DistillationAiSettings | undefined) {
  const [draft, setDraft] = useState<DistillationAiSettings | null>(null);
  const [customModel, setCustomModel] = useState(false);
  const value = draft ?? saved;
  const catalog = useDistillationModels(value?.provider ?? null);
  const models = catalog.data?.models ?? [];
  const selectedModel = models.find((candidate) => candidate.model === value?.model);
  const choice = modelChoice(value?.model ?? null, models, customModel);
  const efforts = selectedModel?.supported_reasoning_efforts ?? [];
  const unsupportedEffort =
    selectedModel != null &&
    value?.reasoning_effort != null &&
    !efforts.includes(value.reasoning_effort);

  function change(patch: Partial<DistillationAiSettings>) {
    if (value) setDraft({ ...value, ...patch });
  }

  function reset() {
    setDraft(null);
    setCustomModel(false);
  }

  function selectProvider(next: string) {
    if ((next === "claude_code" || next === "codex") && next !== value?.provider) {
      setCustomModel(false);
      change({ provider: next, model: null, reasoning_effort: null });
    }
  }

  function selectModel(next: string) {
    // 2026-09-07: 候補の非同期登録中にRadixのnative selectが空値を通知する。
    // 画面を開くだけで保存済みモデル・推論レベルを未保存の空欄にしない。
    if (
      next === choice ||
      (next !== "default" &&
        next !== "custom" &&
        !models.some((model) => next === `model:${model.model}`))
    ) {
      return;
    }
    setCustomModel(next === "custom");
    const model = next === "default" ? null : next === "custom" ? "" : next.slice(6);
    if (model !== value?.model) change({ model, reasoning_effort: null });
  }

  function selectEffort(next: string) {
    if (
      next === (value?.reasoning_effort ?? "default") ||
      (next !== "default" && !efforts.includes(next))
    ) {
      return;
    }
    change({ reasoning_effort: next === "default" ? null : next });
  }

  return {
    value,
    dirty: draft !== null,
    catalog,
    models,
    selectedModel,
    choice,
    efforts,
    unsupportedEffort,
    change,
    reset,
    selectProvider,
    selectModel,
    selectEffort,
  };
}

export type DistillationSettingsDraft = ReturnType<typeof useDistillationSettingsDraft>;
