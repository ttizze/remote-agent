//! Pull-request views shared by desktop and mobile surfaces.
use crate::state::{PullRequestDiffChangeTypeInput, Snapshot, pull_request_diff_context_key};
use agent_domain::{
    PullRequestBadge, PullRequestDetail, PullRequestKey, PullRequestLink, PullRequestLinkSource,
    PullRequestSearchMatch, PullRequestState, PullRequestSummary, resolve_pull_request_badge,
};
use agent_protocol::pull_requests::PullRequestDiffFileContents;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestEntryView {
    pub key: PullRequestKey,
    pub title: String,
    pub url: String,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub head_branch: String,
    pub base_branch: String,
    pub updated_at: agent_domain::Timestamp,
    pub badge: PullRequestBadge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestListView {
    pub project_id: String,
    pub entries: Vec<PullRequestEntryView>,
    pub selected: Option<PullRequestKey>,
    pub loading: bool,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestDetailView {
    pub detail: PullRequestDetail,
    pub badge: PullRequestBadge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestDiffView {
    pub files: Vec<PullRequestDiffFileView>,
    pub patch: String,
    pub truncated: bool,
    pub next_cursor: Option<String>,
    pub omitted_file_stats: Option<Vec<agent_protocol::pull_requests::PullRequestOmittedFileStat>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestDiffFileView {
    pub path: String,
    pub old_path: Option<String>,
    pub additions: u64,
    pub deletions: u64,
    pub status: String,
    pub patch: Option<String>,
    pub truncated: bool,
    pub change_type: PullRequestDiffChangeTypeInput,
    pub context: Option<PullRequestDiffFileContents>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestPanelOptions {
    pub project_id: Option<String>,
    pub query: String,
    pub include_closed: bool,
}

pub fn pull_request_list(
    snapshot: &Snapshot,
    options: &PullRequestPanelOptions,
) -> PullRequestListView {
    let project_id = options
        .project_id
        .clone()
        .or_else(|| snapshot.pull_requests.selected_project.clone())
        .or_else(|| snapshot.selected_project.clone())
        .unwrap_or_default();
    let query = options.query.trim().to_ascii_lowercase();
    let entries = snapshot
        .pull_requests
        .by_project
        .get(&project_id)
        .into_iter()
        .flatten()
        .filter(|entry| options.include_closed || entry.state == PullRequestState::Open)
        .filter(|entry| {
            query.is_empty()
                || entry.title.to_ascii_lowercase().contains(&query)
                || entry.key.number.to_string().contains(&query)
                || entry.head_branch.to_ascii_lowercase().contains(&query)
        })
        .map(entry_view)
        .collect();
    let loaded = snapshot.pull_requests.by_project.contains_key(&project_id);
    PullRequestListView {
        project_id,
        entries,
        selected: None,
        loading: snapshot.connected && !loaded,
        stale: !loaded,
    }
}

pub fn pull_request_detail(
    snapshot: &Snapshot,
    key: &PullRequestKey,
) -> Option<PullRequestDetailView> {
    snapshot
        .pull_requests
        .details
        .get(&key.canonical())
        .cloned()
        .map(|detail| PullRequestDetailView {
            badge: snapshot
                .pull_requests
                .links_by_thread
                .values()
                .flatten()
                .find(|link| link.key() == *key)
                .map(resolve_pull_request_badge)
                .unwrap_or_else(|| {
                    let link = agent_domain::PullRequestLink {
                        host: key.host.clone(),
                        repository: key.repository.clone(),
                        number: key.number,
                        url: detail.summary.url.clone(),
                        source: agent_domain::PullRequestLinkSource::Manual,
                        linked_at: detail.summary.observed_at.clone(),
                        snapshot: Some(detail.summary.clone()),
                        stack: detail.summary.stack.clone(),
                        watch: None,
                    };
                    resolve_pull_request_badge(&link)
                }),
            detail,
        })
}

pub fn pull_request_diff(snapshot: &Snapshot, key: &PullRequestKey) -> Option<PullRequestDiffView> {
    let diff = snapshot.pull_requests.diffs.get(&key.canonical())?;
    let files = diff
        .files
        .iter()
        .map(|file| {
            let old_path = file.old_path.clone().unwrap_or_else(|| file.path.clone());
            let context_key = pull_request_diff_context_key(key, &old_path, &file.path);
            let status = file.status.to_ascii_lowercase();
            let change_type = match (status.as_str(), file.patch.is_some()) {
                ("added", _) => PullRequestDiffChangeTypeInput::New,
                ("removed", _) => PullRequestDiffChangeTypeInput::Deleted,
                ("renamed", false) => PullRequestDiffChangeTypeInput::RenamePure,
                ("renamed", true) => PullRequestDiffChangeTypeInput::RenameChanged,
                _ => PullRequestDiffChangeTypeInput::Change,
            };
            PullRequestDiffFileView {
                path: file.path.clone(),
                old_path: file.old_path.clone(),
                additions: file.additions,
                deletions: file.deletions,
                status: file.status.clone(),
                patch: file.patch.clone(),
                truncated: file.truncated,
                change_type,
                context: snapshot
                    .pull_requests
                    .diff_file_contents
                    .get(&context_key)
                    .cloned(),
            }
        })
        .collect();
    Some(PullRequestDiffView {
        files,
        patch: diff.patch.clone(),
        truncated: diff.truncated,
        next_cursor: diff.next_cursor.clone(),
        omitted_file_stats: diff.omitted_file_stats.clone(),
    })
}

/// Matches the already listed project rows for the `#` composer trigger. The
/// Host owns provider search; this client side narrowing keeps mobile and
/// desktop selection identical and bounded.
pub fn composer_pull_request_matches(
    snapshot: &Snapshot,
    project_id: &str,
    query: &str,
) -> Vec<PullRequestSearchMatch> {
    let query = query.trim().to_ascii_lowercase();
    let mut matches = snapshot
        .pull_requests
        .by_project
        .get(project_id)
        .into_iter()
        .flatten()
        .filter(|entry| {
            query.is_empty()
                || entry.key.number.to_string().contains(&query)
                || format!("#{}", entry.key.number).contains(&query)
                || entry.title.to_ascii_lowercase().contains(&query)
                || entry.head_branch.to_ascii_lowercase().contains(&query)
                || entry.base_branch.to_ascii_lowercase().contains(&query)
        })
        .map(|entry| PullRequestSearchMatch {
            key: entry.key.clone(),
            project: Some(project_id.to_owned()),
            title: entry.title.clone(),
            url: entry.url.clone(),
            state: entry.state,
            is_draft: entry.is_draft,
            head_branch: entry.head_branch.clone(),
            base_branch: entry.base_branch.clone(),
            updated_at: entry.updated_at.clone(),
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        let left_exact = left.key.number.to_string() == query;
        let right_exact = right.key.number.to_string() == query;
        right_exact
            .cmp(&left_exact)
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.key.canonical().cmp(&right.key.canonical()))
    });
    matches.truncate(20);
    matches
}

fn link_for_summary(entry: &PullRequestSummary) -> PullRequestLink {
    PullRequestLink {
        host: entry.key.host.clone(),
        repository: entry.key.repository.clone(),
        number: entry.key.number,
        url: entry.url.clone(),
        source: PullRequestLinkSource::Manual,
        linked_at: entry.observed_at.clone(),
        snapshot: Some(entry.clone()),
        stack: entry.stack.clone(),
        watch: None,
    }
}

fn entry_view(entry: &PullRequestSummary) -> PullRequestEntryView {
    let link = link_for_summary(entry);
    PullRequestEntryView {
        key: entry.key.clone(),
        title: entry.title.clone(),
        url: entry.url.clone(),
        state: entry.state,
        is_draft: entry.is_draft,
        head_branch: entry.head_branch.clone(),
        base_branch: entry.base_branch.clone(),
        updated_at: entry.updated_at.clone(),
        badge: resolve_pull_request_badge(&link),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_view_filters_closed_requests_until_requested() {
        let snapshot = Snapshot::default();
        let view = pull_request_list(&snapshot, &PullRequestPanelOptions::default());
        assert!(view.entries.is_empty());
    }

    #[test]
    fn diff_view_resolves_context_and_change_kind_in_core() {
        let key = PullRequestKey::new("github.com", "owner/repository", 7);
        let mut snapshot = Snapshot::default();
        snapshot.pull_requests.diffs.insert(
            key.canonical(),
            agent_protocol::pull_requests::PullRequestDiff {
                reference: agent_protocol::pull_requests::PullRequestRef {
                    project_id: "project".into(),
                    repository: key.repository.clone(),
                    number: key.number,
                    host: Some(key.host.clone()),
                    allow_stale: false,
                },
                files: vec![agent_protocol::pull_requests::PullRequestDiffFile {
                    path: "src/new.rs".into(),
                    old_path: Some("src/old.rs".into()),
                    additions: 1,
                    deletions: 1,
                    status: "renamed".into(),
                    patch: Some("@@".into()),
                    truncated: false,
                }],
                patch: "diff".into(),
                truncated: false,
                next_cursor: Some("2".into()),
                omitted_file_stats: None,
                stale: false,
            },
        );
        snapshot.pull_requests.diff_file_contents.insert(
            pull_request_diff_context_key(&key, "src/old.rs", "src/new.rs"),
            PullRequestDiffFileContents {
                old_contents: "old".into(),
                new_contents: "new".into(),
            },
        );
        let view = pull_request_diff(&snapshot, &key).unwrap();
        assert_eq!(view.next_cursor.as_deref(), Some("2"));
        assert_eq!(
            view.files[0].change_type,
            PullRequestDiffChangeTypeInput::RenameChanged
        );
        assert_eq!(view.files[0].context.as_ref().unwrap().new_contents, "new");
    }
}
