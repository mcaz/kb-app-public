//! GitHub OAuth device flow と、取得した token の安全な保管。
//!
//! OAuth の `device_code` と access token は Rust 側から出さない。画面へ渡すのは
//! ユーザーが入力する短い `user_code` だけで、token bundle は OS keychain に JSON として
//! 保存する。Git / Git LFS には子 process の環境変数経由で一時 header を渡し、remote URL や
//! `.git/config` へ token を書かない。

use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::backup::{BackupFailureKind, failure};

const GITHUB_OAUTH_BASE: &str = "https://github.com";
const GITHUB_API_BASE: &str = "https://api.github.com";
const VERIFICATION_URI: &str = "https://github.com/login/device";
const KEYRING_SERVICE: &str = "app.kb.desktop.github";
const KEYRING_ACCOUNT: &str = "oauth-token";
const REQUIRED_SCOPE: &str = "repo";
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const REFRESH_LEEWAY_SECS: u64 = 60;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct GitHubAuthState {
    /// OAuth App の client ID が build または実行環境に設定済みか。
    pub configured: bool,
    /// OS keychain に利用可能な credential があるか。
    pub signed_in: bool,
    pub account_login: Option<String>,
}

/// Device flow のうち画面へ出してよい情報。`device_code` は意図的に含めない。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct DeviceAuthorization {
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    token_type: Option<String>,
    scope: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    refresh_token_expires_in: Option<u64>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct UserResponse {
    id: u64,
    login: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct TokenBundle {
    access_token: String,
    refresh_token: Option<String>,
    expires_at_epoch: Option<u64>,
    refresh_token_expires_at_epoch: Option<u64>,
    token_type: String,
    scopes: String,
    account_login: String,
    account_id: u64,
}

trait CredentialStore {
    fn load(&self) -> Result<Option<TokenBundle>>;
    fn save(&self, bundle: &TokenBundle) -> Result<()>;
    fn delete(&self) -> Result<()>;
}

trait DeviceFlowClient {
    fn request_device_code(&self, client_id: &str) -> Result<DeviceCodeResponse>;
    fn poll_token(&self, client_id: &str, device_code: &str) -> Result<PollOutcome>;
    fn validate_user(&self, token: &str) -> Result<UserResponse>;
}

struct GitHubHttpClient<'a> {
    oauth_base: &'a str,
    api_base: &'a str,
}

impl DeviceFlowClient for GitHubHttpClient<'_> {
    fn request_device_code(&self, client_id: &str) -> Result<DeviceCodeResponse> {
        request_device_code_at(self.oauth_base, client_id)
    }

    fn poll_token(&self, client_id: &str, device_code: &str) -> Result<PollOutcome> {
        poll_token_at(self.oauth_base, client_id, device_code)
    }

    fn validate_user(&self, token: &str) -> Result<UserResponse> {
        validate_user_at(self.api_base, token)
    }
}

struct OsCredentialStore;

impl OsCredentialStore {
    fn entry() -> Result<keyring::Entry> {
        keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT).map_err(|error| {
            failure(
                BackupFailureKind::Authentication,
                format!("OS キーチェーンを利用できない: {error}"),
            )
        })
    }
}

impl CredentialStore for OsCredentialStore {
    fn load(&self) -> Result<Option<TokenBundle>> {
        let raw = match Self::entry()?.get_password() {
            Ok(raw) => raw,
            Err(keyring::Error::NoEntry) => return Ok(None),
            Err(error) => {
                return Err(failure(
                    BackupFailureKind::Authentication,
                    format!("OS キーチェーンから GitHub 認証を読めない: {error}"),
                ));
            }
        };
        serde_json::from_str(&raw).map(Some).map_err(|_| {
            failure(
                BackupFailureKind::Authentication,
                "OS キーチェーンの GitHub 認証情報が壊れているためサインインし直す",
            )
        })
    }

    fn save(&self, bundle: &TokenBundle) -> Result<()> {
        let raw = serde_json::to_string(bundle).map_err(|error| {
            failure(
                BackupFailureKind::Authentication,
                format!("GitHub 認証情報を保存用に変換できない: {error}"),
            )
        })?;
        Self::entry()?.set_password(&raw).map_err(|error| {
            failure(
                BackupFailureKind::Authentication,
                format!("GitHub 認証情報を OS キーチェーンへ保存できない: {error}"),
            )
        })
    }

