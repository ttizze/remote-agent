use super::cli::{
    Budget, ChangeRequestState, GhError, GitHubCli, PullRequestRecord,
    PullRequestListState as CliPullRequestListState, HEAD_BRANCH_PROBE_LIMIT,
    decode_pull_request, decode_pull_request_entries,
};
use base64::Engine as _;
use agent_domain::{
    PullRequestAction, PullRequestActor, PullRequestChecksState, PullRequestComment, PullRequestDetail,
    PullRequestKey, PullRequestLink, PullRequestLinkSource, PullRequestMergeability,
    PullRequestReviewDecision, PullRequestReviewThread, PullRequestState,
    PullRequestSummary, Timestamp,
};
use agent_protocol::pull_requests::{
    CloneRepository, GetPullRequestDiff, ListPullRequests, PullRequestDiff, PullRequestDiffFile,
    PullRequestFile, PullRequestList, PullRequestListState, PullRequestRef,
    PullRequestMergeMethod, PullRequestReviewVerdict, PullRequestStackHead, SourceControlAuth,
    SourceControlDiscovery, SourceControlRepository,
};
use agent_runtime::PullRequestStore;
use std::{path::Path, time::Duration};

const PR_FIELDS: &str = "number,title,url,baseRefName,headRefName,headRefOid,state,isDraft,mergedAt,closedAt,updatedAt,createdAt,author,additions,deletions,changedFiles,reviewDecision,statusCheckRollup,mergeable,body,labels,comments,reviews,reviewRequests";
const REVIEW_THREADS_QUERY: &str = r#"query($owner:String!, $repo:String!, $number:Int!) {
  repository(owner:$owner, name:$repo) {
    pullRequest(number:$number) {
      reviewThreads(first:100) {
        nodes {
          id
          path
          line
          isResolved
          comments(first:100) {
            nodes {
              id
              body
              createdAt
              updatedAt
              url
              author { login name avatarUrl }
            }
          }
        }
      }
    }
  }
}"#;

#[derive(Clone)]
pub(crate) struct GitHubPullRequestService {
    pub(crate) cli: Option<GitHubCli>,
    pub(crate) links: PullRequestStore,
}

impl GitHubPullRequestService {
    pub(crate) fn new(data_path: impl AsRef<Path>) -> anyhow::Result<Self> {
        Ok(Self {
            cli: GitHubCli::locate(),
            links: PullRequestStore::open(data_path)?,
        })
    }

    fn cli(&self) -> Result<&GitHubCli, GhError> {
        self.cli.as_ref().ok_or(GhError::Unavailable)
    }

    pub(crate) async fn list(
        &self,
        cwd: &Path,
        project_id: &str,
        request: &ListPullRequests,
        repository: Option<&str>,
        host: Option<&str>,
    ) -> Result<PullRequestList, GhError> {
        let host = host.or(request.host.as_deref());
        if !supports_github_host(host) {
            return Err(GhError::UnsupportedProvider);
        }
        let cli = self.cli()?;
        let state = match request.state {
            PullRequestListState::Open => "open",
            PullRequestListState::Closed => "closed",
            // The CLI accepts open, closed and all. Merged requests are
            // returned by `all` and narrowed after decoding.
            PullRequestListState::Merged | PullRequestListState::All => "all",
        };
        let limit = request.limit.clamp(1, 100).to_string();
        let repo = repository.or(request.repository.as_deref());
        let mut args = vec!["pr", "list"];
        let host = host.unwrap_or("github.com");
        args.extend(["--hostname", host]);
        if let Some(repo) = repo {
            args.extend(["--repo", repo]);
        }
        args.extend(["--state", state, "--limit", &limit, "--json", PR_FIELDS]);
        if let Some(query) = request.query.as_deref().filter(|query| !query.trim().is_empty()) {
            args.extend(["--search", query]);
        }
        let value = cli.run_json(cwd, &args, Budget::default()).await?;
        let rows = value
            .as_array()
            .map(|rows| decode_pull_request_entries(rows))
            .unwrap_or_default();
        let observed_at = now();
        let entries = rows
            .into_iter()
            .filter(|row| match request.state {
                PullRequestListState::All => true,
                PullRequestListState::Open => matches!(row.state, ChangeRequestState::Open),
                PullRequestListState::Closed => matches!(row.state, ChangeRequestState::Closed),
                PullRequestListState::Merged => matches!(row.state, ChangeRequestState::Merged),
            })
            .map(|row| summary_from_record(row, project_id, repo.unwrap_or_default(), observed_at.clone()))
            .collect();
        Ok(PullRequestList {
            entries,
            next_cursor: None,
            observed_at,
            stale: false,
        })
    }

