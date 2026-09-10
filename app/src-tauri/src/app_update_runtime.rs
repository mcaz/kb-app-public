//! 更新取得のOSアダプタ。互換性とarchiveの判断はkb-coreへ委譲する。
//! 公式pluginのinstallは呼ばず、起動受入まで旧版を残す経路にだけ渡す。

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use kb_core::app_update_package::{ValidatedPackage, validate_macos_package};
use kb_core::app_update_transaction::BundleIdentity;
use serde::{Deserialize, Serialize};
use tauri::Manager;
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::error::{AppError, AppResult};

const DOWNLOAD_LIMIT: u64 = 128 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum UpdatePhase {
    Unavailable,
    Idle,
    Checking,
    UpToDate,
    Available,
    Downloading,
    Verifying,
    Ready,
    Installing,
}

impl UpdatePhase {
    fn busy(self) -> bool {
        matches!(
            self,
            Self::Checking | Self::Downloading | Self::Verifying | Self::Installing
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum UpdateFailureKind {
    NotConfigured,
    UnsupportedPlatform,
    Busy,
    Network,
    Signature,
    InvalidPackage,
    Incompatible,
    Storage,
    InstallFailed,
    RestartFailed,
}

#[derive(Debug, Clone, Serialize, specta::Type)]
pub struct UpdateStatus {
    pub current_version: String,
    pub phase: UpdatePhase,
    pub available_version: Option<String>,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub failure: Option<UpdateFailureKind>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    format_version: u32,
    endpoint: Option<String>,
    public_key: Option<String>,
}

fn parse_configuration(json: &str) -> Option<Configuration> {
    let value: Configuration = serde_json::from_str(json).ok()?;
    if value.format_version != 1 {
        return None;
    }
    let endpoint = value.endpoint.as_ref()?;
    if endpoint.len() >= 4096 {
        return None;
    }
    let url = tauri::Url::parse(endpoint).ok()?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !value
            .public_key
            .as_ref()
            .is_some_and(|s| !s.is_empty() && s.len() < 4096)
    {
        return None;
    }
    Some(value)
}

fn configuration() -> Option<Configuration> {
    parse_configuration(include_str!("../updater-config.json"))
}

pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry, tauri_plugin_updater::Config> {
    tauri_plugin_updater::Builder::new()
        .pubkey(
            configuration()
                .and_then(|c| c.public_key)
                .unwrap_or_default(),
        )
        .build()
}

struct Session {
    status: UpdateStatus,
    update: Option<Update>,
    package: Option<Arc<ValidatedPackage>>,
    source: Option<BundleIdentity>,
}

#[derive(Clone)]
pub struct UpdateState(Arc<Mutex<Session>>);

impl Default for UpdateState {
    fn default() -> Self {
        let failure = if !cfg!(target_os = "macos") {
            Some(UpdateFailureKind::UnsupportedPlatform)
        } else if configuration().is_none() {
            Some(UpdateFailureKind::NotConfigured)
        } else {
            None
        };
        let phase = if failure.is_some() {
            UpdatePhase::Unavailable
        } else {
            UpdatePhase::Idle
        };
        let failure =
            if failure.is_none() && crate::app_update_supervisor::previous_boot_was_restored() {
                Some(UpdateFailureKind::RestartFailed)
            } else {
                failure
            };
        Self(Arc::new(Mutex::new(Session {
            status: UpdateStatus {
                current_version: kb_core::CORE_VERSION.into(),
                phase,
                available_version: None,
                downloaded_bytes: 0,
                total_bytes: None,
                failure,
            },
            update: None,
            package: None,
            source: None,
        })))
    }
}

impl UpdateState {
    fn lock(&self) -> AppResult<MutexGuard<'_, Session>> {
        self.0.lock().map_err(AppError::unexpected)
    }

    pub fn status(&self) -> AppResult<UpdateStatus> {
        Ok(self.lock()?.status.clone())
    }

    fn fail(&self, phase: UpdatePhase, failure: UpdateFailureKind) -> AppResult<UpdateStatus> {
        let mut session = self.lock()?;
        session.status.phase = phase;
        session.status.failure = Some(failure);
        Ok(session.status.clone())
    }

    pub async fn check(&self, app: tauri::AppHandle) -> AppResult<UpdateStatus> {
        {
            let mut session = self.lock()?;
            if session.status.phase == UpdatePhase::Unavailable || session.status.phase.busy() {
                return Ok(session.status.clone());
            }
            session.update = None;
            session.package = None;
            session.source = None;
            session.status.available_version = None;
            session.status.downloaded_bytes = 0;
            session.status.total_bytes = None;
            session.status.failure = None;
            session.status.phase = UpdatePhase::Checking;
        }
        let Some(config) = configuration() else {
            return self.fail(UpdatePhase::Unavailable, UpdateFailureKind::NotConfigured);
        };
        let result = async {
            let endpoint = config.endpoint.expect("configurationで検査済み").parse()?;
            app.updater_builder()
                .endpoints(vec![endpoint])?
                .pubkey(config.public_key.expect("configurationで検査済み"))
                .timeout(REQUEST_TIMEOUT)
                // pluginはfeed内URLやredirect先を制限しないため、両方のclientへ強制する。
                .configure_client(|client| client.https_only(true).timeout(REQUEST_TIMEOUT))
                .build()?
                .check()
                .await
        }
        .await;
        match result {
            Ok(update) => {
                let mut session = self.lock()?;
                session.status.available_version = update.as_ref().map(|u| u.version.clone());
                session.status.phase = if update.is_some() {
                    UpdatePhase::Available
                } else {
                    UpdatePhase::UpToDate
                };
                session.update = update;
                Ok(session.status.clone())
            }
            Err(error) => {
                eprintln!("kb-app updater check: {error}");
                self.fail(UpdatePhase::Idle, UpdateFailureKind::Network)
            }
        }
    }

    pub async fn download(&self) -> AppResult<UpdateStatus> {
        let mut update = {
            let mut session = self.lock()?;
            if session.status.phase != UpdatePhase::Available {
                return Ok(session.status.clone());
            }
            let Some(update) = session.update.clone() else {
                return Ok(session.status.clone());
            };
            session.package = None;
            session.source = None;
            session.status.phase = UpdatePhase::Downloading;
            session.status.failure = None;
            session.status.downloaded_bytes = 0;
            session.status.total_bytes = None;
            update
        };
        // 2.11.0のcheck()はUpdate.timeoutをNoneにする。取得側にも期限を明記する。
        update.timeout = Some(REQUEST_TIMEOUT);
        let (limit_tx, mut limit_rx) = tokio::sync::watch::channel(false);
        let progress = self.clone();
        let finish = self.clone();
        let download = update.download(
            move |chunk, total| {
                if let Ok(mut session) = progress.0.lock() {
                    session.status.downloaded_bytes =
                        session.status.downloaded_bytes.saturating_add(chunk as u64);
                    session.status.total_bytes = total.filter(|n| *n <= DOWNLOAD_LIMIT);
                    if session.status.downloaded_bytes > DOWNLOAD_LIMIT
                        || total.is_some_and(|n| n > DOWNLOAD_LIMIT)
                    {
                        let _ = limit_tx.send(true);
                    }
                }
            },
            move || {
                // このcallbackは署名検証前。readyへ進めてはいけない。
                if let Ok(mut session) = finish.0.lock() {
                    session.status.phase = UpdatePhase::Verifying;
                }
            },
        );
        let downloaded =
            match futures_util::future::select(Box::pin(limit_rx.changed()), Box::pin(download))
                .await
            {
                futures_util::future::Either::Left(_) => {
                    return self.fail(UpdatePhase::Available, UpdateFailureKind::InvalidPackage);
                }
                futures_util::future::Either::Right((result, _)) => result,
            };
        let bytes = match downloaded {
            Ok(bytes) if bytes.len() as u64 <= DOWNLOAD_LIMIT => bytes,
            Ok(_) => return self.fail(UpdatePhase::Available, UpdateFailureKind::InvalidPackage),
            Err(error) => {
                let kind = match &error {
                    tauri_plugin_updater::Error::Minisign(_)
                    | tauri_plugin_updater::Error::Base64(_)
                    | tauri_plugin_updater::Error::SignatureUtf8(_) => UpdateFailureKind::Signature,
                    _ => UpdateFailureKind::Network,
                };
                eprintln!("kb-app updater download: {error}");
                return self.fail(UpdatePhase::Available, kind);
            }
        };
        let version = update.version.clone();
        let validation = tauri::async_runtime::spawn_blocking(move || {
            let target = crate::app_update_supervisor::target_triple()
                .ok_or(UpdateFailureKind::UnsupportedPlatform)?;
            let package = validate_macos_package(bytes, kb_core::CORE_VERSION, &version, target)
                .map_err(|error| {
                    eprintln!("kb-app update archive: {error}");
                    UpdateFailureKind::InvalidPackage
                })?;
            let source = crate::app_update_supervisor::current_identity()?;
            if !package.supports_source(&source.plan_sha256, &source.executable_sha256)
                || !kb_core::app_update_compatibility::inspect(package.compatibility())
                    .checks_passed
            {
                return Err(UpdateFailureKind::Incompatible);
            }
            Ok((package, source))
        })
        .await
        .map_err(AppError::unexpected)?;
        match validation {
            Ok((package, source)) => {
                let mut session = self.lock()?;
                session.package = Some(Arc::new(package));
                session.source = Some(source);
                session.status.phase = UpdatePhase::Ready;
                Ok(session.status.clone())
            }
            Err(kind) => self.fail(UpdatePhase::Available, kind),
        }
    }

    pub async fn install(&self, app: tauri::AppHandle) -> AppResult<UpdateStatus> {
        let (package, source) = {
            let mut session = self.lock()?;
            if session.status.phase != UpdatePhase::Ready {
                return Ok(session.status.clone());
            }
            let (Some(package), Some(source)) = (&session.package, &session.source) else {
                return Ok(session.status.clone());
            };
            let values = (package.clone(), source.clone());
            session.status.phase = UpdatePhase::Installing;
            session.status.failure = None;
            values
        };
        let prepared = tauri::async_runtime::spawn_blocking(move || {
            crate::app_update_supervisor::prepare(package, &source)
        })
        .await
        .map_err(AppError::unexpected)?;
        match prepared {
            Ok(()) => {
                // helperの準備完了後だけGUIを終了する。旧MCPを終了する操作は持たない。
                let result = self.status()?;
                app.state::<crate::distillation_worker::WorkerControl>()
                    .shutdown();
                app.exit(0);
                Ok(result)
            }
            Err(kind) => self.fail(
                if kind == UpdateFailureKind::Incompatible {
                    UpdatePhase::Available
                } else {
                    UpdatePhase::Ready
                },
                kind,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tauri_can_initialize_the_unconfigured_plugin_without_install_permissions() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let _: tauri_plugin_updater::Config =
            serde_json::from_value(config["plugins"]["updater"].clone()).unwrap();
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/default.json")).unwrap();
        assert!(
            capability["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|p| !p.as_str().is_some_and(|p| p.starts_with("updater:")))
        );
    }

    #[test]
    fn only_complete_https_configuration_can_enable_network_requests() {
        for endpoint in [
            "http://example.com/update",
            "https://user:secret@example.com/update",
            "https://example.com/update#fragment",
            "file:///tmp/update",
        ] {
            assert!(
                parse_configuration(
                    &serde_json::json!({"format_version":1,"endpoint":endpoint,"public_key":"key"})
                        .to_string()
                )
                .is_none()
            );
        }
        for json in [
            r#"{"format_version":1,"endpoint":null,"public_key":null}"#,
            r#"{"format_version":1,"endpoint":"https://example.com/update","public_key":null}"#,
        ] {
            assert!(parse_configuration(json).is_none());
        }
        assert!(
            parse_configuration(
                r#"{"format_version":1,"endpoint":"https://example.com/update","public_key":"key"}"#
            )
            .is_some()
        );
    }

    #[test]
    fn unconfigured_build_never_claims_up_to_date_or_ready() {
        assert!(
            configuration().is_none(),
            "配信を設定したらこのfixtureを設定専用へ変更する"
        );
        let state = UpdateState::default();
        let status = state.status().unwrap();
        assert_eq!(status.phase, UpdatePhase::Unavailable);
        assert!(status.available_version.is_none());
        assert!(matches!(
            status.failure,
            Some(UpdateFailureKind::NotConfigured | UpdateFailureKind::UnsupportedPlatform)
        ));
    }
}
