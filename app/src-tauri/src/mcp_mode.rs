//! `kb-app --mcp --vault <name> [--client <actor>] [--mcp-surface <surface>]
//! [--retrieval-profile <profile>]` で GUI を開かず MCP サーバーとして動く。
//!
//! Claude Desktop の設定がこの実行ファイル1つを指せるようにするための経路
//! (配布形の前提 — 非エンジニアに別バイナリの導入を求めない)。

use kb_core::client_binding::ClientBinding;
use kb_core::client_surface::ClientSurface;

/// MCP モードなら実行して true を返す。通常起動なら false。
pub fn run_if_requested() -> bool {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|a| a == "--mcp") {
        return false;
    }

    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let client = flag("--client").unwrap_or_else(|| "mcp-client/unknown".into());
    let remote_sync = !args.iter().any(|arg| arg == "--no-remote-sync");
    let require_client_binding = args.iter().any(|arg| arg == "--require-client-binding");
    let tool_surface = flag("--mcp-surface")
        .as_deref()
        .map(kb_core::mcp::ToolSurface::parse)
        .transpose()
        .unwrap_or_else(|error| {
            eprintln!("kb-app --mcp: {error}");
            std::process::exit(2);
        })
        .unwrap_or_default();
    // 配信 profile も surface と同じく起動時に固定し、未知値は既定へ落とさず終了する。
    let retrieval_profile = flag("--retrieval-profile")
        .as_deref()
        .map(kb_core::retrieval_profile::RetrievalProfile::parse)
        .transpose()
        .unwrap_or_else(|error| {
            eprintln!("kb-app --mcp: {error}");
            std::process::exit(2);
        });

    let result = kb_core::mcp::serve_with_options(
        &client,
        kb_core::mcp::ServeOptions {
            remote_sync,
            tool_surface,
            retrieval_profile,
            require_client_binding,
            hook_context: args.iter().any(|arg| arg == "--hook-context"),
        },
        || {
            // coreがONとbindingを確認してから呼ぶ。hook親側では設定やVaultを読まない。
            let vault_name = lazy_vault_name(flag("--vault"), require_client_binding, || {
                kb_core::client_binding::load(ClientSurface::from_hint(&client))
            })?;
            let reg = kb_core::registry::Registry::load()?;
            let path = reg.resolve(vault_name.as_deref())?;
            kb_core::vault::Vault::open(path)
        },
    );

    if let Err(e) = result {
        eprintln!("kb-app --mcp: {e}");
        std::process::exit(1);
    }
    true
}

fn lazy_vault_name(
    requested: Option<String>,
    require_client_binding: bool,
    load_binding: impl FnOnce() -> kb_core::error::Result<Option<ClientBinding>>,
) -> anyhow::Result<Option<String>> {
    if !require_client_binding {
        return Ok(requested);
    }
    // coreの検査後に登録が消えても、現在の既定Vaultへフォールバックさせない。
    let binding = load_binding()?.ok_or_else(|| anyhow::anyhow!("workspace_unverified"))?;
    Ok(Some(binding.vault_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_mcp_keeps_the_requested_vault_without_reading_hook_binding() {
        for requested in [None, Some("explicit".into())] {
            assert_eq!(
                lazy_vault_name(requested.clone(), false, || panic!("bindingを読まない")).unwrap(),
                requested
            );
        }
    }

    /// 2026-09-05: hookは接続済みKBを選び、既定変更や古い--vaultから別KBへ流れない。
    #[test]
    fn hook_uses_the_bound_vault_and_never_falls_back_when_binding_is_missing_or_broken() {
        for requested in [None, Some("stale-vault".into())] {
            let bound = lazy_vault_name(requested.clone(), true, || {
                Ok(Some(ClientBinding {
                    vault_name: "trusted-vault".into(),
                    workspace_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
                }))
            })
            .unwrap();
            assert_eq!(bound.as_deref(), Some("trusted-vault"));
            assert!(lazy_vault_name(requested.clone(), true, || Ok(None)).is_err());
            assert!(
                lazy_vault_name(requested, true, || {
                    Err(kb_core::error::CoreError::configuration(anyhow::anyhow!(
                        "fixture binding invalid"
                    )))
                })
                .is_err()
            );
        }
    }
}