    /// Resolves a checked-out branch to its open GitHub request. The CLI may
    /// return same-named fork heads, so a repository supplied by discovery is
    /// used to keep the result attached to the current remote when possible.
    pub(crate) async fn branch_pull_request(
        &self,
        cwd: &Path,
        project_id: &str,
        branch: &str,
        repository: Option<&str>,
        host: Option<&str>,
    ) -> Result<Option<PullRequestLink>, GhError> {
        let branch = branch.trim();
        if branch.is_empty() {
            return Ok(None);
        }
        let rows = self
            .cli()?
            .list_pull_requests_by_head(
                cwd,
                branch,
                CliPullRequestListState::Open,
                HEAD_BRANCH_PROBE_LIMIT,
                host,
            )
            .await?;
        let wanted_repository = repository.map(agent_domain::normalize_repository);
        let record = rows.into_iter().find(|record| {
            wanted_repository.as_deref().is_none_or(|wanted| {
                pull_request_url_parts(&record.url)
                    .1
                    .as_deref()
                    .is_some_and(|candidate| agent_domain::normalize_repository(candidate) == wanted)
            })
        });
        let Some(record) = record else {
            return Ok(None);
        };
        let observed_at = now();
        let summary = summary_from_record(record, project_id, repository.unwrap_or_default(), observed_at.clone());
        Ok(Some(PullRequestLink {
            host: summary.key.host.clone(),
            repository: summary.key.repository.clone(),
            number: summary.key.number,
            url: summary.url.clone(),
            source: PullRequestLinkSource::Agent,
            linked_at: observed_at,
            snapshot: Some(summary),
            stack: None,
            watch: None,
        }))
    }

    pub(crate) async fn get(
        &self,
        cwd: &Path,
        project_id: &str,
        reference: &PullRequestRef,
    ) -> Result<PullRequestDetail, GhError> {
        ensure_github_reference(reference)?;
        let repository = reference.repository.clone();
        let reference_value = format!("{}#{}", repository, reference.number);
        let host = reference.host.as_deref().unwrap_or("github.com");
        let value = self
            .cli()?
            .run_json(
                cwd,
                &[
                    "pr",
                    "view",
                    &reference_value,
                    "--hostname",
                    host,
                    "--json",
                    PR_FIELDS,
                ],
                Budget::default(),
            )
            .await?;
        let record = decode_pull_request(&value.to_string())?;
        let observed_at = now();
        let mut summary = summary_from_record(record, project_id, &repository, observed_at.clone());
        summary.additions = value.get("additions").and_then(|value| value.as_u64());
        summary.deletions = value.get("deletions").and_then(|value| value.as_u64());
        summary.changed_files = value.get("changedFiles").and_then(|value| value.as_u64());
        summary.opened_at = value
            .get("createdAt")
            .and_then(|value| value.as_str())
            .and_then(|value| Timestamp::parse(value).ok());
        summary.author = actor(value.get("author"));
        summary.review_decision = review_decision(value.get("reviewDecision"));
        summary.checks = checks(value.get("statusCheckRollup"));
        summary.checks_state = checks_state(value.get("statusCheckRollup"));
        summary.mergeability = mergeability(value.get("mergeable"));
        summary.stack = self
            .stack(cwd, reference)
            .await
            .unwrap_or(None);
        summary.labels = value
            .get("labels")
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
            .filter_map(|label| label.get("name").and_then(|name| name.as_str()).map(str::to_owned))
            .collect();
        let body = value.get("body").and_then(|value| value.as_str()).unwrap_or_default().to_owned();
        let review_threads = self
            .review_threads(cwd, reference)
            .await
            .unwrap_or_default();
        let viewer = self.viewer(cwd, reference.host.as_deref()).await;
        Ok(PullRequestDetail {
            summary,
            body,
            comments: comments(value.get("comments"), observed_at.clone()),
            review_threads,
            reviewers: actors(value.get("reviews")),
            requested_reviewers: actors(value.get("reviewRequests")),
            viewer,
        })
    }

    async fn viewer(&self, cwd: &Path, host: Option<&str>) -> Option<PullRequestActor> {
        let host = host.unwrap_or("github.com");
        let value = self
            .cli()
            .ok()?
            .run_json(
                cwd,
                &["api", "--hostname", host, "user"],
                Budget {
                    max_output_bytes: 32 * 1024,
                    ..Budget::default()
                },
            )
            .await
            .ok()?;
        actor(Some(&value))
    }

