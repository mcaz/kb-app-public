import { useTranslation } from "react-i18next";

import { Button } from "@/components/atoms/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/atoms/ui/select";

import { effortLabel } from "../modelChoice";
import type { DistillationSettingsDraft } from "../useDistillationSettingsDraft";
import { distillationModelFieldsVariants } from "./variants";

interface Props {
  form: DistillationSettingsDraft;
  disabled: boolean;
}

export function DistillationModelFields({ form, disabled }: Props) {
  const { t } = useTranslation("common");
  const { value, catalog, models, selectedModel, choice, efforts, unsupportedEffort } = form;
  const styles = distillationModelFieldsVariants();
  if (!value) return null;

  return (
    <>
      <div className={styles.controls()}>
        <div className={styles.field()}>
          <label className={styles.label()} htmlFor="distillation-provider">
            {t("settings.distillation.provider")}
          </label>
          <Select
            value={value.provider ?? "unselected"}
            disabled={disabled}
            onValueChange={form.selectProvider}
          >
            <SelectTrigger id="distillation-provider" className={styles.select()}>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="unselected" disabled>
                {t("settings.distillation.chooseProvider")}
              </SelectItem>
              <SelectItem value="claude_code">Claude Code</SelectItem>
              <SelectItem value="codex">Codex</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>

      <div className={styles.controls()}>
        <div className={styles.field()}>
          <label className={styles.label()} htmlFor="distillation-model-mode">
            {t("settings.distillation.model")}
          </label>
          <Select
            value={choice}
            disabled={disabled || value.provider == null}
            onValueChange={form.selectModel}
          >
            <SelectTrigger id="distillation-model-mode" className={styles.select()}>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="default">{t("settings.distillation.defaultModel")}</SelectItem>
              {models.map((model) => (
                <SelectItem key={model.model} value={`model:${model.model}`}>
                  {model.display_name}
                </SelectItem>
              ))}
              <SelectItem value="custom">{t("settings.distillation.customModel")}</SelectItem>
            </SelectContent>
          </Select>
        </div>

        <div className={styles.field()}>
          <label className={styles.label()} htmlFor="distillation-effort">
            {t("settings.distillation.reasoningEffort")}
          </label>
          <Select
            value={value.reasoning_effort ?? "default"}
            disabled={
              disabled ||
              value.model == null ||
              (efforts.length === 0 && value.reasoning_effort == null)
            }
            onValueChange={form.selectEffort}
          >
            <SelectTrigger id="distillation-effort" className={styles.select()}>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="default">
                {selectedModel?.default_reasoning_effort
                  ? t("settings.distillation.defaultEffortWithValue", {
                      effort: effortLabel(selectedModel.default_reasoning_effort),
                    })
                  : t("settings.distillation.defaultEffort")}
              </SelectItem>
              {efforts.map((effort) => (
                <SelectItem key={effort} value={effort}>
                  {effortLabel(effort)}
                </SelectItem>
              ))}
              {value.reasoning_effort != null && !efforts.includes(value.reasoning_effort) && (
                <SelectItem value={value.reasoning_effort} disabled>
                  {t("settings.distillation.savedEffort", {
                    effort: effortLabel(value.reasoning_effort),
                  })}
                </SelectItem>
              )}
            </SelectContent>
          </Select>
          <p className={styles.description()}>
            {t(
              value.model == null
                ? "settings.distillation.effortChooseModel"
                : selectedModel == null
                  ? "settings.distillation.effortUnverified"
                  : efforts.length === 0
                    ? "settings.distillation.effortNotConfigurable"
                    : "settings.distillation.effortDescription",
            )}
          </p>
          {unsupportedEffort && (
            <p className={styles.warning()} role="status">
              {t("settings.distillation.effortUnsupported")}
            </p>
          )}
          {value.reasoning_effort === "ultra" && (
            <p className={styles.description()}>{t("settings.distillation.ultraDescription")}</p>
          )}
        </div>
      </div>

      {value.provider != null && (
        <div className={styles.actions()}>
          <p className={styles.description()} role="status">
            {t(
              catalog.isFetching
                ? "settings.distillation.modelsLoading"
                : catalog.error || catalog.data?.unavailable_reason != null || models.length === 0
                  ? "settings.distillation.modelsUnavailable"
                  : "settings.distillation.modelsDescription",
            )}
          </p>
          <Button
            type="button"
            variant="quiet"
            disabled={catalog.isFetching || disabled}
            onClick={() => void catalog.refetch()}
          >
            {t("settings.distillation.refreshModels")}
          </Button>
          {!catalog.isFetching && catalog.data?.unavailable_reason != null && (
            <p className={styles.warning()} role="status">
              {t(`settings.distillation.failures.${catalog.data.unavailable_reason}`)}
            </p>
          )}
        </div>
      )}

      {choice === "custom" && value.model != null && (
        <div className={styles.field()}>
          <label className={styles.label()} htmlFor="distillation-model">
            {t("settings.distillation.modelIdentifier")}
          </label>
          <input
            id="distillation-model"
            className={styles.input()}
            value={value.model}
            required
            maxLength={200}
            disabled={disabled}
            autoComplete="off"
            spellCheck={false}
            aria-describedby="distillation-model-description"
            onChange={(event) => form.change({ model: event.target.value, reasoning_effort: null })}
          />
          <p id="distillation-model-description" className={styles.description()}>
            {t("settings.distillation.modelDescription")}
          </p>
        </div>
      )}
    </>
  );
}
