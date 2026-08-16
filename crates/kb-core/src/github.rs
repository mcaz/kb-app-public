//! GitHub backup repository の安全確認。
//!
//! Git URL の構文や `git push` の成功は visibility の証明にならない。個人データを送る前に
//! 認証済み REST API の repository 応答で `private` と `permissions.push` を確認する。
//! 401 / 404 / network error / 欠落フィールドはすべて「確認不能」として閉じる。
//!
//! 認証は OAuth device flow で取得し OS keychain へ保存する。下の検査結果を upload gate にし、
//! credential が失効したら再利用せずサインインへ戻す。

use anyhow::Result;
use serde::Deserialize;

use crate::backup::{BackupFailureKind, failure};

const API_BASE: &str = "https://api.github.com";
const API_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryName {
    pub owner: String,
    pub repo: String,
}

impl RepositoryName {
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPrivateRepository {
    pub id: u64,
    pub full_name: String,
    pub clone_url: String,
}

#[derive(Debug, Deserialize)]
struct RepositoryResponse {
    id: u64,
    full_name: String,
    private: bool,
    visibility: Option<String>,
    clone_url: String,
    permissions: Option<RepositoryPermissions>,
}

#[derive(Debug, Deserialize)]
struct RepositoryPermissions {
    push: bool,
}

/// kb-app が受け付ける GitHub repository URL を owner/name へ正規化する。
pub fn parse_repository_url(url: &str) -> Result<RepositoryName> {
    let trimmed = url.trim().trim_end_matches('/');
    let path = if let Some(path) = trimmed.strip_prefix("git@github.com:") {
        path
    } else if let Some(path) = trimmed.strip_prefix("https://github.com/") {
        path
    } else {
        return Err(failure(
            BackupFailureKind::InvalidRepository,
            "GitHub repository の URL ではない",
        ));
    };
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    let owner = parts.next().unwrap_or_default();
    let repo = parts.next().unwrap_or_default();
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return Err(failure(
            BackupFailureKind::InvalidRepository,
            "GitHub repository URL は owner/repository の形で指定する",
        ));
    }
    if !valid_name(owner) || !valid_name(repo) {
        return Err(failure(
            BackupFailureKind::InvalidRepository,
            "GitHub repository URL に不正な owner/repository 名がある",
        ));
    }
    Ok(RepositoryName {
        owner: owner.to_string(),
        repo: repo.to_string(),
    })
}