    async fn review_threads(
        &self,
        cwd: &Path,
        reference: &PullRequestRef,
    ) -> Result<Vec<PullRequestReviewThread>, GhError> {
        let Some((owner, repository)) = reference.repository.split_once('/') else {
            return Ok(vec![]);
        };
        let query = format!("query={REVIEW_THREADS_QUERY}");
        let owner = format!("owner={owner}");
        let repository = format!("repo={repository}");
        let number = format!("number={}", reference.number);
        let host = reference.host.as_deref().unwrap_or("github.com");
        let value = self
            .cli()?
            .run_json(
                cwd,
                &[
                    "api",
                    "--hostname",
                    host,
                    "graphql",
                    "-f",
                    query.as_str(),
                    "-F",
                    owner.as_str(),
                    "-F",
                    repository.as_str(),
                    "-F",
                    number.as_str(),
                ],
                Budget {
                    max_output_bytes: 4 * 1024 * 1024,
                    ..Budget::default()
                },
            )
            .await?;
        let Some(nodes) = value
            .get("data")
            .and_then(|value| value.get("repository"))
            .and_then(|value| value.get("pullRequest"))
            .and_then(|value| value.get("reviewThreads"))
            .and_then(|value| value.get("nodes"))
            .and_then(|value| value.as_array())
        else {
            return Ok(vec![]);
        };
        let fallback = now();
        Ok(nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                let id = node
                    .get("id")
                    .and_then(|value| value.as_str())
                    .filter(|id| !id.trim().is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("review-thread-{index}"));
                let comments = node
                    .get("comments")
                    .and_then(|value| value.get("nodes"))
                    .and_then(|value| value.as_array())
                    .into_iter()
                    .flatten()
                    .enumerate()
                    .filter_map(|(comment_index, value)| {
                        let body = value.get("body").and_then(|value| value.as_str())?.to_owned();
                        let created_at = value
                            .get("createdAt")
                            .and_then(|value| value.as_str())
                            .and_then(|value| Timestamp::parse(value).ok())
                            .unwrap_or_else(|| fallback.clone());
                        Some(PullRequestComment {
                            id: value
                                .get("id")
                                .and_then(|value| value.as_str())
                                .map(str::to_owned)
                                .unwrap_or_else(|| format!("{id}-comment-{comment_index}")),
                            author: actor(value.get("author")),
                            body,
                            created_at,
                            updated_at: value
                                .get("updatedAt")
                                .and_then(|value| value.as_str())
                                .and_then(|value| Timestamp::parse(value).ok()),
                            url: value
                                .get("url")
                                .and_then(|value| value.as_str())
                                .map(str::to_owned),
                        })
                    })
                    .collect::<Vec<_>>();
                let body = comments
                    .first()
                    .map(|comment| comment.body.clone())
                    .unwrap_or_default();
                Some(PullRequestReviewThread {
                    id,
                    path: node
                        .get("path")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_owned(),
                    line: node.get("line").and_then(|value| value.as_u64()),
                    body,
                    is_resolved: node
                        .get("isResolved")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false),
                    comments,
                })
            })
            .collect())
    }

    pub(crate) async fn diff(
        &self,
        cwd: &Path,
        request: &GetPullRequestDiff,
    ) -> Result<PullRequestDiff, GhError> {
        ensure_github_reference(&request.reference)?;
        let host = request.reference.host.as_deref().unwrap_or("github.com");
        let endpoint = format!(
            "repos/{}/pulls/{}/files",
            request.reference.repository, request.reference.number
        );
        const MAX_DIFF_FRAME_BYTES: usize = 8 * 1024 * 1024;
        let max_output_bytes = (request.max_patch_bytes as usize)
            .saturating_mul(request.max_files as usize)
            .min(MAX_DIFF_FRAME_BYTES);
        let value = self
            .cli()?
            .run_json(
                cwd,
                &["api", "--hostname", host, &endpoint, "--paginate"],
                Budget {
                    max_output_bytes,
                    ..Budget::default()
                },
            )
            .await?;
        let mut truncated = false;
        let mut remaining_patch_bytes = (request.max_patch_bytes as usize).min(MAX_DIFF_FRAME_BYTES);
        let mut files = Vec::new();
        if let Some(entries) = value.as_array() {
            for entry in entries.iter().take(request.max_files as usize) {
                let path = entry.get("filename").and_then(|value| value.as_str()).unwrap_or_default();
                if path.is_empty() {
                    continue;
                }
                let mut file_truncated = false;
                let patch = entry.get("patch").and_then(|value| value.as_str()).map(|patch| {
                    let allowed = request
                        .max_patch_bytes
                        .min(u32::try_from(remaining_patch_bytes).unwrap_or(u32::MAX))
                        as usize;
                    if patch.len() > allowed {
                        truncated = true;
                        file_truncated = true;
                        truncate_text(patch, allowed)
                    } else {
                        patch.to_owned()
                    }
                });
                remaining_patch_bytes = remaining_patch_bytes.saturating_sub(
                    patch.as_ref().map_or(0, String::len),
                );
                files.push(PullRequestDiffFile {
                    path: path.to_owned(),
                    old_path: entry.get("previous_filename").and_then(|value| value.as_str()).map(str::to_owned),
                    additions: entry.get("additions").and_then(|value| value.as_u64()).unwrap_or(0),
                    deletions: entry.get("deletions").and_then(|value| value.as_u64()).unwrap_or(0),
                    status: entry.get("status").and_then(|value| value.as_str()).unwrap_or("modified").to_owned(),
                    patch,
                    truncated: file_truncated,
                });
            }
            if entries.len() > request.max_files as usize {
                truncated = true;
            }
        }
        Ok(PullRequestDiff {
            reference: request.reference.clone(),
            files,
            truncated,
            stale: false,
        })
    }

    async fn stack(
        &self,
        cwd: &Path,
        reference: &PullRequestRef,
    ) -> Result<Option<agent_domain::PullRequestStack>, GhError> {
        let endpoint = format!(
            "repos/{}/stacks?pull_request={}",
            reference.repository, reference.number
        );
        let host = reference.host.as_deref().unwrap_or("github.com");
        let value = match self
            .cli()?
            .run_json(cwd, &["api", "--hostname", host, &endpoint], Budget::default())
            .await
        {
            Ok(value) => value,
            Err(error) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error),
        };
        let Some(stack) = value
            .as_array()
            .and_then(|stacks| stacks.first())
            .or_else(|| value.is_object().then_some(&value))
        else {
            return Ok(None);
        };
        let number = stack.get("number").and_then(|value| value.as_u64()).unwrap_or(0);
        if number == 0 {
            return Ok(None);
        }
        let base = stack
            .get("baseRefName")
            .or_else(|| stack.get("base"))
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_owned();
        let layers = stack
            .get("pull_requests")
            .or_else(|| stack.get("pullRequests"))
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
            .filter_map(|layer| {
                let number = layer.get("number").and_then(|value| value.as_u64())?;
                let head_branch = layer
                    .get("headRefName")
                    .or_else(|| layer.get("head_branch"))
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_owned();
                let head_sha = layer
                    .get("headRefOid")
                    .or_else(|| layer.get("head_sha"))
                    .or_else(|| layer.get("head").and_then(|head| head.get("sha")))
                    .and_then(|value| value.as_str())
                    .map(str::to_owned);
                let state = match layer
                    .get("state")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "merged" => PullRequestState::Merged,
                    "closed" => PullRequestState::Closed,
                    "open" => PullRequestState::Open,
                    _ => PullRequestState::Unknown,
                };
                Some(agent_domain::PullRequestStackLayer {
                    number,
                    head_branch,
                    head_sha,
                    state,
                    is_draft: layer
                        .get("isDraft")
                        .or_else(|| layer.get("draft"))
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false),
                })
            })
            .collect::<Vec<_>>();
        if layers.is_empty() {
            return Ok(None);
        }
        let url = stack
            .get("html_url")
            .or_else(|| stack.get("url"))
            .and_then(|value| value.as_str())
            .unwrap_or_else(|| "")
            .to_owned();
        Ok(Some(agent_domain::PullRequestStack {
            id: number.to_string(),
            number,
            url,
            base,
            layers,
        }))
    }

    pub(crate) async fn file(
        &self,
        cwd: &Path,
        reference: &PullRequestRef,
        path: &str,
        max_bytes: usize,
    ) -> Result<PullRequestFile, GhError> {
        ensure_github_reference(reference)?;
        let host = reference.host.as_deref().unwrap_or("github.com");
        let endpoint = format!(
            "repos/{}/contents/{}?ref=pull/{}/head",
            reference.repository, path, reference.number
        );
        let value = self
            .cli()?
            .run_json(
                cwd,
                &["api", "--hostname", host, &endpoint],
                Budget {
                    max_output_bytes: max_bytes.saturating_mul(2).min(8 * 1024 * 1024),
                    ..Budget::default()
                },
            )
            .await?;
        let encoded = value.get("content").and_then(|value| value.as_str()).unwrap_or_default();
        let mut data = base64::engine::general_purpose::STANDARD
            .decode(encoded.replace('\n', ""))
            .unwrap_or_else(|_| encoded.as_bytes().to_vec());
        let truncated = data.len() > max_bytes;
        data.truncate(max_bytes);
        Ok(PullRequestFile {
            reference: reference.clone(),
            path: path.to_owned(),
            data,
            truncated,
        })
    }

    pub(crate) async fn action(
        &self,
        cwd: &Path,
        reference: &PullRequestRef,
        action: PullRequestAction,
        merge_method: Option<PullRequestMergeMethod>,
    ) -> Result<(), GhError> {
        ensure_github_reference(reference)?;
        let host = reference.host.as_deref().unwrap_or("github.com");
        let reference = format!("{}#{}", reference.repository, reference.number);
        let merge_method = match merge_method.unwrap_or(PullRequestMergeMethod::Merge) {
            PullRequestMergeMethod::Merge => "--merge",
            PullRequestMergeMethod::Squash => "--squash",
            PullRequestMergeMethod::Rebase => "--rebase",
        };
        let args: Vec<&str> = match action {
            PullRequestAction::Merge => vec!["pr", "merge", &reference, "--hostname", host, merge_method],
            PullRequestAction::MarkReady => vec!["pr", "ready", &reference, "--hostname", host],
            PullRequestAction::MarkDraft => vec!["pr", "ready", &reference, "--hostname", host, "--undo"],
            PullRequestAction::Close => vec!["pr", "close", &reference, "--hostname", host],
            PullRequestAction::Reopen => vec!["pr", "reopen", &reference, "--hostname", host],
            PullRequestAction::UpdateBranch => vec!["pr", "update-branch", &reference, "--hostname", host],
            PullRequestAction::EnableAutoMerge => vec!["pr", "merge", &reference, "--hostname", host, "--auto", merge_method],
            PullRequestAction::DisableAutoMerge => vec!["pr", "merge", &reference, "--hostname", host, "--disable-auto"],
            PullRequestAction::Revert => vec!["pr", "revert", &reference, "--hostname", host],
        };
        self.cli()?.run(cwd, &args, Budget::default()).await.map(|_| ())
    }

    pub(crate) async fn stack_action(
        &self,
        cwd: &Path,
        reference: &PullRequestRef,
        action: PullRequestAction,
        stack_number: u64,
        expected_heads: &[PullRequestStackHead],
        merge_method: Option<PullRequestMergeMethod>,
    ) -> Result<(), GhError> {
        ensure_github_reference(reference)?;
        if !matches!(action, PullRequestAction::Merge | PullRequestAction::UpdateBranch) {
            return Err(GhError::StackUnsupported);
        }
        let stack = self
            .stack(cwd, reference)
            .await?
            .ok_or(GhError::StackUnsupported)?;
        let target_index = stack
            .layers
            .iter()
            .position(|layer| layer.number == reference.number)
            .ok_or(GhError::StackChanged)?;
        if stack.number != stack_number
            || (action == PullRequestAction::UpdateBranch
                && target_index + 1 != stack.layers.len())
        {
            return Err(GhError::StackChanged);
        }
        let affected = if action == PullRequestAction::Merge {
            &stack.layers[..=target_index]
        } else {
            &stack.layers[..]
        };
        let open = affected
            .iter()
            .filter(|layer| layer.state != PullRequestState::Merged)
            .collect::<Vec<_>>();
        if action == PullRequestAction::Merge
            && affected[target_index].state != PullRequestState::Open
        {
            return Err(GhError::StackUnsupported);
        }
        if open.len() != expected_heads.len()
            || open.iter().any(|layer| {
                let Some(head_sha) = layer.head_sha.as_deref() else {
                    return true;
                };
                !expected_heads
                    .iter()
                    .any(|expected| {
                        expected.number == layer.number
                            && expected.head_sha.as_str() == head_sha
                    })
            })
        {
            return Err(GhError::StackChanged);
        }
        if open.is_empty() || open.iter().any(|layer| layer.state != PullRequestState::Open) {
            return Err(GhError::StackUnsupported);
        }
        let host = reference.host.as_deref().unwrap_or("github.com");
        if action == PullRequestAction::UpdateBranch {
            for layer in open {
                let Some(head_sha) = layer.head_sha.as_deref() else {
                    return Err(GhError::StackChanged);
                };
                let endpoint = format!(
                    "repos/{}/pulls/{}/update-branch",
                    reference.repository, layer.number
                );
                let expected = format!("expected_head_sha={head_sha}");
                self.cli()?
                    .run(
                        cwd,
                        &[
                            "api",
                            "--hostname",
                            host,
                            "--method",
                            "PUT",
                            &endpoint,
                            "-f",
                            &expected,
                            "-f",
                            "update_method=REBASE",
                        ],
                        Budget::default(),
                    )
                    .await?;
            }
            return Ok(());
        }
        if open.iter().any(|layer| layer.is_draft) {
            return Err(GhError::StackUnsupported);
        }
        let Some(target_sha) = affected[target_index].head_sha.as_deref() else {
            return Err(GhError::StackChanged);
        };
        let endpoint = format!(
            "repos/{}/pulls/{}/merge-async",
            reference.repository, reference.number
        );
        let method = match merge_method.unwrap_or(PullRequestMergeMethod::Merge) {
            PullRequestMergeMethod::Merge => "merge",
            PullRequestMergeMethod::Squash => "squash",
            PullRequestMergeMethod::Rebase => "rebase",
        };
        let merge_method = format!("merge_method={method}");
        let sha = format!("sha={target_sha}");
        let mut result = self
            .cli()?
            .run_json(
                cwd,
                &[
                    "api",
                    "--hostname",
                    host,
                    "--method",
                    "PUT",
                    &endpoint,
                    "-f",
                    &merge_method,
                    "-f",
                    "merge_action=default",
                    "-f",
                    &sha,
                ],
                Budget::default(),
            )
            .await?;
        for _ in 0..300 {
            match result.get("status").and_then(|value| value.as_str()) {
                Some("merged") | Some("enqueued") => return Ok(()),
                Some("failed") => return Err(GhError::StackMergeRejected),
                Some("pending") => {}
                _ => return Err(GhError::Decode("GitHub returned an unreadable stack merge response.")),
            }
            let Some(uuid) = result
                .get("details")
                .and_then(|details| details.get("uuid"))
                .and_then(|value| value.as_str())
            else {
                return Err(GhError::Decode("GitHub returned an unreadable stack merge response."));
            };
            tokio::time::sleep(Duration::from_secs(1)).await;
            let poll_endpoint = format!("{endpoint}/{uuid}");
            result = self
                .cli()?
                .run_json(
                    cwd,
                    &["api", "--hostname", host, &poll_endpoint],
                    Budget::default(),
                )
                .await?;
        }
        Err(GhError::StackMergePending)
    }

    pub(crate) async fn review(
        &self,
        cwd: &Path,
        reference: &PullRequestRef,
        verdict: PullRequestReviewVerdict,
        body: &str,
    ) -> Result<(), GhError> {
        ensure_github_reference(reference)?;
        let host = reference.host.as_deref().unwrap_or("github.com");
        let reference = format!("{}#{}", reference.repository, reference.number);
        let file = tempfile::NamedTempFile::new().map_err(|_| GhError::Command { exit_code: None })?;
        std::fs::write(file.path(), body).map_err(|_| GhError::Command { exit_code: None })?;
        let body_path = file.path().to_string_lossy().into_owned();
        let verdict = match verdict {
            PullRequestReviewVerdict::Approve => "--approve",
            PullRequestReviewVerdict::RequestChanges => "--request-changes",
            PullRequestReviewVerdict::Comment => "--comment",
        };
        self.cli()?
            .run(
                cwd,
                &[
                    "pr",
                    "review",
                    &reference,
                    "--hostname",
                    host,
                    verdict,
                    "--body-file",
                    &body_path,
                ],
                Budget::default(),
            )
            .await
            .map(|_| ())
    }

    pub(crate) async fn auth(
        &self,
        cwd: &Path,
        requested_host: Option<&str>,
        fresh: bool,
    ) -> SourceControlAuth {
        let host = requested_host
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .unwrap_or("github.com")
            .to_ascii_lowercase();
        if !is_github_host(&host) {
            return SourceControlAuth {
                provider: source_control_provider(&host),
                host,
                authenticated: false,
                account: None,
                message: Some("Authentication for this provider is not handled by the GitHub CLI.".into()),
                stale: !fresh,
            };
        }
        let Some(cli) = &self.cli else {
            return SourceControlAuth {
                provider: "github".into(),
                host: host.clone(),
                authenticated: false,
                account: None,
                message: Some("GitHub CLI is not available on PATH.".into()),
                stale: !fresh,
            };
        };
        let auth = cli.auth_status(cwd).await;
        SourceControlAuth {
            provider: "github".into(),
            host: auth.host.unwrap_or(host),
            authenticated: matches!(auth.status, super::cli::AuthStatus::Authenticated),
            account: auth.account,
            message: auth.detail,
            stale: !fresh,
        }
    }

    pub(crate) async fn discover(&self, cwd: &Path, fresh: bool) -> SourceControlDiscovery {
        let remote = tokio::process::Command::new("git")
            .args(["remote", "get-url", "origin"])
            .current_dir(cwd)
            .output()
            .await
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .filter(|remote| !remote.is_empty());
        let branch = tokio::process::Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(cwd)
            .output()
            .await
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .filter(|branch| !branch.is_empty());
        let (host, repository) = remote
            .as_deref()
            .map(parse_remote)
            .unwrap_or((None, None));
        let provider = host.as_deref().map(source_control_provider);
        let auth = (provider.as_deref() == Some("github"))
            .then(|| self.auth(cwd, host.as_deref(), fresh));
        let auth = match auth {
            Some(auth) => Some(auth.await),
            None => None,
        };
        let repository_info = if provider.as_deref() == Some("github") {
            if let (Some(repository), Some(cli)) = (repository.as_deref(), self.cli.as_ref()) {
                let urls = cli
                    .repository_clone_urls(cwd, repository, host.as_deref())
                    .await
                    .ok();
                let default_branch = cli
                    .default_branch(cwd, host.as_deref())
                    .await
                    .ok()
                    .flatten();
                urls.map(|urls| SourceControlRepository {
                    host: host.clone().unwrap_or_else(|| "github.com".into()),
                    repository: urls.name_with_owner,
                    clone_url: Some(urls.url),
                    ssh_url: Some(urls.ssh_url),
                    web_url: None,
                    default_branch,
                })
            } else {
                None
            }
        } else {
            None
        };
        SourceControlDiscovery {
            provider,
            host,
            repository,
            branch,
            repository_info,
            auth,
            stale: !fresh,
        }
    }

    pub(crate) async fn clone_repository(&self, request: &CloneRepository) -> Result<(), GhError> {
        let mut command = tokio::process::Command::new("git");
        command.arg("clone");
        if let Some(branch) = request.branch.as_deref() {
            command.args(["--branch", branch]);
        }
        command.args([request.url.as_str(), request.destination.as_str()]);
        let output = tokio::time::timeout(std::time::Duration::from_secs(300), command.output())
            .await
            .map_err(|_| GhError::Timeout(std::time::Duration::from_secs(300)))?
            .map_err(|_| GhError::Command { exit_code: None })?;
        if output.status.success() {
            Ok(())
        } else {
            Err(super::cli::classify_failure(&String::from_utf8_lossy(&output.stderr), output.status.code()))
        }
    }

}

