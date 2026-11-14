//! Conversation links are saved independently of the checkout's current branch.
pub use crate::context::PullRequestState as State;
use crate::session::SessionRef;
use serde::{Deserialize, Serialize};

pub const LINK_LIMIT: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub url: String,
    pub number: u64,
    pub title: String,
    pub state: State,
    pub draft: bool,
    pub head: String,
    pub base: String,
    pub updated_at: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub session: SessionRef,
    pub action: Action,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Action {
    Read { refresh: bool },
    Link { url: String },
    Unlink { url: String },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    pub linked: Vec<PullRequest>,
    pub current: Vec<PullRequest>,
    pub current_error: Option<String>,
}

/// Never pass a flag, credential-bearing URL or arbitrary endpoint to `gh`.
pub fn validate_url(value: &str) -> Result<(), &'static str> {
    let url = url::Url::parse(value).map_err(|_| "invalid pull request URL")?;
    let parts = url
        .path_segments()
        .ok_or("invalid pull request URL")?
        .collect::<Vec<_>>();
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || parts.len() != 4
        || parts[2] != "pull"
        || parts[3]
            .parse::<u64>()
            .ok()
            .filter(|number| *number > 0)
            .is_none()
        || parts[..2].iter().any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        })
    {
        return Err("expected a GitHub pull request URL");
    }
    Ok(())
}

impl Request {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.session
            .validate()
            .map_err(|_| "invalid conversation")?;
        match &self.action {
            Action::Link { url } | Action::Unlink { url } => validate_url(url),
            _ => Ok(()),
        }
    }
}

/// Refresh cannot resurrect an unlinked PR or overwrite links added concurrently.
pub fn refresh(links: &[PullRequest], updates: &[PullRequest]) -> Vec<PullRequest> {
    links
        .iter()
        .map(|link| {
            updates
                .iter()
                .find(|update| update.url == link.url)
                .unwrap_or(link)
                .clone()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn urls_are_explicit_pr_resources_without_credentials_or_command_options() {
        assert!(validate_url("https://github.com/acme/repo/pull/12").is_ok());
        for url in [
            "--help",
            "https://github.com/acme/repo/pull/0",
            "https://user@github.com/acme/repo/pull/1",
            "https://github.com/acme/repo/pull/1?x=y",
            "https://example.com/acme/repo/pull/1",
            "https://github.com/acme/repo/pull/1/files",
        ] {
            assert!(validate_url(url).is_err(), "{url}");
        }
    }
}
