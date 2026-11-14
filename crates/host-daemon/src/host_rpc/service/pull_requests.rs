use super::super::storage::pull_requests::Update;
use super::*;
use agent_protocol::pull_requests::{self as pr, Action, PullRequest, Reply, Request};
use std::{path::Path, time::Duration};

const FIELDS: &str = "url,number,title,state,isDraft,headRefName,baseRefName,updatedAt";

impl HostRpcService {
    pub(super) async fn pull_requests(&self, request: &Request) -> Result<Reply, Failure> {
        request
            .validate()
            .map_err(|e| Failure::new("invalid_params", e))?;
        let runtime = &self.inner.conversation;
        let header = runtime
            .storage
            .header(&request.session)
            .map_err(super::super::runtime::storage_failure)?;
        let cwd = header.state.workspace.cwd().map(std::path::PathBuf::from);
        let mut reply = Reply {
            linked: header.details.pull_requests,
            ..Default::default()
        };
        let update = match &request.action {
            Action::Link { url } => Some(Update::Link(read(cwd.as_deref(), url).await?)),
            Action::Unlink { url } => Some(Update::Unlink(url.clone())),
            Action::Read { refresh: false } => None,
            Action::Read { refresh: true } => {
                let path = cwd.as_deref();
                let linked = futures_util::future::join_all(
                    std::mem::take(&mut reply.linked)
                        .into_iter()
                        .map(|mut link| async move {
                            match read(path, &link.url).await {
                                Ok(value) => value,
                                Err(error) => {
                                    link.error = Some(error.message);
                                    link
                                }
                            }
                        }),
                );
                let branch = async {
                    match path {
                        Some(path) => current(path).await,
                        None => Ok(Vec::new()),
                    }
                };
                let (linked, current) = tokio::join!(linked, branch);
                reply.linked = linked;
                match current {
                    Ok(value) => reply.current = value,
                    Err(error) => reply.current_error = Some(error.message),
                }
                Some(Update::Refresh(reply.linked.clone()))
            }
        };
        if let Some(update) = update {
            runtime
                .owners
                .write(&request.session, Write::PullRequests(update))
                .await
                .map_err(super::super::runtime::storage_failure)?;
            reply.linked = runtime
                .storage
                .header(&request.session)
                .map_err(super::super::runtime::storage_failure)?
                .details
                .pull_requests;
        }
        Ok(reply)
    }
}

async fn gh(cwd: Option<&Path>, args: &[&str]) -> Result<Vec<u8>, Failure> {
    let mut command = tokio::process::Command::new("gh");
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let output = tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .map_err(|_| {
            Failure::new(
                "pull_request_timeout",
                "GitHubへの問い合わせがタイムアウトしました。",
            )
        })?
        .map_err(|_| {
            Failure::new(
                "pull_request_unavailable",
                "GitHub CLIを起動できません。Hostでghをインストールしてください。",
            )
        })?;
    if !output.status.success() {
        return Err(Failure::new(
            "pull_request_failed",
            "PRを取得できません。HostのGitHub認証とリポジトリへのアクセスを確認してください。",
        ));
    }
    if output.stdout.len() > 1024 * 1024 {
        return Err(Failure::new(
            "pull_request_too_large",
            "PRの応答が大きすぎます。",
        ));
    }
    Ok(output.stdout)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GithubPullRequest {
    url: String,
    number: u64,
    title: String,
    state: String,
    is_draft: bool,
    head_ref_name: String,
    base_ref_name: String,
    updated_at: String,
}

impl GithubPullRequest {
    fn into_pr(self) -> Result<PullRequest, Failure> {
        pr::validate_url(&self.url).map_err(|e| Failure::new("invalid_pull_request", e))?;
        let state = match self.state.as_str() {
            "OPEN" => pr::State::Open,
            "CLOSED" => pr::State::Closed,
            "MERGED" => pr::State::Merged,
            _ => return Err(Failure::new("invalid_pull_request", "unknown PR state")),
        };
        if !self.url.ends_with(&format!("/{}", self.number)) {
            return Err(Failure::new(
                "invalid_pull_request",
                "PR number differs from URL",
            ));
        }
        Ok(PullRequest {
            url: self.url,
            number: self.number,
            title: self.title,
            state,
            draft: self.is_draft,
            head: self.head_ref_name,
            base: self.base_ref_name,
            updated_at: self.updated_at,
            error: None,
        })
    }
}

async fn read(cwd: Option<&Path>, url: &str) -> Result<PullRequest, Failure> {
    let bytes = gh(cwd, &["pr", "view", url, "--json", FIELDS]).await?;
    let value: GithubPullRequest = serde_json::from_slice(&bytes)
        .map_err(|_| Failure::new("invalid_pull_request", "invalid GitHub PR response"))?;
    let value = value.into_pr()?;
    if !value.url.eq_ignore_ascii_case(url) {
        return Err(Failure::new(
            "invalid_pull_request",
            "GitHub returned a different PR",
        ));
    }
    Ok(value)
}

async fn current(cwd: &Path) -> Result<Vec<PullRequest>, Failure> {
    let path = cwd.to_owned();
    let head = tokio::task::spawn_blocking(move || {
        crate::git::bounded(
            &path,
            &["symbolic-ref", "--short", "HEAD"],
            Duration::from_secs(5),
        )
    })
    .await
    .map_err(|e| Failure::new("git_failed", e.to_string()))?
    .map_err(|_| Failure::new("git_failed", "現在のブランチを取得できません。"))?;
    let head = String::from_utf8(head.stdout)
        .map_err(|_| Failure::new("git_failed", "invalid branch name"))?;
    let bytes = gh(
        Some(cwd),
        &[
            "pr",
            "list",
            "--head",
            head.trim(),
            "--state",
            "all",
            "--limit",
            "20",
            "--json",
            FIELDS,
        ],
    )
    .await?;
    let values: Vec<GithubPullRequest> = serde_json::from_slice(&bytes)
        .map_err(|_| Failure::new("invalid_pull_request", "invalid GitHub PR list"))?;
    values.into_iter().map(GithubPullRequest::into_pr).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_metadata_preserves_drafts_and_rejects_mismatched_resources() {
        let response = serde_json::json!({
            "url": "https://github.com/acme/repo/pull/12", "number": 12,
            "title": "Fix", "state": "OPEN", "isDraft": true,
            "headRefName": "fix", "baseRefName": "main", "updatedAt": "2026-11-14T00:00:00Z"
        });
        let read = |value| {
            serde_json::from_value::<GithubPullRequest>(value)
                .unwrap()
                .into_pr()
        };
        let metadata = read(response.clone()).unwrap();
        assert!(metadata.draft);
        assert_eq!(metadata.state, pr::State::Open);
        assert_eq!(metadata.head, "fix");
        assert_eq!(metadata.base, "main");
        for (key, value) in [
            (
                "url",
                serde_json::json!("https://example.com/acme/repo/pull/12"),
            ),
            ("number", serde_json::json!(13)),
            ("state", serde_json::json!("UNKNOWN")),
        ] {
            let mut invalid = response.clone();
            invalid[key] = value;
            assert!(read(invalid).is_err());
        }
    }
}