fn truncate_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let end = value
        .char_indices()
        .take_while(|(index, character)| *index + character.len_utf8() <= max_bytes)
        .map(|(index, character)| index + character.len_utf8())
        .last()
        .unwrap_or(0);
    value[..end].to_owned()
}

fn summary_from_record(record: PullRequestRecord, project_id: &str, repository: &str, observed_at: Timestamp) -> PullRequestSummary {
    let (url_host, url_repository) = pull_request_url_parts(&record.url);
    let host = url_host.unwrap_or_else(|| "github.com".into());
    let repository = url_repository.as_deref().unwrap_or(repository);
    let updated_at = record.updated_at.as_deref().and_then(|value| Timestamp::parse(value).ok()).unwrap_or_else(|| observed_at.clone());
    let closed_at = record.closed_at.as_deref().and_then(|value| Timestamp::parse(value).ok());
    let merged_at = record.merged_at.as_deref().and_then(|value| Timestamp::parse(value).ok());
    let state = match record.state {
        ChangeRequestState::Open => PullRequestState::Open,
        ChangeRequestState::Closed => PullRequestState::Closed,
        ChangeRequestState::Merged => PullRequestState::Merged,
    };
    PullRequestSummary {
        key: PullRequestKey::new(host, repository, record.number),
        project: (!project_id.is_empty()).then(|| project_id.to_owned()),
        url: record.url,
        title: record.title,
        state,
        is_draft: record.is_draft,
        head_branch: record.head_ref_name,
        head_sha: record.head_sha,
        base_branch: record.base_ref_name,
        opened_at: None,
        closed_at,
        merged_at,
        updated_at,
        observed_at,
        author: record.head_repository_owner_login.map(|login| PullRequestActor { login, display_name: None, avatar_url: None }),
        additions: None,
        deletions: None,
        changed_files: None,
        review_decision: PullRequestReviewDecision::Unknown,
        checks_state: PullRequestChecksState::Unknown,
        mergeability: PullRequestMergeability::Unknown,
        checks: vec![],
        labels: vec![],
        stack: None,
    }
}