    fn delete(&self) -> Result<()> {
        match Self::entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(failure(
                BackupFailureKind::Authentication,
                format!("OS キーチェーンの GitHub 認証情報を削除できない: {error}"),
            )),
        }
    }
}

/// OAuth App の client ID。client secret は device flow では使わず、埋め込まない。
/// release build は `KB_GITHUB_CLIENT_ID` を build 環境へ渡す。実行時の同名環境変数は
/// 開発・受入用 override として先に見る。
fn client_id() -> Option<String> {
    std::env::var("KB_GITHUB_CLIENT_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            option_env!("KB_GITHUB_CLIENT_ID")
                .map(str::to_string)
                .filter(|value| !value.trim().is_empty())
        })
}

pub fn auth_state() -> Result<GitHubAuthState> {
    auth_state_with(&OsCredentialStore, client_id().is_some())
}

fn auth_state_with(store: &impl CredentialStore, configured: bool) -> Result<GitHubAuthState> {
    let bundle = store.load()?;
    let usable = bundle.as_ref().is_some_and(|bundle| {
        let now = epoch_now().saturating_add(REFRESH_LEEWAY_SECS);
        let access_valid = bundle.expires_at_epoch.is_none_or(|expires| expires > now);
        let refresh_valid = configured
            && bundle.refresh_token.is_some()
            && bundle
                .refresh_token_expires_at_epoch
                .is_none_or(|expires| expires > now);
        access_valid || refresh_valid
    });
    if bundle.is_some() && !usable {
        store.delete()?;
    }
    Ok(GitHubAuthState {
        configured,
        signed_in: usable,
        account_login: usable.then(|| bundle.expect("usable bundle").account_login),
    })
}

/// ローカル credential を消す。OAuth App の client secret を端末へ持たせないため、
/// GitHub 上の token 失効は GitHub の Applications 設定から行う。
pub fn sign_out() -> Result<()> {
    OsCredentialStore.delete()
}

/// 401 を受けた credential は再利用しない。
pub(crate) fn invalidate() {
    let _ = OsCredentialStore.delete();
}

pub fn sign_in(notify: impl FnMut(DeviceAuthorization)) -> Result<GitHubAuthState> {
    let client_id = client_id().ok_or_else(|| {
        failure(
            BackupFailureKind::Authentication,
            "GitHub OAuth App の client ID が未設定のためサインインを開始できない",
        )
    })?;
    let http = GitHubHttpClient {
        oauth_base: GITHUB_OAUTH_BASE,
        api_base: GITHUB_API_BASE,
    };
    sign_in_with(
        &OsCredentialStore,
        &http,
        &client_id,
        std::thread::sleep,
        notify,
    )
}

fn sign_in_with(
    store: &impl CredentialStore,
    client: &impl DeviceFlowClient,
    client_id: &str,
    mut sleep: impl FnMut(Duration),
    mut notify: impl FnMut(DeviceAuthorization),
) -> Result<GitHubAuthState> {
    let device = client.request_device_code(client_id)?;
    notify(DeviceAuthorization {
        user_code: device.user_code.clone(),
        verification_uri: device.verification_uri.clone(),
        expires_in: device.expires_in,
    });
    let deadline = Instant::now() + Duration::from_secs(device.expires_in);
    let mut interval = Duration::from_secs(device.interval.unwrap_or(5).max(1));
    loop {
        if Instant::now() >= deadline {
            return Err(failure(
                BackupFailureKind::Authentication,
                "GitHub の認証コードが期限切れになったため、もう一度サインインする",
            ));
        }
        sleep(interval);
        if Instant::now() >= deadline {
            return Err(failure(
                BackupFailureKind::Authentication,
                "GitHub の認証コードが期限切れになったため、もう一度サインインする",
            ));
        }
        match client.poll_token(client_id, &device.device_code)? {
            PollOutcome::Pending => {}
            PollOutcome::SlowDown => interval += Duration::from_secs(5),
            PollOutcome::Authorized(token) => {
                let bundle = authorize_token_with(token, |token| client.validate_user(token))?;
                store.save(&bundle)?;
                return auth_state_with(store, true);
            }
            PollOutcome::Denied => {
                return Err(failure(
                    BackupFailureKind::Authentication,
                    "GitHub へのサインインがキャンセルされた",
                ));
            }
            PollOutcome::Expired => {
                return Err(failure(
                    BackupFailureKind::Authentication,
                    "GitHub の認証コードが期限切れになったため、もう一度サインインする",
                ));
            }
        }
    }
}

