import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Button } from "@/components/atoms/ui/button";
import { StatusPill } from "@/components/atoms/StatusPill";
import { useErrorText } from "@/hooks/useErrorText";
import {
  useAiGuardStatus,
  useClientDiagnostics,
  useClientRegistrations,
  useRegisterClient,
} from "@/lib/queries";

import type { ClientSurface, RegistrationClient } from "@/lib/api";

import { clientConnectionsVariants } from "./variants";

const surfaces: Record<RegistrationClient, ClientSurface> = {
  codex: "codex_cli",
  claude_code: "claude_code",
  claude_desktop: "claude_desktop",
};

export function ClientConnections() {
  const { t, i18n } = useTranslation(["connect", "common"]);
  const registrations = useClientRegistrations();
  const diagnostics = useClientDiagnostics();
  const guard = useAiGuardStatus();
  const register = useRegisterClient();
  const errorText = useErrorText();
  const styles = clientConnectionsVariants();
  const checking = registrations.isFetching || diagnostics.isFetching || guard.isFetching;
  const report = diagnostics.isError ? undefined : diagnostics.data;
  const registrationData = registrations.data;

  return (
    <section className={styles.section()} aria-labelledby="client-connections-title">
      <div className={styles.heading()}>
        <div>
          <h2 id="client-connections-title" className={styles.title()}>
            {t("clients.title")}
          </h2>
          <p className={styles.description()}>
            {registrationData
              ? t("clients.selectedKnowledgeBase", { name: registrationData.vault_name })
              : t("clients.description")}
          </p>
        </div>
        <Button
          size="sm"
          disabled={checking || register.isPending}
          onClick={() =>
            void Promise.all([registrations.refetch(), diagnostics.refetch(), guard.refetch()])
          }
        >
          {t(checking ? "common:state.checking" : "clients.recheck")}
        </Button>
      </div>

      {registrations.isError && (
        <p className={styles.error()} role="status">
          {t("clients.registrationLoadFailed", { error: errorText(registrations.error) })}
        </p>
      )}
      {(diagnostics.isError || guard.isError) && (
        <p className={styles.error()} role="status">
          {t("clients.diagnosticsLoadFailed", {
            error: errorText(diagnostics.error ?? guard.error),
          })}
        </p>
      )}
      {!registrationData && registrations.isPending && (
        <p className={styles.description()}>{t("common:state.checking")}</p>
      )}

      {registrationData && (
        <div className={styles.grid()}>
          {registrationData.clients.map(({ registration, binding }) => {
            const client = registration.client;
            const observed = report?.clients.find((entry) => entry.surface === surfaces[client]);
            const protection =
              guard.isError || !guard.data
                ? undefined
                : client === "codex"
                  ? guard.data.codex
                  : client === "claude_code"
                    ? guard.data.claude
                    : null;
            const canRegister =
              registration.can_repair ||
              (registration.state === "registered" && binding !== "matched");
            const thisPending = register.isPending && register.variables === client;
            const justRegistered = register.isSuccess && register.variables === client;
            const registrationKnown = !registrations.isError;

            return (
              <article key={client} className={styles.card()}>
                <div className={styles.cardHeading()}>
                  <h3 className={styles.title()}>{t(`clients.names.${client}`)}</h3>
                  <StatusPill tone="muted">
                    {registrationKnown
                      ? t(`clients.registrationStates.${registration.state}`)
                      : t("clients.previousCheck")}
                  </StatusPill>
                </div>
                <dl className={styles.facts()}>
                  <dt className={styles.label()}>{t("clients.capabilities")}</dt>
                  <dd className={styles.value()}>{t("clients.capabilitySummary")}</dd>
                  <dt className={styles.label()}>{t("clients.binding")}</dt>
                  <dd className={styles.value()}>
                    {registrationKnown
                      ? t(`clients.bindingStates.${binding}`)
                      : t("clients.unverified")}
                  </dd>
                  <dt className={styles.label()}>{t("clients.protection")}</dt>
                  <dd className={styles.value()}>
                    {protection === null
                      ? t("clients.protectionNotApplicable")
                      : protection === undefined
                        ? t("clients.unverified")
                        : t(`clients.guardStates.${protection}`)}
                  </dd>
                  <dt className={styles.label()}>{t("clients.ruleOutput")}</dt>
                  <dd className={styles.value()}>
                    {observed?.observations_available
                      ? t(`clients.outputStates.${observed.hook_output.state}`)
                      : t("clients.unverified")}
                  </dd>
                  <dt className={styles.label()}>{t("clients.receipt")}</dt>
                  <dd className={styles.value()}>{t("clients.unverified")}</dd>
                  <dt className={styles.label()}>{t("clients.runningVersion")}</dt>
                  <dd className={styles.value()}>{t("clients.unverified")}</dd>
                  <dt className={styles.label()}>{t("clients.operations")}</dt>
                  <dd className={styles.value()}>
                    {observed?.observations_available
                      ? t("clients.operationCounts", {
                          days: report?.window_days,
                          proposed: observed.operations.propose_successes,
                          updated: observed.operations.update_successes,
                          errors:
                            observed.operations.propose_errors + observed.operations.update_errors,
                        })
                      : t("clients.unverified")}
                  </dd>
                </dl>

                {registration.issues.length > 0 && (
                  <ul className={styles.issues()}>
                    {registration.issues.map((issue, index) => (
                      <li key={`${issue.kind}:${issue.server ?? ""}:${index}`}>
                        {t(`clients.issues.${issue.kind}`)}
                        {issue.server && <> ({issue.server})</>}
                      </li>
                    ))}
                  </ul>
                )}
                {protection && protection !== "enforced" && (
                  <p className={styles.description()}>{t("clients.protectionHint")}</p>
                )}
                {canRegister && (
                  <div className={styles.actions()}>
                    <Button
                      variant="primary"
                      size="sm"
                      disabled={register.isPending || checking || !registrationKnown}
                      onClick={() =>
                        register.mutate(client, {
                          onSuccess: () => toast(t("clients.registered")),
                          onError: (error) => toast(errorText(error)),
                        })
                      }
                    >
                      {t(
                        thisPending
                          ? "clients.registering"
                          : registration.state === "missing"
                            ? "clients.register"
                            : "clients.repair",
                      )}
                    </Button>
                  </div>
                )}
                {(justRegistered || registration.state === "registered") && (
                  <p className={styles.notice()}>
                    {t("clients.reconnect", { client: t(`clients.names.${client}`) })}
                  </p>
                )}
                {observed && (
                  <details className={styles.details()}>
                    <summary>{t("clients.details")}</summary>
                    <dl className={styles.detailList()}>
                      <div>
                        <dt>{t("clients.serverVersion")}</dt>
                        <dd>{observed.rules.server_version}</dd>
                      </div>
                      <div>
                        <dt>{t("clients.contractVersion")}</dt>
                        <dd>{observed.rules.contract_sha256}</dd>
                      </div>
                      <div>
                        <dt>{t("clients.instructionsVersion")}</dt>
                        <dd>{observed.rules.instructions_sha256}</dd>
                      </div>
                      {observed.observations_available &&
                        observed.hook_output.last_observed_at_ms !== null && (
                          <div>
                            <dt>{t("clients.lastOutput")}</dt>
                            <dd>
                              {new Date(observed.hook_output.last_observed_at_ms).toLocaleString(
                                i18n.language,
                              )}
                            </dd>
                          </div>
                        )}
                    </dl>
                  </details>
                )}
              </article>
            );
          })}
        </div>
      )}

      <p className={styles.description()}>{t("clients.evidenceNote")}</p>
      <p className={styles.description()}>{t("clients.supportedOs")}</p>
      {report && (
        <details className={styles.details()}>
          <summary>{t("clients.knowledgeBaseDetails")}</summary>
          <dl className={styles.detailList()}>
            <div>
              <dt>{t("clients.os")}</dt>
              <dd>{report.os}</dd>
            </div>
            <div>
              <dt>{t("clients.workspaceIdentity")}</dt>
              <dd>{report.workspace.workspace_id}</dd>
            </div>
            <div>
              <dt>{t("clients.vocabularySource")}</dt>
              <dd>{t(`clients.vocabularyStates.${report.workspace.vocabulary.source_status}`)}</dd>
            </div>
            <div>
              <dt>{t("clients.vocabularyNote")}</dt>
              <dd>{report.workspace.vocabulary.source_note_uid ?? t("clients.unverified")}</dd>
            </div>
            <div>
              <dt>{t("clients.vocabularyRevision")}</dt>
              <dd>{report.workspace.vocabulary.source_revision ?? t("clients.unverified")}</dd>
            </div>
            <div>
              <dt>{t("clients.vocabularyDocument")}</dt>
              <dd>
                {report.workspace.vocabulary.source_document_sha256 ?? t("clients.unverified")}
              </dd>
            </div>
            <div>
              <dt>{t("clients.lastCheck")}</dt>
              <dd>{new Date(report.observed_at_ms).toLocaleString(i18n.language)}</dd>
            </div>
          </dl>
        </details>
      )}
    </section>
  );
}