fn pull_request_url_parts(url: &str) -> (Option<String>, Option<String>) {
    let Ok(url) = url::Url::parse(url) else {
        return (None, None);
    };
    let host = url.host_str().map(str::to_ascii_lowercase);
    let segments = url
        .path_segments()
        .into_iter()
        .flatten()
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let repository = (segments.len() >= 4
        && segments[2].eq_ignore_ascii_case("pull"))
        .then(|| format!("{}/{}", segments[0], segments[1]));
    (host, repository)
}

fn actor(value: Option<&serde_json::Value>) -> Option<PullRequestActor> {
    let login = value?.get("login").and_then(|value| value.as_str())?.trim();
    (!login.is_empty()).then(|| PullRequestActor {
        login: login.to_owned(),
        display_name: value
            .and_then(|value| value.get("name"))
            .and_then(|value| value.as_str())
            .map(str::to_owned),
        avatar_url: value
            .and_then(|value| value.get("avatarUrl"))
            .and_then(|value| value.as_str())
            .map(str::to_owned),
    })
}

fn actors(value: Option<&serde_json::Value>) -> Vec<PullRequestActor> {
    value
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| actor(Some(value)))
        .collect()
}

fn comments(value: Option<&serde_json::Value>, fallback: Timestamp) -> Vec<PullRequestComment> {
    value
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, value)| {
            let body = value.get("body").and_then(|value| value.as_str())?.to_owned();
            let created_at = value
                .get("createdAt")
                .and_then(|value| value.as_str())
                .and_then(|value| Timestamp::parse(value).ok())
                .unwrap_or_else(|| fallback.clone());
            Some(PullRequestComment {
                id: value
                    .get("id")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("comment-{index}")),
                author: actor(value.get("author")),
                body,
                created_at,
                updated_at: value
                    .get("updatedAt")
                    .and_then(|value| value.as_str())
                    .and_then(|value| Timestamp::parse(value).ok()),
                url: value.get("url").and_then(|value| value.as_str()).map(str::to_owned),
            })
        })
        .collect()
}

