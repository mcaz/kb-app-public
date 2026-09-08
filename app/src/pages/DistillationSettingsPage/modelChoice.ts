type ModelEntry = { model: string };

/** 一覧の未取得・モデル廃止で、保存済みの識別子を既定モデルへ置き換えない。 */
export function modelChoice(model: string | null, models: readonly ModelEntry[], custom = false) {
  if (model === null) return "default";
  if (custom || !models.some((entry) => entry.model === model)) return "custom";
  return `model:${model}`;
}

export function effortLabel(effort: string): string {
  if (effort === "xhigh") return "Extra High";
  return effort.charAt(0).toUpperCase() + effort.slice(1);
}
