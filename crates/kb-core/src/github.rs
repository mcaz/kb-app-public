//! GitHub backup repository の安全確認。
//!
//! Git URL の構文や `git push` の成功は visibility の証明にならない。個人データを送る前に
//! 認証済み REST API の repository 応答で `private` と `permissions.push` を確認する。
//! 401 / 404 / network error / 欠落フィールドはすべて「確認不能」として閉じる。
//!
//! 現段階の認証情報は system Git の credential helper から**メモリ上だけ**で借りる。
//! OAuth device flow + OS keychain へ置き換えても、下の検査結果を upload gate にする構造は同じ。

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

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
        bail!("GitHub repository の URL ではない")
    };
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    let owner = parts.next().unwrap_or_default();
    let repo = parts.next().unwrap_or_default();
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        bail!("GitHub repository URL は owner/repository の形で指定する");
    }
    if !valid_name(owner) || !valid_name(repo) {
        bail!("GitHub repository URL に不正な owner/repository 名がある");
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

/// system Git の credential helper から GitHub token を借りる。
/// stdout は credential 本体なので、失敗メッセージへ含めない。
fn credential_token() -> Result<String> {
    let mut child = Command::new("git")
        .args(["credential", "fill"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("GitHub 認証情報の確認を開始できない")?;
    child
        .stdin
        .as_mut()
        .context("GitHub 認証情報へ問い合わせられない")?
        .write_all(b"protocol=https\nhost=github.com\n\n")?;
    let output = child
        .wait_with_output()
        .context("GitHub 認証情報を確認できない")?;
    if !output.status.success() {
        bail!("GitHub にサインインしていないため private repository を確認できない");
    }
    let text = String::from_utf8(output.stdout).context("GitHub 認証情報の形式が不正")?;
    text.lines()
        .find_map(|line| line.strip_prefix("password="))
        .filter(|token| !token.trim().is_empty())
        .map(str::to_string)
        .context("GitHub にサインインしていないため private repository を確認できない")
}

/// upload の直前に呼ぶ fail-closed gate。
pub fn verify_private_repository(url: &str) -> Result<VerifiedPrivateRepository> {
    let token = credential_token()?;
    verify_private_repository_at(API_BASE, url, &token)
}

/// 認証中の GitHub account に空の private repository を作り、応答を信用せず再検査する。
pub fn create_private_repository(name: &str) -> Result<VerifiedPrivateRepository> {
    let name = name.trim();
    if name.is_empty() || !valid_name(name) || matches!(name, "." | "..") {
        bail!("repository 名は英数字・ハイフン・アンダースコア・ピリオドで指定する");
    }
    let token = credential_token()?;
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
            ureq::Error::StatusCode(401 | 403) => {
                anyhow::anyhow!("private repository を作る GitHub 権限がない")
            }
            ureq::Error::StatusCode(422) => {
                anyhow::anyhow!("同名の repository があるか、名前を使用できない")
            }
            other => anyhow::anyhow!("private repository の作成に失敗: {other}"),
        })?;
    let text = response
        .body_mut()
        .read_to_string()
        .context("GitHub repository 作成応答を読めない")?;
    let created: RepositoryResponse =
        serde_json::from_str(&text).context("GitHub repository 作成応答の形式を確認できない")?;
    if !created.private || created.visibility.as_deref() != Some("private") {
        bail!("作成された repository が private と確認できないため接続しない");
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
            ureq::Error::StatusCode(401 | 403 | 404) => anyhow::anyhow!(
                "GitHub repository を認証済みAPIで確認できない(権限・URL・認証を確認)"
            ),
            other => anyhow::anyhow!("GitHub repository の安全確認に失敗: {other}"),
        })?;
    let body = response
        .body_mut()
        .read_to_string()
        .context("GitHub repository 応答を読めない")?;
    let repository: RepositoryResponse =
        serde_json::from_str(&body).context("GitHub repository 応答の形式を確認できない")?;
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
        bail!("GitHub API が別の repository を返したため接続しない");
    }
    if !repository.private || repository.visibility.as_deref() != Some("private") {
        bail!("接続先は private repository ではないためデータを送信しない");
    }
    if !repository
        .permissions
        .is_some_and(|permissions| permissions.push)
    {
        bail!("接続先への書き込み権限を確認できないためデータを送信しない");
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