fn review_decision(value: Option<&serde_json::Value>) -> PullRequestReviewDecision {
    match value.and_then(|value| value.as_str()).unwrap_or_default() {
        "APPROVED" => PullRequestReviewDecision::Approved,
        "CHANGES_REQUESTED" => PullRequestReviewDecision::ChangesRequested,
        "REVIEW_REQUIRED" => PullRequestReviewDecision::ReviewRequired,
        _ => PullRequestReviewDecision::Unknown,
    }
}

fn checks_state(value: Option<&serde_json::Value>) -> PullRequestChecksState {
    let Some(values) = value.and_then(|value| value.as_array()) else {
        return PullRequestChecksState::Unknown;
    };
    let mut pending = false;
    for check in values {
        let state = check
            .get("conclusion")
            .or_else(|| check.get("status"))
            .or_else(|| check.get("state"))
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_ascii_uppercase();
        match state.as_str() {
            "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" => return PullRequestChecksState::Failure,
            "PENDING" | "IN_PROGRESS" | "QUEUED" => pending = true,
            _ => {}
        }
    }
    if pending {
        PullRequestChecksState::Pending
    } else if values.is_empty() {
        PullRequestChecksState::Unknown
    } else {
        PullRequestChecksState::Success
    }
}

fn checks(value: Option<&serde_json::Value>) -> Vec<agent_domain::PullRequestCheck> {
    value
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(index, check)| {
            let name = check
                .get("name")
                .or_else(|| check.get("context"))
                .and_then(|value| value.as_str())
                .filter(|name| !name.trim().is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("check-{index}"));
            let state = check
                .get("conclusion")
                .or_else(|| check.get("status"))
                .or_else(|| check.get("state"))
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_ascii_uppercase();
            agent_domain::PullRequestCheck {
                name,
                state: match state.as_str() {
                    "SUCCESS" => PullRequestChecksState::Success,
                    "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" => PullRequestChecksState::Failure,
                    "PENDING" | "IN_PROGRESS" | "QUEUED" => PullRequestChecksState::Pending,
                    "NEUTRAL" => PullRequestChecksState::Neutral,
                    "SKIPPED" => PullRequestChecksState::Skipped,
                    _ => PullRequestChecksState::Unknown,
                },
                url: check
                    .get("detailsUrl")
                    .or_else(|| check.get("targetUrl"))
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
                started_at: check
                    .get("startedAt")
                    .and_then(|value| value.as_str())
                    .and_then(|value| Timestamp::parse(value).ok()),
                completed_at: check
                    .get("completedAt")
                    .and_then(|value| value.as_str())
                    .and_then(|value| Timestamp::parse(value).ok()),
            }
        })
        .collect()
}