fn request_device_code_at(oauth_base: &str, client_id: &str) -> Result<DeviceCodeResponse> {
    let endpoint = format!("{}/login/device/code", oauth_base.trim_end_matches('/'));
    let mut response = ureq::post(&endpoint)
        .config()
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .header("Accept", "application/json")
        .header("User-Agent", "kb-app")
        .send_form([("client_id", client_id), ("scope", REQUIRED_SCOPE)])
        .map_err(|error| {
            failure(
                BackupFailureKind::Network,
                format!("GitHub のサインインを開始できない: {error}"),
            )
        })?;
    let body = response.body_mut().read_to_string().map_err(|error| {
        failure(
            BackupFailureKind::Network,
            format!("GitHub の認証コード応答を読めない: {error}"),
        )
    })?;
    let device: DeviceCodeResponse = serde_json::from_str(&body).map_err(|error| {
        failure(
            BackupFailureKind::Authentication,
            format!("GitHub の認証コード応答が不正: {error}"),
        )
    })?;
    if device.device_code.is_empty()
        || device.user_code.is_empty()
        || device.verification_uri != VERIFICATION_URI
        || device.expires_in == 0
    {
        return Err(failure(
            BackupFailureKind::Authentication,
            "GitHub の認証コード応答を安全に確認できない",
        ));
    }
    Ok(device)
}

enum PollOutcome {
    Pending,
    SlowDown,
    Authorized(TokenResponse),
    Denied,
    Expired,
}

fn poll_token_at(oauth_base: &str, client_id: &str, device_code: &str) -> Result<PollOutcome> {
    let endpoint = format!(
        "{}/login/oauth/access_token",
        oauth_base.trim_end_matches('/')
    );
    let mut response = ureq::post(&endpoint)
        .config()
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .header("Accept", "application/json")
        .header("User-Agent", "kb-app")
        .send_form([
            ("client_id", client_id),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ])
        .map_err(|error| {
            failure(
                BackupFailureKind::Network,
                format!("GitHub のサインイン完了を確認できない: {error}"),
            )
        })?;
    let body = response.body_mut().read_to_string().map_err(|error| {
        failure(
            BackupFailureKind::Network,
            format!("GitHub の token 応答を読めない: {error}"),
        )
    })?;
    let token: TokenResponse = serde_json::from_str(&body).map_err(|error| {
        failure(
            BackupFailureKind::Authentication,
            format!("GitHub の token 応答が不正: {error}"),
        )
    })?;
    if token
        .access_token
        .as_deref()
        .is_some_and(|value| !value.is_empty())
    {
        return Ok(PollOutcome::Authorized(token));
    }
    match token.error.as_deref() {
        Some("authorization_pending") => Ok(PollOutcome::Pending),
        Some("slow_down") => Ok(PollOutcome::SlowDown),
        Some("access_denied") => Ok(PollOutcome::Denied),
        Some("expired_token") => Ok(PollOutcome::Expired),
        Some(error) => Err(failure(
            BackupFailureKind::Authentication,
            format!(
                "GitHub のサインインを完了できない({error}): {}",
                token.error_description.as_deref().unwrap_or("詳細なし")
            ),
        )),
        None => Err(failure(
            BackupFailureKind::Authentication,
            "GitHub の token 応答に token も失敗理由もない",
        )),
    }
}

