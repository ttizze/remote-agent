//! Pure pull-request facts and selection rules.
//!
//! Provider adapters populate these values at the Host boundary.  The rules in
//! this module deliberately operate on those values only, so list ordering,
//! badges, composer matches, and linked-thread selection are replayable on
//! every native client through the shared core.
use crate::{LinkedPullRequest, Timestamp};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestState {
    Open,
    Closed,
    Merged,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestChecksState {
    Success,
    Failure,
    Pending,
    Neutral,
    Skipped,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestMergeability {
    Mergeable,
    Conflicting,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestAction {
    Merge,
    MarkReady,
    MarkDraft,
    Close,
    Reopen,
    UpdateBranch,
    EnableAutoMerge,
    DisableAutoMerge,
    Revert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestLinkSource {
    Manual,
    Created,
    Agent,
    Stack,
    #[serde(rename = "stack-dismissed")]
    StackDismissed,
}

impl Default for PullRequestLinkSource {
    fn default() -> Self {
        Self::Manual
    }
}

impl PullRequestLinkSource {
    pub fn visible(self) -> bool {
        self != Self::StackDismissed
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Created => "created",
            Self::Agent => "agent",
            Self::Stack => "stack",
            Self::StackDismissed => "stack-dismissed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestKey {
    pub host: String,
    pub repository: String,
    pub number: u64,
}

impl PullRequestKey {
    pub fn new(host: impl Into<String>, repository: impl Into<String>, number: u64) -> Self {
        Self {
            host: normalize_host(&host.into()),
            repository: normalize_repository(&repository.into()),
            number,
        }
    }

    pub fn canonical(&self) -> String {
        format!("{}/{}/{}", self.host, self.repository, self.number)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestActor {
    pub login: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestCheck {
    pub name: String,
    pub state: PullRequestChecksState,
    pub url: Option<String>,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestComment {
    pub id: String,
    pub author: Option<PullRequestActor>,
    pub body: String,
    pub created_at: Timestamp,
    pub updated_at: Option<Timestamp>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestReviewThread {
    pub id: String,
    pub path: String,
    pub line: Option<u64>,
    pub body: String,
    pub is_resolved: bool,
    pub comments: Vec<PullRequestComment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestStackLayer {
    pub number: u64,
    pub head_branch: String,
    #[serde(default)]
    pub head_sha: Option<String>,
    pub state: PullRequestState,
    #[serde(default)]
    pub is_draft: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestStack {
    pub id: String,
    pub number: u64,
    pub url: String,
    pub base: String,
    pub layers: Vec<PullRequestStackLayer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestSummary {
    pub key: PullRequestKey,
    pub project: Option<String>,
    pub url: String,
    pub title: String,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub head_branch: String,
    pub head_sha: Option<String>,
    pub base_branch: String,
    pub opened_at: Option<Timestamp>,
    pub closed_at: Option<Timestamp>,
    pub merged_at: Option<Timestamp>,
    pub updated_at: Timestamp,
    pub observed_at: Timestamp,
    pub author: Option<PullRequestActor>,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub changed_files: Option<u64>,
    pub review_decision: PullRequestReviewDecision,
    pub checks_state: PullRequestChecksState,
    pub mergeability: PullRequestMergeability,
    pub checks: Vec<PullRequestCheck>,
    pub labels: Vec<String>,
    pub stack: Option<PullRequestStack>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestDetail {
    pub summary: PullRequestSummary,
    pub body: String,
    pub comments: Vec<PullRequestComment>,
    pub review_threads: Vec<PullRequestReviewThread>,
    pub reviewers: Vec<PullRequestActor>,
    pub requested_reviewers: Vec<PullRequestActor>,
    pub viewer: Option<PullRequestActor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestWatch {
    pub started_at: Timestamp,
    pub head_sha: Option<String>,
    pub failed_checks: Vec<String>,
    pub passed: bool,
    pub remarks_through: Option<Timestamp>,
    pub remark_ids: Vec<String>,
    pub conflicting: bool,
    pub wakes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestLink {
    pub host: String,
    pub repository: String,
    pub number: u64,
    pub url: String,
    pub source: PullRequestLinkSource,
    pub linked_at: Timestamp,
    pub snapshot: Option<PullRequestSummary>,
    pub stack: Option<PullRequestStack>,
    pub watch: Option<PullRequestWatch>,
}

impl PullRequestLink {
    pub fn key(&self) -> PullRequestKey {
        PullRequestKey::new(&self.host, &self.repository, self.number)
    }

    pub fn visible(&self) -> bool {
        self.source.visible()
    }

    pub fn effective_updated_at(&self) -> &Timestamp {
        self.snapshot
            .as_ref()
            .map(|snapshot| &snapshot.updated_at)
            .unwrap_or(&self.linked_at)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestBadge {
    Open,
    Draft,
    Merged,
    Closed,
    ChecksFailed,
    ChecksPending,
    ChangesRequested,
    Conflicting,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestSearchMatch {
    pub key: PullRequestKey,
    pub project: Option<String>,
    pub title: String,
    pub url: String,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub head_branch: String,
    pub base_branch: String,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPullRequestUrl {
    pub key: PullRequestKey,
    pub url: String,
}

pub fn normalize_host(value: &str) -> String {
    let value = value.trim();
    let value = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
        .unwrap_or(value);
    value.trim_end_matches('/').to_ascii_lowercase()
}

pub fn normalize_repository(value: &str) -> String {
    value
        .trim()
        .trim_matches('/')
        .trim_end_matches(".git")
        .to_ascii_lowercase()
}

pub fn normalize_pull_request_key(key: &PullRequestKey) -> PullRequestKey {
    PullRequestKey::new(&key.host, &key.repository, key.number)
}

pub fn pull_request_key_of(link: &PullRequestLink) -> PullRequestKey {
    link.key()
}

pub fn visible_pull_requests(links: &[PullRequestLink]) -> Vec<PullRequestLink> {
    let mut result = links
        .iter()
        .filter(|link| link.visible())
        .cloned()
        .collect::<Vec<_>>();
    result.sort_by(|left, right| {
        right
            .effective_updated_at()
            .cmp(left.effective_updated_at())
            .then_with(|| right.linked_at.cmp(&left.linked_at))
            .then_with(|| left.key().canonical().cmp(&right.key().canonical()))
    });
    result
}

pub fn resolve_current_pull_request(links: &[PullRequestLink]) -> Option<PullRequestLink> {
    let visible = visible_pull_requests(links);
    let open = visible.iter().filter(|link| {
        link.snapshot
            .as_ref()
            .is_none_or(|snapshot| snapshot.state == PullRequestState::Open)
    });
    let open = open.collect::<Vec<_>>();
    if open.is_empty() {
        return visible.into_iter().next();
    }
    // A provider supplied stack order is authoritative. The last layer is the
    // one the thread can act on; otherwise the most recently linked/updated
    // request is the current one.
    open.iter()
        .filter_map(|link| {
            let stack = link.stack.as_ref()?;
            let position = stack
                .layers
                .iter()
                .position(|layer| layer.number == link.number)?;
            Some((position, link))
        })
        .max_by_key(|(position, link)| (*position, link.effective_updated_at(), &link.linked_at))
        .map(|(_, link)| (*link).clone())
        .or_else(|| open.into_iter().next().cloned())
}

pub fn resolve_pull_request_chains(links: &[PullRequestLink]) -> Vec<Vec<PullRequestLink>> {
    let visible = visible_pull_requests(links);
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();

    // Native stacks keep the host's layer order and are not re-derived from
    // branch names. A dismissed layer is already absent from `visible`.
    let mut native = BTreeSet::new();
    for link in &visible {
        let Some(stack) = link.stack.as_ref() else {
            continue;
        };
        let stack_key = format!("{}/{}:{}", link.host, link.repository, stack.id);
        if native.contains(&stack_key) {
            continue;
        }
        let mut members = visible
            .iter()
            .filter(|candidate| {
                candidate.stack.as_ref().is_some_and(|other| {
                    other.id == stack.id
                        && candidate.host.eq_ignore_ascii_case(&link.host)
                        && candidate.repository.eq_ignore_ascii_case(&link.repository)
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        members.sort_by_key(|candidate| {
            stack
                .layers
                .iter()
                .position(|layer| layer.number == candidate.number)
                .unwrap_or(usize::MAX)
        });
        for member in &members {
            seen.insert(member.key().canonical());
        }
        native.insert(stack_key);
        if !members.is_empty() {
            result.push(members);
        }
    }

    // Derived chains are bottom-to-top and only use an unambiguous head
    // branch. Reused branch names stay as independent rows.
    let remaining = visible
        .iter()
        .filter(|link| !seen.contains(&link.key().canonical()))
        .cloned()
        .collect::<Vec<_>>();
    let mut by_head = std::collections::BTreeMap::<String, Option<PullRequestLink>>::new();
    for link in &remaining {
        let Some(snapshot) = link.snapshot.as_ref() else {
            continue;
        };
        let branch = format!("{}:{}:{}", link.host, link.repository, snapshot.head_branch);
        let value = by_head.entry(branch).or_insert_with(|| Some(link.clone()));
        if value
            .as_ref()
            .is_some_and(|existing| existing.key() != link.key())
        {
            *value = None;
        }
    }
    let mut has_child = BTreeSet::new();
    for link in &remaining {
        let Some(snapshot) = link.snapshot.as_ref() else {
            continue;
        };
        let branch = format!("{}:{}:{}", link.host, link.repository, snapshot.base_branch);
        if let Some(Some(parent)) = by_head.get(&branch) {
            if parent.key() != link.key() {
                has_child.insert(parent.key().canonical());
            }
        }
    }
    for top in &remaining {
        if seen.contains(&top.key().canonical()) || has_child.contains(&top.key().canonical()) {
            continue;
        }
        let mut chain = Vec::new();
        let mut cursor = Some(top.clone());
        while let Some(link) = cursor {
            let key = link.key().canonical();
            if !seen.insert(key) {
                break;
            }
            chain.push(link.clone());
            cursor = link.snapshot.as_ref().and_then(|snapshot| {
                let branch = format!("{}:{}:{}", link.host, link.repository, snapshot.base_branch);
                by_head.get(&branch).and_then(|candidate| candidate.clone())
            });
        }
        chain.reverse();
        if !chain.is_empty() {
            result.push(chain);
        }
    }
    for link in remaining {
        if seen.insert(link.key().canonical()) {
            result.push(vec![link]);
        }
    }
    result
}

pub fn resolve_pull_request_badge(link: &PullRequestLink) -> PullRequestBadge {
    let Some(snapshot) = link.snapshot.as_ref() else {
        return PullRequestBadge::Unknown;
    };
    if snapshot.mergeability == PullRequestMergeability::Conflicting {
        return PullRequestBadge::Conflicting;
    }
    match snapshot.checks_state {
        PullRequestChecksState::Failure => return PullRequestBadge::ChecksFailed,
        PullRequestChecksState::Pending => return PullRequestBadge::ChecksPending,
        _ => {}
    }
    if snapshot.review_decision == PullRequestReviewDecision::ChangesRequested {
        return PullRequestBadge::ChangesRequested;
    }
    match snapshot.state {
        PullRequestState::Merged => PullRequestBadge::Merged,
        PullRequestState::Closed => PullRequestBadge::Closed,
        PullRequestState::Open if snapshot.is_draft => PullRequestBadge::Draft,
        PullRequestState::Open => PullRequestBadge::Open,
        PullRequestState::Unknown => PullRequestBadge::Unknown,
    }
}

pub fn pull_request_search_terms(link: &PullRequestLink) -> Vec<String> {
    let mut terms = vec![
        link.host.clone(),
        link.repository.clone(),
        link.number.to_string(),
        format!("#{}", link.number),
        link.url.clone(),
    ];
    if let Some(snapshot) = &link.snapshot {
        terms.push(snapshot.title.clone());
        terms.push(snapshot.head_branch.clone());
        terms.push(snapshot.base_branch.clone());
    }
    terms
}

pub fn filter_composer_pull_request_matches(
    links: &[PullRequestLink],
    project: Option<&str>,
    repository: &str,
    query: &str,
    limit: usize,
) -> Vec<PullRequestSearchMatch> {
    let repository = normalize_repository(repository);
    let query = query.trim().to_ascii_lowercase();
    let mut matches = visible_pull_requests(links)
        .into_iter()
        .filter(|link| normalize_repository(&link.repository) == repository)
        .filter(|link| {
            project.is_none_or(|project| {
                link.snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.project.as_deref())
                    == Some(project)
            })
        })
        .filter(|link| {
            query.is_empty()
                || pull_request_search_terms(link)
                    .iter()
                    .any(|term| term.to_ascii_lowercase().contains(&query))
        })
        .map(|link| PullRequestSearchMatch {
            key: link.key(),
            project: link
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.project.clone()),
            title: link
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.title.clone())
                .unwrap_or_else(|| format!("#{}", link.number)),
            url: link.url.clone(),
            state: link
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.state)
                .unwrap_or(PullRequestState::Unknown),
            is_draft: link
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.is_draft),
            head_branch: link
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.head_branch.clone())
                .unwrap_or_default(),
            base_branch: link
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.base_branch.clone())
                .unwrap_or_default(),
            updated_at: link.effective_updated_at().clone(),
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        let left_exact = left.title.to_ascii_lowercase() == query;
        let right_exact = right.title.to_ascii_lowercase() == query;
        right_exact
            .cmp(&left_exact)
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.key.canonical().cmp(&right.key.canonical()))
    });
    matches.truncate(limit);
    matches
}

pub fn parse_change_request_url(value: &str) -> Option<ParsedPullRequestUrl> {
    let url = url::Url::parse(value.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let segments = url
        .path_segments()?
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let githubish = host == "github.com"
        || host.ends_with(".github.com")
        || host
            .split('.')
            .any(|part| part.eq_ignore_ascii_case("github"));
    let (owner, repository, number) =
        if githubish && segments.len() >= 4 && segments[2].eq_ignore_ascii_case("pull") {
            (segments[0], segments[1], segments[3])
        } else if segments.len() >= 5
            && segments[2] == "-"
            && segments[3].eq_ignore_ascii_case("merge_requests")
        {
            (segments[0], segments[1], segments[4])
        } else if segments.len() >= 4 && segments[2].eq_ignore_ascii_case("pulls") {
            (segments[0], segments[1], segments[3])
        } else {
            return None;
        };
    let number = number.parse().ok()?;
    Some(ParsedPullRequestUrl {
        key: PullRequestKey::new(host, format!("{owner}/{repository}"), number),
        url: value.trim().to_owned(),
    })
}

pub fn github_browser_url(host: &str, repository: &str, number: u64) -> String {
    format!(
        "https://{}/{}/pull/{}",
        normalize_host(host),
        normalize_repository(repository),
        number
    )
}

pub fn linked_pull_request(link: &PullRequestLink) -> LinkedPullRequest {
    LinkedPullRequest {
        project: link
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.project.clone())
            .unwrap_or_default(),
        repository: link.repository.clone(),
        number: link.number,
        url: link.url.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(value: &str) -> Timestamp {
        Timestamp::parse(value).unwrap()
    }

    fn link(number: u64, state: PullRequestState, updated_at: &str) -> PullRequestLink {
        PullRequestLink {
            host: "github.com".into(),
            repository: "owner/repo".into(),
            number,
            url: github_browser_url("github.com", "owner/repo", number),
            source: PullRequestLinkSource::Manual,
            linked_at: at("2026-10-01T00:00:00Z"),
            snapshot: Some(PullRequestSummary {
                key: PullRequestKey::new("github.com", "owner/repo", number),
                project: Some("project".into()),
                url: github_browser_url("github.com", "owner/repo", number),
                title: format!("PR {number}"),
                state,
                is_draft: false,
                head_branch: format!("feature/{number}"),
                head_sha: None,
                base_branch: "main".into(),
                opened_at: None,
                closed_at: None,
                merged_at: None,
                updated_at: at(updated_at),
                observed_at: at("2026-10-02T00:00:00Z"),
                author: None,
                additions: None,
                deletions: None,
                changed_files: None,
                review_decision: PullRequestReviewDecision::Unknown,
                checks_state: PullRequestChecksState::Success,
                mergeability: PullRequestMergeability::Mergeable,
                checks: vec![],
                labels: vec![],
                stack: None,
            }),
            stack: None,
            watch: None,
        }
    }

    #[test]
    fn parses_github_and_gitlab_paths() {
        let github = parse_change_request_url("https://github.com/Owner/Repo/pull/42").unwrap();
        assert_eq!(
            github.key,
            PullRequestKey::new("github.com", "owner/repo", 42)
        );
        let gitlab =
            parse_change_request_url("https://gitlab.example/owner/repo/-/merge_requests/7")
                .unwrap();
        assert_eq!(gitlab.key.number, 7);
        assert!(parse_change_request_url("https://example/owner/repo/issues/1").is_none());
    }

    #[test]
    fn open_request_wins_over_newer_terminal_request() {
        let links = vec![
            link(1, PullRequestState::Merged, "2026-10-03T00:00:00Z"),
            link(2, PullRequestState::Open, "2026-10-01T00:00:00Z"),
        ];
        assert_eq!(resolve_current_pull_request(&links).unwrap().number, 2);
    }

    #[test]
    fn hidden_stack_entries_are_excluded_and_matches_are_bounded() {
        let mut hidden = link(3, PullRequestState::Open, "2026-10-03T00:00:00Z");
        hidden.source = PullRequestLinkSource::StackDismissed;
        let matches = filter_composer_pull_request_matches(
            &[
                hidden,
                link(2, PullRequestState::Open, "2026-10-02T00:00:00Z"),
            ],
            Some("project"),
            "OWNER/REPO",
            "#2",
            1,
        );
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].key.number, 2);
    }

    #[test]
    fn badge_prioritizes_checks_and_conflicts() {
        let mut link = link(1, PullRequestState::Open, "2026-10-01T00:00:00Z");
        link.snapshot.as_mut().unwrap().checks_state = PullRequestChecksState::Failure;
        assert_eq!(
            resolve_pull_request_badge(&link),
            PullRequestBadge::ChecksFailed
        );
        link.snapshot.as_mut().unwrap().checks_state = PullRequestChecksState::Success;
        link.snapshot.as_mut().unwrap().mergeability = PullRequestMergeability::Conflicting;
        assert_eq!(
            resolve_pull_request_badge(&link),
            PullRequestBadge::Conflicting
        );
    }
}