fn mergeability(value: Option<&serde_json::Value>) -> PullRequestMergeability {
    match value.and_then(|value| value.as_str()).unwrap_or_default() {
        "MERGEABLE" => PullRequestMergeability::Mergeable,
        "CONFLICTING" => PullRequestMergeability::Conflicting,
        _ => PullRequestMergeability::Unknown,
    }
}

fn now() -> Timestamp {
    Timestamp::from_millis(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis() as i64)).expect("the system clock is within the supported timestamp range")
}

fn parse_remote(remote: &str) -> (Option<String>, Option<String>) {
    if let Some((_, rest)) = remote.split_once('@')
        && let Some((host, path)) = rest.split_once(':')
    {
        return (Some(host.to_ascii_lowercase()), normalize_repository_path(path));
    }
    let Ok(url) = url::Url::parse(remote) else {
        return (None, None);
    };
    (
        url.host_str().map(str::to_ascii_lowercase),
        normalize_repository_path(url.path()),
    )
}

fn normalize_repository_path(path: &str) -> Option<String> {
    let repository = path.trim().trim_matches('/').trim_end_matches(".git");
    (!repository.is_empty()).then(|| repository.to_ascii_lowercase())
}

pub(crate) fn supports_github_host(host: Option<&str>) -> bool {
    host.is_none_or(is_github_host)
}

