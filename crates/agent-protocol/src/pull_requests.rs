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

/// T3 ThreadPullRequestLink fields Bex records on the canonical thread.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPullRequestLink {
    pub host: String,
    pub repository: String,
    pub number: u64,
    pub url: String,
    pub source: String,
    pub linked_at: String,
    pub snapshot: Option<ThreadPullRequestSnapshot>,
    pub stack: Option<serde_json::Value>,
}

/// T3 ThreadPullRequestSnapshot fields Bex reads from `gh`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPullRequestSnapshot {
    pub state: State,
    pub title: String,
    pub head_branch: String,
    pub base_branch: String,
    pub is_draft: bool,
    pub updated_at: Option<String>,
    pub synced_at: String,
}

impl ThreadPullRequestLink {
    /// The link as clients show it. A failed refresh is reported, not saved.
    pub fn pull_request(&self) -> PullRequest {
        let snapshot = self.snapshot.as_ref();
        PullRequest {
            url: self.url.clone(),
            number: self.number,
            title: snapshot.map_or_else(String::new, |value| value.title.clone()),
            state: snapshot.map_or(State::Open, |value| value.state),
            draft: snapshot.is_some_and(|value| value.is_draft),
            head: snapshot.map_or_else(String::new, |value| value.head_branch.clone()),
            base: snapshot.map_or_else(String::new, |value| value.base_branch.clone()),
            updated_at: snapshot
                .and_then(|value| value.updated_at.clone())
                .unwrap_or_default(),
            error: None,
        }
    }
}

pub enum Update {
    Link(PullRequest),
    Unlink(String),
    Refresh(Vec<PullRequest>),
}

fn snapshot(value: &PullRequest, now: &str) -> ThreadPullRequestSnapshot {
    ThreadPullRequestSnapshot {
        state: value.state,
        title: value.title.clone(),
        head_branch: value.head.clone(),
        base_branch: value.base.clone(),
        is_draft: value.draft,
        updated_at: (!value.updated_at.is_empty()).then(|| value.updated_at.clone()),
        synced_at: now.into(),
    }
}

/// A thread's links after one update. A refresh cannot resurrect an unlinked
/// PR or overwrite links added concurrently, and a failed read keeps the last snapshot.
pub fn apply(
    links: &[ThreadPullRequestLink],
    update: Update,
    now: &str,
) -> Result<Vec<ThreadPullRequestLink>, &'static str> {
    let mut links = links.to_vec();
    match update {
        Update::Link(value) => {
            validate_url(&value.url)?;
            if let Some(link) = links.iter_mut().find(|link| link.url == value.url) {
                link.snapshot = Some(snapshot(&value, now));
            } else {
                if links.len() >= LINK_LIMIT {
                    return Err("too many linked pull requests");
                }
                let parts: Vec<_> = value.url.split('/').collect();
                links.push(ThreadPullRequestLink {
                    host: parts[2].into(),
                    repository: format!("{}/{}", parts[3], parts[4]),
                    number: value.number,
                    url: value.url.clone(),
                    source: "manual".into(),
                    linked_at: now.into(),
                    snapshot: Some(snapshot(&value, now)),
                    stack: None,
                });
            }
        }
        Update::Unlink(url) => links.retain(|link| link.url != url),
        Update::Refresh(updates) => {
            for link in &mut links {
                if let Some(value) = updates
                    .iter()
                    .find(|value| value.url == link.url && value.error.is_none())
                {
                    link.snapshot = Some(snapshot(value, now));
                }
            }
        }
    }
    Ok(links)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pr(number: u64) -> PullRequest {
        PullRequest {
            url: format!("https://github.com/acme/repo/pull/{number}"),
            number,
            title: "Fix".into(),
            state: State::Open,
            draft: false,
            head: "fix".into(),
            base: "main".into(),
            updated_at: "2026-11-16T00:00:00Z".into(),
            error: None,
        }
    }

    #[test]
    fn links_follow_t3_shape_and_stale_or_failed_refreshes_do_not_resurrect_or_erase() {
        let now = "2026-11-16T01:00:00Z";
        let links = apply(&[], Update::Link(pr(1)), now).unwrap();
        let links = apply(&links, Update::Link(pr(2)), now).unwrap();
        assert_eq!(links[1].host, "github.com");
        assert_eq!(links[1].repository, "acme/repo");
        assert_eq!(links[1].source, "manual");
        assert_eq!(links[1].linked_at, now);
        let links = apply(&links, Update::Unlink(pr(1).url), now).unwrap();
        let mut merged = pr(2);
        merged.state = State::Merged;
        let mut failed = pr(2);
        failed.error = Some("offline".into());
        let refreshed = apply(&links, Update::Refresh(vec![pr(1), merged.clone()]), now).unwrap();
        assert_eq!(
            refreshed
                .iter()
                .map(ThreadPullRequestLink::pull_request)
                .collect::<Vec<_>>(),
            [merged]
        );
        let kept = apply(&refreshed, Update::Refresh(vec![failed]), now).unwrap();
        assert_eq!(kept, refreshed);
        let mut full = Vec::new();
        for number in 1..=LINK_LIMIT as u64 {
            full = apply(&full, Update::Link(pr(number)), now).unwrap();
        }
        assert!(apply(&full, Update::Link(pr(99)), now).is_err());
        assert!(apply(&full, Update::Link(pr(1)), now).is_ok());
    }
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