fn valid_name(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// upload の直前に呼ぶ fail-closed gate。
pub fn verify_private_repository(url: &str) -> Result<VerifiedPrivateRepository> {
    let token = crate::github_auth::access_token()?;
    verify_private_repository_at(API_BASE, url, &token)
}

/// 認証中の GitHub account に空の private repository を作り、応答を信用せず再検査する。
pub fn create_private_repository(name: &str) -> Result<VerifiedPrivateRepository> {
    let name = name.trim();
    if name.is_empty() || !valid_name(name) || matches!(name, "." | "..") {
        return Err(failure(
            BackupFailureKind::InvalidRepository,
            "repository 名は英数字・ハイフン・アンダースコア・ピリオドで指定する",
        ));
    }
    let token = crate::github_auth::access_token()?;
    create_private_repository_at(API_BASE, name, &token)
}

fn create_private_repository_at(
    api_base: &str,
    name: &str,
    token: &str,
) -> Result<VerifiedPrivateRepository> {
    let endpoint = format!("{}/user/repos", api_base.trim_end_matches('/'));
    let body = serde_json::to_vec(&serde_json::json!({
        "name": name,
        "private": true,
        "auto_init": false
    }))?;
    let mut response = ureq::post(&endpoint)
        .config()
        .timeout_global(Some(API_TIMEOUT))
        .build()
        .header("Accept", "application/vnd.github+json")
        .header("Authorization", &format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", "kb-app")
        .send(body.as_slice())
        .map_err(|error| match error {
            ureq::Error::StatusCode(401) => {
                crate::github_auth::invalidate();
                failure(
                    BackupFailureKind::Authentication,
                    "GitHub の認証が失効しているため private repository を作れない",
                )
            }
            ureq::Error::StatusCode(403) => failure(
                BackupFailureKind::Permission,
                "private repository を作る GitHub 権限がない",
            ),
            ureq::Error::StatusCode(422) => failure(
                BackupFailureKind::InvalidRepository,
                "同名の repository があるか、名前を使用できない",
            ),
            other => failure(
                BackupFailureKind::Network,
                format!("private repository の作成に失敗: {other}"),
            ),
        })?;
    let text = response.body_mut().read_to_string().map_err(|error| {
        failure(
            BackupFailureKind::PrivacyCheck,
            format!("GitHub repository 作成応答を読めない: {error}"),
        )
    })?;
    let created: RepositoryResponse = serde_json::from_str(&text).map_err(|error| {
        failure(
            BackupFailureKind::PrivacyCheck,
            format!("GitHub repository 作成応答の形式を確認できない: {error}"),
        )
    })?;
    if !created.private || created.visibility.as_deref() != Some("private") {
        return Err(failure(
            BackupFailureKind::PrivacyCheck,
            "作成された repository が private と確認できないため接続しない",
        ));
    }
    // 作成応答だけを証明にせず、upload gate と同じ GET をもう一度通す。
    verify_private_repository_at(api_base, &created.clone_url, token)
}

fn verify_private_repository_at(
    api_base: &str,
    url: &str,
    token: &str,
) -> Result<VerifiedPrivateRepository> {
    let name = parse_repository_url(url)?;
    let endpoint = format!(
        "{}/repos/{}/{}",
        api_base.trim_end_matches('/'),
        name.owner,
        name.repo
    );
    let mut response = ureq::get(&endpoint)
        .config()
        .timeout_global(Some(API_TIMEOUT))
        .build()
        .header("Accept", "application/vnd.github+json")
        .header("Authorization", &format!("Bearer {token}"))
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", "kb-app")
        .call()
        .map_err(|error| match error {
            ureq::Error::StatusCode(401) => {
                crate::github_auth::invalidate();
                failure(
                    BackupFailureKind::Authentication,
                    "GitHub の認証が失効しているため repository を確認できない",
                )
            }
            ureq::Error::StatusCode(403) => failure(
                BackupFailureKind::Permission,
                "GitHub repository を確認する権限がない",
            ),
            ureq::Error::StatusCode(404) => failure(
                BackupFailureKind::RemoteMissing,
                "GitHub repository が見つからないか、アクセスできない",
            ),
            other => failure(
                BackupFailureKind::Network,
                format!("GitHub repository の安全確認に失敗: {other}"),
            ),
        })?;
    let body = response.body_mut().read_to_string().map_err(|error| {
        failure(
            BackupFailureKind::PrivacyCheck,
            format!("GitHub repository 応答を読めない: {error}"),
        )
    })?;
    let repository: RepositoryResponse = serde_json::from_str(&body).map_err(|error| {
        failure(
            BackupFailureKind::PrivacyCheck,
            format!("GitHub repository 応答の形式を確認できない: {error}"),
        )
    })?;
    evaluate(repository, &name)
}

fn evaluate(
    repository: RepositoryResponse,
    requested: &RepositoryName,
) -> Result<VerifiedPrivateRepository> {
    if !repository
        .full_name
        .eq_ignore_ascii_case(&requested.full_name())
    {
        return Err(failure(
            BackupFailureKind::InvalidRepository,
            "GitHub API が別の repository を返したため接続しない",
        ));
    }
    if !repository.private || repository.visibility.as_deref() != Some("private") {
        return Err(failure(
            BackupFailureKind::PrivacyCheck,
            "接続先は private repository ではないためデータを送信しない",
        ));
    }
    if !repository
        .permissions
        .is_some_and(|permissions| permissions.push)
    {
        return Err(failure(
            BackupFailureKind::Permission,
            "接続先への書き込み権限を確認できないためデータを送信しない",
        ));
    }
    Ok(VerifiedPrivateRepository {
        id: repository.id,
        full_name: repository.full_name,
        clone_url: repository.clone_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(private: bool, visibility: Option<&str>, push: Option<bool>) -> RepositoryResponse {
        RepositoryResponse {
            id: 42,
            full_name: "mcaz/my-notes".into(),
            private,
            visibility: visibility.map(str::to_string),
            clone_url: "https://github.com/mcaz/my-notes.git".into(),
            permissions: push.map(|push| RepositoryPermissions { push }),
        }
    }

    #[test]
    fn parses_supported_repository_urls_without_accepting_subpaths() {
        let https = parse_repository_url("https://github.com/mcaz/my-notes.git/").unwrap();
        assert_eq!(https.full_name(), "mcaz/my-notes");
        let ssh = parse_repository_url("git@github.com:mcaz/my-notes.git").unwrap();
        assert_eq!(ssh, https);
        assert!(parse_repository_url("https://github.com/mcaz/my-notes/issues").is_err());
        assert!(parse_repository_url("https://example.com/mcaz/my-notes").is_err());
    }

    #[test]
    fn only_private_repository_with_explicit_push_permission_passes() {
        let name = parse_repository_url("https://github.com/mcaz/my-notes").unwrap();
        assert!(evaluate(response(true, Some("private"), Some(true)), &name).is_ok());
        assert!(evaluate(response(false, Some("public"), Some(true)), &name).is_err());
        assert!(evaluate(response(true, Some("private"), Some(false)), &name).is_err());
        assert!(evaluate(response(true, Some("private"), None), &name).is_err());
        assert!(evaluate(response(true, None, Some(true)), &name).is_err());
    }
}