pub(crate) fn is_github_host(host: &str) -> bool {
    let host = host.trim().to_ascii_lowercase();
    host == "github.com"
        || host.ends_with(".github.com")
        || host.split('.').any(|part| part == "github")
}

fn ensure_github_reference(reference: &PullRequestRef) -> Result<(), GhError> {
    if supports_github_host(reference.host.as_deref()) {
        Ok(())
    } else {
        Err(GhError::UnsupportedProvider)
    }
}

fn source_control_provider(host: &str) -> String {
    let host = host.trim().to_ascii_lowercase();
    if is_github_host(&host) {
        "github".into()
    } else if host == "gitlab.com" || host.contains("gitlab") {
        "gitlab".into()
    } else if host == "bitbucket.org" || host.contains("bitbucket") {
        "bitbucket".into()
    } else if host.contains("azure") || host.contains("visualstudio.com") {
        "azure_devops".into()
    } else if host.contains("forgejo") || host.contains("gitea") {
        "forgejo".into()
    } else {
        "unknown".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_truncation_keeps_utf8_boundaries() {
        assert_eq!(truncate_text("a🙂b", 2), "a");
        assert_eq!(truncate_text("a🙂b", 5), "a🙂");
        assert_eq!(truncate_text("abc", 3), "abc");
    }

    #[test]
    fn checks_prioritize_failure_then_pending_then_success() {
        let failure = serde_json::json!([
            {"name": "unit", "conclusion": "SUCCESS"},
            {"name": "lint", "conclusion": "FAILURE"}
        ]);
        assert_eq!(checks_state(Some(&failure)), PullRequestChecksState::Failure);
        let pending = serde_json::json!([{"name": "unit", "status": "IN_PROGRESS"}]);
        assert_eq!(checks_state(Some(&pending)), PullRequestChecksState::Pending);
        let success = serde_json::json!([{"name": "unit", "conclusion": "SUCCESS"}]);
        assert_eq!(checks_state(Some(&success)), PullRequestChecksState::Success);
    }

    #[test]
    fn discovery_keeps_non_github_remote_identity() {
        assert_eq!(source_control_provider("github.com"), "github");
        assert_eq!(source_control_provider("gitlab.example"), "gitlab");
        assert_eq!(source_control_provider("code.example"), "unknown");
    }
}