fn authorize_token_with(
    token: TokenResponse,
    validate_user: impl FnOnce(&str) -> Result<UserResponse>,
) -> Result<TokenBundle> {
    let access_token = token.access_token.ok_or_else(|| {
        failure(
            BackupFailureKind::Authentication,
            "GitHub の token 応答に access token がない",
        )
    })?;
    let scopes = token.scope.unwrap_or_default();
    if !has_scope(&scopes, REQUIRED_SCOPE) {
        return Err(failure(
            BackupFailureKind::Permission,
            "private repository の作成・読み書きに必要な repo 権限が許可されていない",
        ));
    }
    let user = validate_user(&access_token)?;
    let now = epoch_now();
    Ok(TokenBundle {
        access_token,
        refresh_token: token.refresh_token,
        expires_at_epoch: token.expires_in.map(|seconds| now.saturating_add(seconds)),
        refresh_token_expires_at_epoch: token
            .refresh_token_expires_in
            .map(|seconds| now.saturating_add(seconds)),
        token_type: token.token_type.unwrap_or_else(|| "bearer".to_string()),
        scopes,
        account_login: user.login,
        account_id: user.id,
    })
}

fn has_scope(scopes: &str, required: &str) -> bool {
    scopes
        .split([',', ' '])
        .map(str::trim)
        .any(|scope| scope == required)
}

fn validate_user_at(api_base: &str, token: &str) -> Result<UserResponse> {
    let endpoint = format!("{}/user", api_base.trim_end_matches('/'));
    let mut response = ureq::get(&endpoint)
        .config()
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .header("Accept", "application/vnd.github+json")
        .header("Authorization", &format!("Bearer {token}"))
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", "kb-app")
        .call()
        .map_err(|error| match error {
            ureq::Error::StatusCode(401) => failure(
                BackupFailureKind::Authentication,
                "GitHub が発行した認証情報を確認できない",
            ),
            ureq::Error::StatusCode(403) => failure(
                BackupFailureKind::Permission,
                "GitHub account を確認する権限がない",
            ),
            other => failure(
                BackupFailureKind::Network,
                format!("GitHub account の確認に失敗: {other}"),
            ),
        })?;
    let body = response.body_mut().read_to_string().map_err(|error| {
        failure(
            BackupFailureKind::Authentication,
            format!("GitHub account 応答を読めない: {error}"),
        )
    })?;
    let user: UserResponse = serde_json::from_str(&body).map_err(|error| {
        failure(
            BackupFailureKind::Authentication,
            format!("GitHub account 応答が不正: {error}"),
        )
    })?;
    if user.id == 0 || user.login.trim().is_empty() {
        return Err(failure(
            BackupFailureKind::Authentication,
            "GitHub account の識別情報を確認できない",
        ));
    }
    Ok(user)
}

fn epoch_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub(crate) fn access_token() -> Result<String> {
    access_token_with(&OsCredentialStore, GITHUB_OAUTH_BASE, GITHUB_API_BASE)
}

