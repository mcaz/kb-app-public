import type {
  ClientDiagnosticsReport,
  ClientRegistrations,
  ClientSurface,
  RegistrationClient,
  RegistrationRepair,
} from "@/lib/api/types";

const clients: RegistrationClient[] = ["codex", "claude_code", "claude_desktop"];
const registered = new Set<RegistrationClient>();

export function demoClientRegistrations(): ClientRegistrations {
  const allRegistered = new URLSearchParams(location.search).get("connect") === "connected";
  return {
    vault_name: "わたしのノート",
    workspace_id: "demo-workspace",
    clients: clients.map((client) => {
      const present = allRegistered || registered.has(client);
      return {
        registration: {
          client,
          state: present ? "registered" : "missing",
          issues: present ? [] : [{ kind: "missing_server", server: "kb-app-read" }],
          can_repair: !present,
        },
        binding: present ? "matched" : "missing",
      };
    }),
  };
}

export function demoRegisterClient(client: RegistrationClient): RegistrationRepair {
  registered.add(client);
  return {
    status: { client, state: "registered", issues: [], can_repair: false },
    changed: true,
    backup_path: null,
  };
}

export function demoClientDiagnostics(): ClientDiagnosticsReport {
  const surfaces: ClientSurface[] = ["codex_cli", "claude_code", "claude_desktop"];
  return {
    observed_at_ms: Date.now(),
    window_days: 30,
    os: "macos (demo)",
    workspace: {
      schema: 1,
      workspace_id: "demo-workspace",
      vocabulary: {
        source_status: "unconfigured",
        source_note_uid: null,
        source_revision: null,
        source_document_sha256: null,
      },
    },
    clients: surfaces.map((surface) => ({
      surface,
      rules: {
        schema: 1,
        contract_sha256: "demo-contract",
        instructions_sha256: "demo-instructions",
        client_surface: surface,
        tool_surface: "read",
        server_version: "0.0.1 (demo)",
      },
      hook_output: {
        state: surface === "claude_desktop" ? "not_applicable" : "not_observed",
        last_observed_at_ms: null,
      },
      receipt: "unverified",
      operations: {
        propose_successes: 0,
        update_successes: 0,
        propose_errors: 0,
        update_errors: 0,
      },
      observations_available: false,
    })),
  };
}
