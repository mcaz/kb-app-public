//! 端末設定。意味と永続化はkb-coreに置き、Tauriは薄い呼び出し口だけを持つ。

use kb_core::ai_guard::{AiGuardInstallError, AiGuardStatus, GuardTargetState};
use kb_core::client_binding::{self, ClientBinding};
use kb_core::client_surface::ClientSurface;
use kb_core::registry::Registry;
use kb_core::vault::Vault;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[tauri::command]
#[specta::specta]
pub fn settings_get() -> AppResult<kb_core::settings::Settings> {
    kb_core::settings::load().map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub fn settings_set_ai_kb_enabled(enabled: bool) -> AppResult<kb_core::settings::Settings> {
    kb_core::settings::set_ai_kb_enabled(enabled).map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub fn settings_set_claude_kb_enabled(enabled: bool) -> AppResult<kb_core::settings::Settings> {
    kb_core::settings::set_claude_kb_enabled(enabled).map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub fn settings_set_gpt_kb_enabled(enabled: bool) -> AppResult<kb_core::settings::Settings> {
    kb_core::settings::set_gpt_kb_enabled(enabled).map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub fn settings_set_harvest_status_line(enabled: bool) -> AppResult<kb_core::settings::Settings> {
    kb_core::settings::set_harvest_status_line(enabled).map_err(Into::into)
}

#[tauri::command(async)]
#[specta::specta]
pub fn settings_ai_guard_status(state: State<'_, AppState>) -> AppResult<AiGuardStatus> {
    let status = kb_core::ai_guard::status()?;
    let binding = selected_client_binding(&state).ok();
    Ok(guard_status_with_bindings(
        status,
        binding.as_ref(),
        client_binding::load,
    ))
}

/// macOS の管理者領域へ、Codex と Claude Code が上書きできないポリシーを置く。
/// 既存の Codex requirements は管理者の正本なので、kb-app 所有でなければ止める。
#[tauri::command(async)]
#[specta::specta]
pub fn settings_install_ai_guard(state: State<'_, AppState>) -> AppResult<AiGuardStatus> {
    let binding = selected_client_binding(&state)?;
    // 管理者認証の取消しで接続先だけを変えない。保護の導入成功後に固定する。
    kb_core::ai_guard::install().map_err(guard_install_error)?;
    bind_coding_clients(&binding, client_binding::bind)?;
    settings_ai_guard_status(state)
}

/// Codex だけを Full Access に切り替える。Codex の KB 仲介は同時に fail-closed になる。
#[tauri::command(async)]
#[specta::specta]
pub fn settings_enable_ai_guard_development_mode(
    state: State<'_, AppState>,
) -> AppResult<AiGuardStatus> {
    let status = kb_core::ai_guard::enable_development_mode().map_err(guard_install_error)?;
    let binding = selected_client_binding(&state).ok();
    Ok(guard_status_with_bindings(
        status,
        binding.as_ref(),
        client_binding::load,
    ))
}

fn guard_install_error(error: AiGuardInstallError) -> AppError {
    match error {
        AiGuardInstallError::Conflict => AppError::AiGuardPolicyConflict,
        AiGuardInstallError::StrictModeRequired => AppError::AiGuardStrictModeRequired,
        AiGuardInstallError::Unsupported => AppError::AiGuardUnsupported,
        AiGuardInstallError::Failed(_) => AppError::AiGuardInstallFailed,
    }
}

/// GUIのキャッシュ済みVaultと、毎回変わり得る既定名を別々に結び付けない。
pub(super) fn selected_client_binding(state: &AppState) -> AppResult<ClientBinding> {
    state.with_vault(|vault| {
        let registry = Registry::load().map_err(AppError::configuration)?;
        binding_for_vault(vault, &registry)
    })
}

fn binding_for_vault(vault: &Vault, registry: &Registry) -> AppResult<ClientBinding> {
    let name = registry
        .vaults
        .iter()
        .find(|entry| entry.path == vault.root)
        .map(|entry| entry.name.clone())
        .ok_or(AppError::VaultUnavailable)?;
    let workspace_id = kb_core::workspace::stored_workspace_id(vault).map_err(AppError::storage)?;
    ClientBinding::new(name, workspace_id).map_err(Into::into)
}

pub(super) fn binding_matches(actual: &ClientBinding, expected: &ClientBinding) -> bool {
    actual.vault_name == expected.vault_name && actual.workspace_id == expected.workspace_id
}

/// 再導入の案内はGUIだけに重ねる。OS guard判定へ混ぜると、MCPの未検証状態が
/// kb_disabledへ変わり、旧接続の互換経路とhookの劣化報告が失われる。
fn guard_status_with_bindings(
    mut status: AiGuardStatus,
    expected: Option<&ClientBinding>,
    mut load: impl FnMut(ClientSurface) -> kb_core::error::Result<Option<ClientBinding>>,
) -> AiGuardStatus {
    for (target, surface) in [
        (&mut status.codex, ClientSurface::CodexCli),
        (&mut status.claude, ClientSurface::ClaudeCode),
    ] {
        if *target == GuardTargetState::Enforced
            && expected.is_none_or(|expected| {
                load(surface)
                    .ok()
                    .flatten()
                    .is_none_or(|actual| !binding_matches(&actual, expected))
            })
        {
            *target = GuardTargetState::Outdated;
        }
    }
    status.ready &=
        status.codex == GuardTargetState::Enforced && status.claude == GuardTargetState::Enforced;
    status
}

fn bind_coding_clients(
    binding: &ClientBinding,
    mut bind: impl FnMut(ClientSurface, &ClientBinding) -> kb_core::error::Result<()>,
) -> AppResult<()> {
    for surface in [ClientSurface::CodexCli, ClientSurface::ClaudeCode] {
        bind(surface, binding)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const OTHER_WORKSPACE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

    fn binding(name: &str, workspace: &str) -> ClientBinding {
        ClientBinding::new(name.into(), workspace.into()).unwrap()
    }

    fn enforced() -> AiGuardStatus {
        AiGuardStatus {
            ready: true,
            codex: GuardTargetState::Enforced,
            claude: GuardTargetState::Enforced,
            guarded_paths: Vec::new(),
        }
    }

    /// 2026-09-05: GUIが保持するVaultとRegistryの既定がずれても、別Vaultの名を付けない。
    #[test]
    fn selected_binding_uses_the_open_vault_instead_of_the_new_default() {
        let temp = tempfile::tempdir().unwrap();
        let first = Vault::create(temp.path().join("first")).unwrap();
        let second = Vault::create(temp.path().join("second")).unwrap();
        let mut registry = Registry::default();
        registry.add("first", first.root.clone()).unwrap();
        registry.add("second", second.root.clone()).unwrap();
        registry.default = Some("second".into());

        let selected = binding_for_vault(&first, &registry).unwrap();
        assert_eq!(selected.vault_name, "first");
        assert_eq!(
            selected.workspace_id,
            kb_core::workspace::stored_workspace_id(&first).unwrap()
        );
        assert_ne!(
            selected.workspace_id,
            kb_core::workspace::stored_workspace_id(&second).unwrap()
        );

        registry.vaults.retain(|entry| entry.name != "first");
        assert!(binding_for_vault(&first, &registry).is_err());
    }

    #[test]
    fn registering_does_not_repair_or_invent_an_unknown_workspace_id() {
        let temp = tempfile::tempdir().unwrap();
        let vault = Vault::create(temp.path().join("vault")).unwrap();
        let mut registry = Registry::default();
        registry.add("selected", vault.root.clone()).unwrap();
        let id_file = vault.root.join(kb_core::workspace::ID_FILE);
        std::fs::write(&id_file, "broken\n").unwrap();

        assert!(binding_for_vault(&vault, &registry).is_err());
        assert_eq!(std::fs::read_to_string(id_file).unwrap(), "broken\n");
    }

    /// UIの再導入案内を重ねるだけで、元のOS guard判定を変更しない。
    #[test]
    fn guard_overlay_requires_both_client_bindings_for_the_selected_vault() {
        let expected = binding("selected", WORKSPACE);
        let original = enforced();
        let ready = guard_status_with_bindings(original.clone(), Some(&expected), |_| {
            Ok(Some(expected.clone()))
        });
        assert!(ready.ready);

        for stale in [
            None,
            Some(binding("old-name", WORKSPACE)),
            Some(binding("selected", OTHER_WORKSPACE)),
        ] {
            let status = guard_status_with_bindings(original.clone(), Some(&expected), |surface| {
                Ok(if surface == ClientSurface::CodexCli {
                    stale.clone()
                } else {
                    Some(expected.clone())
                })
            });
            assert_eq!(status.codex, GuardTargetState::Outdated);
            assert_eq!(status.claude, GuardTargetState::Enforced);
            assert!(!status.ready);
        }
        assert!(original.ready);
        assert_eq!(original.codex, GuardTargetState::Enforced);
    }

    #[test]
    fn unreadable_binding_and_unknown_selection_remain_repairable() {
        let expected = binding("selected", WORKSPACE);
        let failed = guard_status_with_bindings(enforced(), Some(&expected), |_| {
            Err(kb_core::error::CoreError::configuration(anyhow::anyhow!(
                "fixture"
            )))
        });
        assert_eq!(failed.codex, GuardTargetState::Outdated);
        assert_eq!(failed.claude, GuardTargetState::Outdated);
        assert!(!failed.ready);

        let unknown = guard_status_with_bindings(enforced(), None, |_| {
            panic!("選択先が不明ならbindingを読む必要はない")
        });
        assert_eq!(unknown.codex, GuardTargetState::Outdated);
        assert!(!unknown.ready);
    }

    #[test]
    fn binding_overlay_preserves_development_conflict_and_other_os_states() {
        let expected = binding("selected", WORKSPACE);
        for codex in [
            GuardTargetState::Development,
            GuardTargetState::Missing,
            GuardTargetState::Outdated,
            GuardTargetState::Conflict,
            GuardTargetState::Unsupported,
        ] {
            let status = guard_status_with_bindings(
                AiGuardStatus {
                    ready: false,
                    codex,
                    ..enforced()
                },
                Some(&expected),
                |surface| {
                    assert_eq!(surface, ClientSurface::ClaudeCode);
                    Ok(Some(expected.clone()))
                },
            );
            assert_eq!(status.codex, codex);
            assert_eq!(status.claude, GuardTargetState::Enforced);
            assert!(!status.ready);
        }
    }

    #[test]
    fn a_partial_binding_write_is_an_error_and_only_targets_coding_clients() {
        let expected = binding("selected", WORKSPACE);
        let mut saved = Vec::new();
        let result = bind_coding_clients(&expected, |surface, actual| {
            assert!(binding_matches(actual, &expected));
            saved.push(surface);
            if surface == ClientSurface::ClaudeCode {
                return Err(kb_core::error::CoreError::configuration(anyhow::anyhow!(
                    "fixture"
                )));
            }
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(saved, [ClientSurface::CodexCli, ClientSurface::ClaudeCode]);
    }
}