fn access_token_with(
    store: &impl CredentialStore,
    oauth_base: &str,
    api_base: &str,
) -> Result<String> {
    let Some(mut bundle) = store.load()? else {
        return Err(failure(
            BackupFailureKind::Authentication,
            "GitHub にサインインしていない",
        ));
    };
    let expired = bundle
        .expires_at_epoch
        .is_some_and(|expires| expires <= epoch_now().saturating_add(REFRESH_LEEWAY_SECS));
    if !expired {
        return Ok(bundle.access_token);
    }
    let client_id = client_id().ok_or_else(|| {
        failure(
            BackupFailureKind::Authentication,
            "GitHub の認証更新に必要な OAuth App client ID が未設定",
        )
    })?;
    let refresh_token = bundle.refresh_token.as_deref().ok_or_else(|| {
        let _ = store.delete();
        failure(
            BackupFailureKind::Authentication,
            "GitHub の認証が期限切れのためサインインし直す",
        )
    })?;
    if bundle
        .refresh_token_expires_at_epoch
        .is_some_and(|expires| expires <= epoch_now().saturating_add(REFRESH_LEEWAY_SECS))
    {
        let _ = store.delete();
        return Err(failure(
            BackupFailureKind::Authentication,
            "GitHub の認証更新期限が切れたためサインインし直す",
        ));
    }
    let refreshed = match refresh_token_at(oauth_base, &client_id, refresh_token) {
        Ok(refreshed) => refreshed,
        Err(error)
            if crate::backup::failure_kind(&error) == Some(BackupFailureKind::Authentication) =>
        {
            let _ = store.delete();
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    let access_token = refreshed.access_token.clone().ok_or_else(|| {
        failure(
            BackupFailureKind::Authentication,
            "GitHub の認証更新応答に access token がない",
        )
    })?;
    let scopes = refreshed
        .scope
        .clone()
        .unwrap_or_else(|| bundle.scopes.clone());
    if !has_scope(&scopes, REQUIRED_SCOPE) {
        let _ = store.delete();
        return Err(failure(
            BackupFailureKind::Permission,
            "更新後の GitHub 認証に repo 権限がないためサインインし直す",
        ));
    }
    let user = match validate_user_at(api_base, &access_token) {
        Ok(user) => user,
        Err(error)
            if crate::backup::failure_kind(&error) == Some(BackupFailureKind::Authentication) =>
        {
            let _ = store.delete();
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    if user.id != bundle.account_id {
        let _ = store.delete();
        return Err(failure(
            BackupFailureKind::Authentication,
            "認証更新後の GitHub account が変わったためサインインし直す",
        ));
    }
    let now = epoch_now();
    bundle.access_token = access_token;
    bundle.refresh_token = refreshed.refresh_token.or(bundle.refresh_token);
    bundle.expires_at_epoch = refreshed
        .expires_in
        .map(|seconds| now.saturating_add(seconds));
    bundle.refresh_token_expires_at_epoch = refreshed
        .refresh_token_expires_in
        .map(|seconds| now.saturating_add(seconds))
        .or(bundle.refresh_token_expires_at_epoch);
    bundle.token_type = refreshed.token_type.unwrap_or(bundle.token_type);
    bundle.scopes = scopes;
    bundle.account_login = user.login;
    store.save(&bundle)?;
    Ok(bundle.access_token)
}

fn refresh_token_at(
    oauth_base: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<TokenResponse> {
    let endpoint = format!(
        "{}/login/oauth/access_token",
        oauth_base.trim_end_matches('/')
    );
    let mut response = ureq::post(&endpoint)
        .config()
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .header("Accept", "application/json")
        .header("User-Agent", "kb-app")
        .send_form([
            ("client_id", client_id),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .map_err(|error| match error {
            ureq::Error::StatusCode(400 | 401) => failure(
                BackupFailureKind::Authentication,
                "GitHub が認証更新を拒否したためサインインし直す",
            ),
            other => failure(
                BackupFailureKind::Network,
                format!("GitHub の認証を更新できない: {other}"),
            ),
        })?;
    let body = response.body_mut().read_to_string().map_err(|error| {
        failure(
            BackupFailureKind::Network,
            format!("GitHub の認証更新応答を読めない: {error}"),
        )
    })?;
    let token: TokenResponse = serde_json::from_str(&body).map_err(|error| {
        failure(
            BackupFailureKind::Authentication,
            format!("GitHub の認証更新応答が不正: {error}"),
        )
    })?;
    if let Some(error) = &token.error {
        return Err(failure(
            BackupFailureKind::Authentication,
            format!("GitHub の認証更新に失敗({error})。サインインし直す"),
        ));
    }
    Ok(token)
}

/// GitHub HTTPS transport 用の認証を子 process にだけ渡す。
pub fn configure_git_auth(command: &mut Command, repository_url: &str) -> Result<()> {
    if crate::github::parse_repository_url(repository_url).is_err() {
        return Ok(());
    }
    let token = access_token()?;
    configure_git_token(command, &token);
    Ok(())
}

fn configure_git_token(command: &mut Command, token: &str) {
    let raw = format!("x-access-token:{token}");
    let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
    command
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "http.https://github.com/.extraheader")
        .env(
            "GIT_CONFIG_VALUE_0",
            format!("Authorization: Basic {encoded}"),
        );
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct MemoryStore(Mutex<Option<TokenBundle>>);

    impl CredentialStore for MemoryStore {
        fn load(&self) -> Result<Option<TokenBundle>> {
            Ok(self.0.lock().unwrap().clone())
        }

        fn save(&self, bundle: &TokenBundle) -> Result<()> {
            *self.0.lock().unwrap() = Some(bundle.clone());
            Ok(())
        }

        fn delete(&self) -> Result<()> {
            *self.0.lock().unwrap() = None;
            Ok(())
        }
    }

    struct MockClient {
        device: DeviceCodeResponse,
        polls: Mutex<VecDeque<PollOutcome>>,
        user: UserResponse,
    }

    impl DeviceFlowClient for MockClient {
        fn request_device_code(&self, _client_id: &str) -> Result<DeviceCodeResponse> {
            Ok(self.device.clone())
        }

        fn poll_token(&self, _client_id: &str, _device_code: &str) -> Result<PollOutcome> {
            Ok(self.polls.lock().unwrap().pop_front().unwrap())
        }

        fn validate_user(&self, _token: &str) -> Result<UserResponse> {
            Ok(self.user.clone())
        }
    }

    fn mock_client(scopes: &str) -> MockClient {
        MockClient {
            device: DeviceCodeResponse {
                device_code: "secret-device-code".into(),
                user_code: "ABCD-EFGH".into(),
                verification_uri: VERIFICATION_URI.into(),
                expires_in: 900,
                interval: Some(1),
            },
            polls: Mutex::new(VecDeque::from([PollOutcome::Authorized(TokenResponse {
                access_token: Some("secret-access-token".into()),
                token_type: Some("bearer".into()),
                scope: Some(scopes.into()),
                refresh_token: None,
                expires_in: None,
                refresh_token_expires_in: None,
                error: None,
                error_description: None,
            })])),
            user: UserResponse {
                id: 42,
                login: "octocat".into(),
            },
        }
    }

    #[test]
    fn mock_device_flow_validates_account_and_saves_without_exposing_device_code() {
        let store = MemoryStore::default();
        let client = mock_client("repo");
        let mut shown = None;
        let state = sign_in_with(
            &store,
            &client,
            "test-client-id",
            |_| {},
            |authorization| shown = Some(authorization),
        )
        .unwrap();

        assert_eq!(shown.unwrap().user_code, "ABCD-EFGH");
        assert_eq!(state.account_login.as_deref(), Some("octocat"));
        assert!(state.signed_in);
        let saved = store.load().unwrap().unwrap();
        assert_eq!(saved.access_token, "secret-access-token");
        assert_eq!(saved.account_id, 42);
    }

    #[test]
    fn token_without_repo_scope_is_rejected_and_never_saved() {
        let store = MemoryStore::default();
        let client = mock_client("read:user");
        let result = sign_in_with(&store, &client, "test-client-id", |_| {}, |_| {});

        assert!(result.is_err());
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn auth_state_does_not_need_the_os_keychain_in_tests() {
        let store = MemoryStore::default();
        assert_eq!(
            auth_state_with(&store, false).unwrap(),
            GitHubAuthState {
                configured: false,
                signed_in: false,
                account_login: None,
            }
        );
    }

    #[test]
    fn expired_unrefreshable_credential_is_removed_from_the_mock_keychain() {
        let store = MemoryStore::default();
        store
            .save(&TokenBundle {
                access_token: "expired".into(),
                refresh_token: None,
                expires_at_epoch: Some(1),
                refresh_token_expires_at_epoch: None,
                token_type: "bearer".into(),
                scopes: "repo".into(),
                account_login: "octocat".into(),
                account_id: 42,
            })
            .unwrap();

        assert!(!auth_state_with(&store, true).unwrap().signed_in);
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn git_token_is_a_process_only_header_not_a_command_argument() {
        let mut command = Command::new("git");
        configure_git_token(&mut command, "secret-token");
        let env: std::collections::BTreeMap<_, _> = command
            .get_envs()
            .filter_map(|(key, value)| Some((key.to_str()?, value?.to_str()?)))
            .collect();

        assert_eq!(command.get_args().count(), 0);
        assert_eq!(env["GIT_CONFIG_COUNT"], "1");
        assert_eq!(
            env["GIT_CONFIG_KEY_0"],
            "http.https://github.com/.extraheader"
        );
        assert!(env["GIT_CONFIG_VALUE_0"].starts_with("Authorization: Basic "));
        assert!(!env["GIT_CONFIG_VALUE_0"].contains("secret-token"));
    }
}
