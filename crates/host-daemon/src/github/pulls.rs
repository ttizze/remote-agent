use super::cli::{
    Budget, ChangeRequestState, GhError, GitHubCli, HEAD_BRANCH_PROBE_LIMIT,
    PullRequestListState as CliPullRequestListState, PullRequestRecord, decode_pull_request,
    decode_pull_request_entries, run_bounded_command, scoped_repository,
};
use agent_domain::{
    PullRequestAction, PullRequestActor, PullRequestChecksState, PullRequestComment,
    PullRequestDetail, PullRequestKey, PullRequestLink, PullRequestLinkSource,
    PullRequestMergeability, PullRequestReviewDecision, PullRequestReviewThread, PullRequestState,
    PullRequestSummary, Timestamp,
};
use agent_protocol::pull_requests::{
    CloneRepository, GetPullRequestDiff, GetPullRequestDiffFileContents, ListPullRequests,
    PullRequestDiff, PullRequestDiffChangeType, PullRequestDiffFile, PullRequestDiffFileContents,
    PullRequestFile, PullRequestList, PullRequestListState, PullRequestMergeMethod, PullRequestRef,
    PullRequestReviewVerdict, PullRequestStackHead, SourceControlAuth, SourceControlDiscovery,
    SourceControlRepository,
};
use agent_runtime::PullRequestStore;
use base64::Engine as _;
use serde_json::Value;
use std::{path::Path, time::Duration};

const PR_FIELDS: &str = "number,title,url,baseRefName,headRefName,headRefOid,state,isDraft,mergedAt,closedAt,updatedAt,createdAt,author,additions,deletions,changedFiles,reviewDecision,statusCheckRollup,mergeable,body,labels,comments,reviews,reviewRequests";
const DIFF_PAGE_SIZE: usize = 100;
const MAX_DIFF_PAGE_BYTES: usize = 8 * 1024 * 1024;
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

/// The permission probe is deliberately separate from the mutation. GitHub reports
/// `viewerCanUpdateBranch` for the current revision, which is false after an earlier
/// layer moves. Checking the head repository's write permission and maintainer setting
/// before touching any layer keeps a later fork from leaving a half-rebased stack.
fn stack_permission_query(numbers: &[u64]) -> String {
    let requests = numbers
        .iter()
        .map(|number| {
            format!(
                "pr{number}:pullRequest(number:{number}){{headRepository{{viewerPermission}} maintainerCanModify}}"
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "query($owner:String!,$name:String!){{repository(owner:$owner,name:$name){{{requests}}}}}"
    )
}

fn encode_node_ids(ids: &[String]) -> String {
    serde_json::to_string(ids).unwrap_or_else(|_| "[]".into())
}

fn percent_encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    encoded
}

fn stack_rebase_query(processed: &[String]) -> String {
    let processed = if processed.is_empty() {
        String::new()
    } else {
        format!(
            "processed:nodes(ids:{}){{... on PullRequest{{headRefOid}}}} ",
            encode_node_ids(processed),
        )
    };
    format!(
        "query($owner:String!,$name:String!,$number:Int!,$sha:String!){{{processed}repository(owner:$owner,name:$name){{pullRequest(number:$number){{id headRefOid baseRef{{compare(headRef:$sha){{behindBy}}}}}}}}}}"
    )
}

fn repository_parts(repository: &str) -> Result<(&str, &str), GhError> {
    let Some((owner, name)) = repository.split_once('/') else {
        return Err(GhError::Decode(
            "GitHub returned an invalid repository reference.",
        ));
    };
    if owner.trim().is_empty()
        || name.trim().is_empty()
        || name.contains('/')
        || owner.contains('/')
    {
        return Err(GhError::Decode(
            "GitHub returned an invalid repository reference.",
        ));
    }
    Ok((owner, name))
}

fn check_stack_permissions(value: &Value, numbers: &[u64]) -> Result<(), GhError> {
    if value
        .get("errors")
        .and_then(Value::as_array)
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err(GhError::StackPermission);
    }
    let data = value
        .get("data")
        .and_then(Value::as_object)
        .ok_or(GhError::Decode(
            "GitHub returned an unreadable stack permission response.",
        ))?;
    let repository = data.get("repository").ok_or(GhError::Decode(
        "GitHub returned an unreadable stack permission response.",
    ))?;
    let Some(repository) = repository.as_object() else {
        return Err(GhError::StackPermission);
    };
    for number in numbers {
        let key = format!("pr{number}");
        let pull_request = repository.get(&key).ok_or(GhError::Decode(
            "GitHub returned an unreadable stack permission response.",
        ))?;
        let Some(pull_request) = pull_request.as_object() else {
            return Err(GhError::StackPermission);
        };
        let maintainer_can_modify = pull_request
            .get("maintainerCanModify")
            .and_then(Value::as_bool)
            .ok_or(GhError::Decode(
                "GitHub returned an unreadable stack permission response.",
            ))?;
        let Some(head_repository) = pull_request
            .get("headRepository")
            .and_then(Value::as_object)
        else {
            return Err(GhError::StackPermission);
        };
        let permission = head_repository
            .get("viewerPermission")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let can_write = matches!(
            permission.to_ascii_uppercase().as_str(),
            "ADMIN" | "MAINTAIN" | "WRITE"
        );
        if !maintainer_can_modify && !can_write {
            return Err(GhError::StackPermission);
        }
    }
    Ok(())
}

struct RebaseProbe {
    processed: Option<Vec<Option<String>>>,
    id: String,
    head_sha: String,
    behind_by: i64,
}

fn reject_graphql_errors(value: &Value, message: &'static str) -> Result<(), GhError> {
    if value
        .get("errors")
        .and_then(Value::as_array)
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err(GhError::Decode(message));
    }
    Ok(())
}

fn decode_rebase_probe(value: &Value) -> Result<RebaseProbe, GhError> {
    reject_graphql_errors(
        value,
        "GitHub returned an unreadable stack rebase response.",
    )?;
    let data = value
        .get("data")
        .and_then(Value::as_object)
        .ok_or(GhError::Decode(
            "GitHub returned an unreadable stack rebase response.",
        ))?;
    let processed = match data.get("processed") {
        None => None,
        Some(Value::Array(values)) => Some(
            values
                .iter()
                .map(|value| {
                    value
                        .as_object()
                        .and_then(|value| value.get("headRefOid"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect(),
        ),
        Some(_) => {
            return Err(GhError::Decode(
                "GitHub returned an unreadable stack rebase response.",
            ));
        }
    };
    let repository = data
        .get("repository")
        .and_then(Value::as_object)
        .ok_or(GhError::Decode(
            "GitHub returned an unreadable stack rebase response.",
        ))?;
    let pull_request = repository
        .get("pullRequest")
        .and_then(Value::as_object)
        .ok_or(GhError::Decode(
            "GitHub returned an unreadable stack rebase response.",
        ))?;
    let id = pull_request
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .ok_or(GhError::Decode(
            "GitHub returned an unreadable stack rebase response.",
        ))?;
    let head_sha = pull_request
        .get("headRefOid")
        .and_then(Value::as_str)
        .filter(|sha| !sha.trim().is_empty())
        .map(str::to_owned)
        .ok_or(GhError::Decode(
            "GitHub returned an unreadable stack rebase response.",
        ))?;
    let behind_by = pull_request
        .get("baseRef")
        .and_then(Value::as_object)
        .and_then(|base| base.get("compare"))
        .and_then(Value::as_object)
        .and_then(|compare| compare.get("behindBy"))
        .and_then(Value::as_i64)
        .ok_or(GhError::Decode(
            "GitHub returned an unreadable stack rebase response.",
        ))?;
    Ok(RebaseProbe {
        processed,
        id,
        head_sha,
        behind_by,
    })
}

fn decode_rebase_mutation(value: &Value) -> Result<String, GhError> {
    reject_graphql_errors(
        value,
        "GitHub returned an unreadable stack rebase mutation response.",
    )?;
    value
        .get("data")
        .and_then(Value::as_object)
        .and_then(|data| data.get("updatePullRequestBranch"))
        .and_then(Value::as_object)
        .and_then(|update| update.get("pullRequest"))
        .and_then(Value::as_object)
        .and_then(|pull_request| pull_request.get("headRefOid"))
        .and_then(Value::as_str)
        .filter(|sha| !sha.trim().is_empty())
        .map(str::to_owned)
        .ok_or(GhError::Decode(
            "GitHub returned an unreadable stack rebase mutation response.",
        ))
}

fn merge_status(value: &Value) -> Result<(&str, Option<&str>), GhError> {
    let merge = value
        .get("status")
        .and_then(Value::as_str)
        .zip(value.get("details").and_then(Value::as_object))
        .ok_or(GhError::Decode(
            "GitHub returned an unreadable stack merge response.",
        ))?;
    let uuid = merge
        .1
        .get("uuid")
        .and_then(Value::as_str)
        .filter(|uuid| !uuid.trim().is_empty());
    Ok((merge.0, uuid))
}

struct DecodedDiffPage {
    files: Vec<PullRequestDiffFile>,
    patch: String,
    truncated: bool,
    next_cursor: Option<String>,
    omitted_file_stats: Option<Vec<agent_protocol::pull_requests::PullRequestOmittedFileStat>>,
}

fn decode_diff_page(value: &Value, page: usize) -> Result<DecodedDiffPage, GhError> {
    let entries = value.as_array().ok_or(GhError::Decode(
        "GitHub returned invalid pull request file JSON.",
    ))?;
    let mut truncated = false;
    let mut remaining_patch_bytes = MAX_DIFF_PAGE_BYTES;
    let mut files = Vec::new();
    let mut omitted_file_stats = Vec::new();
    let mut patch_sections = Vec::new();
    for entry in entries {
        let path = entry
            .get("filename")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if path.is_empty() {
            continue;
        }
        let old_path = entry
            .get("previous_filename")
            .and_then(|value| value.as_str())
            .map(str::to_owned);
        let additions = entry
            .get("additions")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        let deletions = entry
            .get("deletions")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        let status = entry
            .get("status")
            .and_then(|value| value.as_str())
            .unwrap_or("modified")
            .trim()
            .to_ascii_lowercase();
        let mut file_truncated = false;
        let patch = entry
            .get("patch")
            .and_then(|value| value.as_str())
            .filter(|patch| !patch.is_empty())
            .map(|patch| {
                let allowed = remaining_patch_bytes;
                if patch.len() > allowed {
                    truncated = true;
                    file_truncated = true;
                    truncate_text(patch, allowed)
                } else {
                    patch.to_owned()
                }
            });
        remaining_patch_bytes =
            remaining_patch_bytes.saturating_sub(patch.as_ref().map_or(0, String::len));
        if patch.as_deref().is_none_or(|patch| patch.is_empty())
            && additions.saturating_add(deletions) > 0
        {
            truncated = true;
            omitted_file_stats.push(agent_protocol::pull_requests::PullRequestOmittedFileStat {
                path: path.to_owned(),
                additions,
                deletions,
            });
        }
        patch_sections.push(diff_file_section(
            path,
            old_path.as_deref(),
            &status,
            patch.as_deref(),
        ));
        files.push(PullRequestDiffFile {
            path: path.to_owned(),
            old_path,
            additions,
            deletions,
            status,
            patch,
            truncated: file_truncated,
        });
    }
    Ok(DecodedDiffPage {
        files,
        patch: patch_sections.join(""),
        truncated,
        next_cursor: (entries.len() == DIFF_PAGE_SIZE).then(|| (page + 1).to_string()),
        omitted_file_stats: (!omitted_file_stats.is_empty()).then_some(omitted_file_stats),
    })
}

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
        let repository_selector = repo.map(|repository| scoped_repository(Some(host), repository));
        if let Some(repository) = repository_selector.as_deref() {
            args.extend(["--repo", repository]);
        }
        args.extend(["--state", state, "--limit", &limit, "--json", PR_FIELDS]);
        if let Some(query) = request
            .query
            .as_deref()
            .filter(|query| !query.trim().is_empty())
        {
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
            .map(|row| {
                summary_from_record(
                    row,
                    project_id,
                    repo.unwrap_or_default(),
                    observed_at.clone(),
                )
            })
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
                repository,
            )
            .await?;
        let wanted_repository = repository.map(agent_domain::normalize_repository);
        let record = rows.into_iter().find(|record| {
            wanted_repository.as_deref().is_none_or(|wanted| {
                pull_request_url_parts(&record.url)
                    .1
                    .as_deref()
                    .is_some_and(|candidate| {
                        agent_domain::normalize_repository(candidate) == wanted
                    })
            })
        });
        let Some(record) = record else {
            return Ok(None);
        };
        let observed_at = now();
        let summary = summary_from_record(
            record,
            project_id,
            repository.unwrap_or_default(),
            observed_at.clone(),
        );
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
        let host = reference.host.as_deref().unwrap_or("github.com");
        let repository_selector = scoped_repository(Some(host), &repository);
        let number = reference.number.to_string();
        let value = self
            .cli()?
            .run_json(
                cwd,
                &[
                    "pr",
                    "view",
                    &number,
                    "--repo",
                    &repository_selector,
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
        summary.stack = self.stack(cwd, reference).await.unwrap_or(None);
        summary.labels = value
            .get("labels")
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
            .filter_map(|label| {
                label
                    .get("name")
                    .and_then(|name| name.as_str())
                    .map(str::to_owned)
            })
            .collect();
        let body = value
            .get("body")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_owned();
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
            .map(|(index, node)| {
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
                        let body = value
                            .get("body")
                            .and_then(|value| value.as_str())?
                            .to_owned();
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
        if request
            .commit
            .as_deref()
            .is_some_and(|commit| !is_commit_sha(commit))
        {
            return Err(GhError::Decode("GitHub returned an invalid diff commit."));
        }
        let page = request
            .cursor
            .as_deref()
            .map(parse_diff_cursor)
            .transpose()?
            .unwrap_or(1);
        let paging = format!("per_page={DIFF_PAGE_SIZE}&page={page}");
        let endpoint = match request.commit.as_deref() {
            Some(commit) => format!(
                "repos/{}/commits/{commit}?{paging}",
                request.reference.repository
            ),
            None => format!(
                "repos/{}/pulls/{}/files?{paging}",
                request.reference.repository, request.reference.number
            ),
        };
        let mut args = vec!["api", "--hostname", host, endpoint.as_str()];
        if request.commit.is_some() {
            args.extend(["--jq", ".files // []"]);
        }
        let value = self
            .cli()?
            .run_json(
                cwd,
                &args,
                Budget {
                    max_output_bytes: MAX_DIFF_PAGE_BYTES,
                    ..Budget::default()
                },
            )
            .await?;
        let decoded = decode_diff_page(&value, page)?;
        Ok(PullRequestDiff {
            reference: request.reference.clone(),
            files: decoded.files,
            patch: decoded.patch,
            truncated: decoded.truncated,
            next_cursor: decoded.next_cursor,
            omitted_file_stats: decoded.omitted_file_stats,
            stale: false,
        })
    }

    pub(crate) async fn diff_file_contents(
        &self,
        cwd: &Path,
        request: &GetPullRequestDiffFileContents,
    ) -> Result<PullRequestDiffFileContents, GhError> {
        ensure_github_reference(&request.reference)?;
        let host = request.reference.host.as_deref().unwrap_or("github.com");
        if request
            .commit
            .as_deref()
            .is_some_and(|commit| !is_commit_sha(commit))
        {
            return Err(GhError::Decode("GitHub returned an invalid diff commit."));
        }
        let cli = self.cli()?;
        let revisions_endpoint = match request.commit.as_deref() {
            Some(commit) => format!("repos/{}/commits/{commit}", request.reference.repository),
            None => format!(
                "repos/{}/pulls/{}",
                request.reference.repository, request.reference.number
            ),
        };
        let revision_query = if request.commit.is_some() {
            "[.parents[0].sha, .sha] | @tsv"
        } else {
            "[.base.sha, .head.sha] | @tsv"
        };
        let revision_output = cli
            .run(
                cwd,
                &[
                    "api",
                    "--hostname",
                    host,
                    &revisions_endpoint,
                    "--jq",
                    revision_query,
                ],
                Budget {
                    max_output_bytes: 1024,
                    ..Budget::default()
                },
            )
            .await?;
        let revisions = revision_output
            .stdout
            .trim_end()
            .split('\t')
            .collect::<Vec<_>>();
        let base_ref = revisions.first().copied().unwrap_or_default();
        let head_ref = revisions.get(1).copied().unwrap_or_default();
        let root_new_file =
            matches!(request.change_type, PullRequestDiffChangeType::New) && base_ref.is_empty();
        if revision_output.stdout_truncated
            || revision_output.stdout_invalid_utf8
            || revisions.len() != 2
            || head_ref.is_empty()
            || (!root_new_file && !is_commit_sha(base_ref))
            || !is_commit_sha(head_ref)
        {
            return Err(GhError::Decode(
                "GitHub returned no usable diff file revisions.",
            ));
        }
        let read_file = |revision: String, path: String| async move {
            let encoded_path = path
                .split('/')
                .map(percent_encode_path_segment)
                .collect::<Vec<_>>()
                .join("/");
            let endpoint = format!(
                "repos/{}/contents/{encoded_path}?ref={revision}",
                request.reference.repository
            );
            let output = cli
                .run(
                    cwd,
                    &[
                        "api",
                        "--hostname",
                        host,
                        "--header",
                        "Accept: application/vnd.github.raw+json",
                        &endpoint,
                    ],
                    Budget {
                        max_output_bytes: 1024 * 1024,
                        ..Budget::default()
                    },
                )
                .await?;
            if output.stdout_truncated || output.stdout_invalid_utf8 || output.stdout.contains('\0')
            {
                return Err(GhError::Decode(
                    "GitHub diff file contents are unavailable.",
                ));
            }
            Ok::<String, GhError>(output.stdout)
        };
        let old_contents = if matches!(request.change_type, PullRequestDiffChangeType::New) {
            String::new()
        } else {
            read_file(base_ref.to_owned(), request.old_path.clone()).await?
        };
        let new_contents = if matches!(request.change_type, PullRequestDiffChangeType::Deleted) {
            String::new()
        } else {
            read_file(head_ref.to_owned(), request.new_path.clone()).await?
        };
        Ok(PullRequestDiffFileContents {
            old_contents,
            new_contents,
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
            .run_json(
                cwd,
                &["api", "--hostname", host, &endpoint],
                Budget::default(),
            )
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
        let number = stack
            .get("number")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
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
            .unwrap_or("")
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
        let encoded_path = path
            .split('/')
            .map(percent_encode_path_segment)
            .collect::<Vec<_>>()
            .join("/");
        let endpoint = format!(
            "repos/{}/contents/{}?ref=pull/{}/head",
            reference.repository, encoded_path, reference.number
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
        let encoded = value
            .get("content")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
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
        let repository = scoped_repository(Some(host), &reference.repository);
        let number = reference.number.to_string();
        let merge_method = match merge_method.unwrap_or(PullRequestMergeMethod::Merge) {
            PullRequestMergeMethod::Merge => "--merge",
            PullRequestMergeMethod::Squash => "--squash",
            PullRequestMergeMethod::Rebase => "--rebase",
        };
        let args: Vec<&str> = match action {
            PullRequestAction::Merge => {
                vec!["pr", "merge", &number, "--repo", &repository, merge_method]
            }
            PullRequestAction::MarkReady => vec!["pr", "ready", &number, "--repo", &repository],
            PullRequestAction::MarkDraft => {
                vec!["pr", "ready", &number, "--repo", &repository, "--undo"]
            }
            PullRequestAction::Close => vec!["pr", "close", &number, "--repo", &repository],
            PullRequestAction::Reopen => vec!["pr", "reopen", &number, "--repo", &repository],
            PullRequestAction::UpdateBranch => {
                vec!["pr", "update-branch", &number, "--repo", &repository]
            }
            PullRequestAction::EnableAutoMerge => vec![
                "pr",
                "merge",
                &number,
                "--repo",
                &repository,
                "--auto",
                merge_method,
            ],
            PullRequestAction::DisableAutoMerge => vec![
                "pr",
                "merge",
                &number,
                "--repo",
                &repository,
                "--disable-auto",
            ],
            PullRequestAction::Revert => vec!["pr", "revert", &number, "--repo", &repository],
        };
        self.cli()?
            .run(cwd, &args, Budget::default())
            .await
            .map(|_| ())
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
        if !matches!(
            action,
            PullRequestAction::Merge | PullRequestAction::UpdateBranch
        ) {
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
            || (action == PullRequestAction::UpdateBranch && target_index + 1 != stack.layers.len())
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
                !expected_heads.iter().any(|expected| {
                    expected.number == layer.number && expected.head_sha.as_str() == head_sha
                })
            })
        {
            return Err(GhError::StackChanged);
        }
        if open.is_empty()
            || open
                .iter()
                .any(|layer| layer.state != PullRequestState::Open)
        {
            return Err(GhError::StackUnsupported);
        }
        let host = reference.host.as_deref().unwrap_or("github.com");
        if action == PullRequestAction::UpdateBranch {
            let (owner_name, repository_name) = repository_parts(&reference.repository)?;
            let numbers = open.iter().map(|layer| layer.number).collect::<Vec<_>>();
            let owner = format!("owner={owner_name}");
            let name = format!("name={repository_name}");
            let query = format!("query={}", stack_permission_query(&numbers));
            let permissions = self
                .cli()?
                .run_json(
                    cwd,
                    &[
                        "api",
                        "--hostname",
                        host,
                        "graphql",
                        "-f",
                        &owner,
                        "-f",
                        &name,
                        "-f",
                        &query,
                    ],
                    Budget {
                        max_output_bytes: 512 * 1024,
                        ..Budget::default()
                    },
                )
                .await?;
            check_stack_permissions(&permissions, &numbers)?;

            let mut processed = Vec::<(String, String)>::new();
            for (index, layer) in open.into_iter().enumerate() {
                let Some(head_sha) = layer.head_sha.as_deref() else {
                    return Err(GhError::StackChanged);
                };
                let result = async {
                    let number = format!("number={}", layer.number);
                    let sha = format!("sha={head_sha}");
                    let query = format!("query={}", stack_rebase_query(
                        &processed
                            .iter()
                            .map(|(id, _)| id.clone())
                            .collect::<Vec<_>>(),
                    ));
                    let response = self
                        .cli()?
                        .run_json(
                            cwd,
                            &[
                                "api",
                                "--hostname",
                                host,
                                "graphql",
                                "-f",
                                &owner,
                                "-f",
                                &name,
                                "-F",
                                &number,
                                "-f",
                                &sha,
                                "-f",
                                &query,
                            ],
                            Budget {
                                max_output_bytes: 512 * 1024,
                                ..Budget::default()
                            },
                        )
                        .await?;
                    let probe = decode_rebase_probe(&response)?;
                    for (processed_index, (_, processed_sha)) in processed.iter().enumerate() {
                        if probe
                            .processed
                            .as_ref()
                            .and_then(|heads| heads.get(processed_index))
                            .and_then(|sha| sha.as_deref())
                            != Some(processed_sha.as_str())
                        {
                            return Err(GhError::StackChanged);
                        }
                    }
                    if probe.head_sha != head_sha {
                        return Err(GhError::StackChanged);
                    }
                    if probe.behind_by == 0 {
                        processed.push((probe.id, probe.head_sha));
                        return Ok::<(), GhError>(());
                    }
                    let id = format!("id={}", probe.id);
                    let expected_sha = format!("sha={head_sha}");
                    let mutation = "query=mutation($id:ID!,$sha:GitObjectID!){updatePullRequestBranch(input:{pullRequestId:$id,expectedHeadOid:$sha,updateMethod:REBASE}){pullRequest{headRefOid}}}";
                    let updated = self
                        .cli()?
                        .run_json(
                            cwd,
                            &[
                                "api",
                                "--hostname",
                                host,
                                "graphql",
                                "-f",
                                &id,
                                "-f",
                                &expected_sha,
                                "-f",
                                mutation,
                            ],
                            Budget {
                                max_output_bytes: 256 * 1024,
                                ..Budget::default()
                            },
                        )
                        .await?;
                    let rebased_sha = decode_rebase_mutation(&updated)?;
                    processed.push((probe.id, rebased_sha));
                    Ok(())
                }
                .await;
                if let Err(error) = result {
                    return Err(match error {
                        GhError::StackChanged => GhError::StackChanged,
                        _ => GhError::StackRebaseFailed {
                            layer: layer.number,
                            completed: index,
                        },
                    });
                }
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
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5 * 60);
        let mut attempt = 0u32;
        loop {
            let (status, uuid) = merge_status(&result)?;
            match status {
                "merged" | "enqueued" => return Ok(()),
                "failed" => return Err(GhError::StackMergeRejected),
                "pending" => {}
                _ => {
                    return Err(GhError::Decode(
                        "GitHub returned an unreadable stack merge response.",
                    ));
                }
            }
            let Some(uuid) = uuid else {
                return Err(GhError::Decode(
                    "GitHub returned an unreadable stack merge response.",
                ));
            };
            if tokio::time::Instant::now() >= deadline {
                return Err(GhError::StackMergePending);
            }
            let delay = Duration::from_millis((1_000u64 << attempt.min(3)).min(10_000));
            tokio::time::sleep(delay).await;
            if tokio::time::Instant::now() >= deadline {
                return Err(GhError::StackMergePending);
            }
            attempt = attempt.saturating_add(1);
            let poll_endpoint = format!("{endpoint}/{}", percent_encode_path_segment(uuid));
            result = self
                .cli()?
                .run_json(
                    cwd,
                    &["api", "--hostname", host, &poll_endpoint],
                    Budget::default(),
                )
                .await?;
        }
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
        let repository = scoped_repository(Some(host), &reference.repository);
        let number = reference.number.to_string();
        let file =
            tempfile::NamedTempFile::new().map_err(|_| GhError::Command { exit_code: None })?;
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
                    &number,
                    "--repo",
                    &repository,
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
                message: Some(
                    "Authentication for this provider is not handled by the GitHub CLI.".into(),
                ),
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
        let (host, repository) = remote.as_deref().map(parse_remote).unwrap_or((None, None));
        let provider = host.as_deref().map(source_control_provider);
        let auth = if provider.as_deref() == Some("github") {
            Some(self.auth(cwd, host.as_deref(), fresh).await)
        } else {
            None
        };
        let repository_info = if provider.as_deref() == Some("github") {
            if let (Some(repository), Some(cli)) = (repository.as_deref(), self.cli.as_ref()) {
                let urls = cli
                    .repository_clone_urls(cwd, repository, host.as_deref())
                    .await
                    .ok();
                let default_branch = cli
                    .default_branch(cwd, repository, host.as_deref())
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
        command
            .arg("clone")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(branch) = request.branch.as_deref() {
            command.args(["--branch", branch]);
        }
        command.args([request.url.as_str(), request.destination.as_str()]);
        let (status, output) = run_bounded_command(
            command,
            Budget {
                timeout: std::time::Duration::from_secs(300),
                max_output_bytes: 1024 * 1024,
            },
        )
        .await?;
        if status.success() {
            Ok(())
        } else {
            Err(super::cli::classify_failure(&output.stderr, status.code()))
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

fn parse_diff_cursor(cursor: &str) -> Result<usize, GhError> {
    let page = cursor
        .parse::<usize>()
        .map_err(|_| GhError::Decode("GitHub returned an invalid diff cursor."))?;
    (page > 0 && page <= 9_999_999)
        .then_some(page)
        .ok_or(GhError::Decode("GitHub returned an invalid diff cursor."))
}

fn is_commit_sha(value: &str) -> bool {
    (7..=64).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn quote_git_patch_path(path: &str) -> String {
    let mut body = String::new();
    let mut quoted = false;
    for character in path.chars() {
        let escape = match character {
            '"' => Some("\\\""),
            '\\' => Some("\\\\"),
            '\u{0007}' => Some("\\a"),
            '\u{0008}' => Some("\\b"),
            '\t' => Some("\\t"),
            '\n' => Some("\\n"),
            '\u{000b}' => Some("\\v"),
            '\u{000c}' => Some("\\f"),
            '\r' => Some("\\r"),
            _ => None,
        };
        if let Some(escape) = escape {
            body.push_str(escape);
            quoted = true;
        } else if character.is_control() || character == '\u{007f}' {
            body.push('\\');
            body.push_str(&format!("{:03o}", character as u32));
            quoted = true;
        } else {
            body.push(character);
        }
    }
    if quoted {
        format!("\"{body}\"")
    } else {
        path.to_owned()
    }
}

fn diff_file_section(
    path: &str,
    old_path: Option<&str>,
    status: &str,
    patch: Option<&str>,
) -> String {
    let old_path = if status == "renamed" {
        old_path.unwrap_or(path)
    } else {
        path
    };
    let old_header = quote_git_patch_path(&format!("a/{old_path}"));
    let new_header = quote_git_patch_path(&format!("b/{path}"));
    let mut section = format!("diff --git {old_header} {new_header}\n");
    if status == "added" {
        section.push_str("new file mode 100644\n");
    } else if status == "removed" {
        section.push_str("deleted file mode 100644\n");
    } else if status == "renamed" {
        section.push_str("rename from ");
        section.push_str(&quote_git_patch_path(old_path));
        section.push_str("\nrename to ");
        section.push_str(&quote_git_patch_path(path));
        section.push('\n');
    }
    if status == "added" {
        section.push_str("--- /dev/null\n+++ ");
        section.push_str(&new_header);
        section.push('\n');
    } else if status == "removed" {
        section.push_str("--- ");
        section.push_str(&old_header);
        section.push_str("\n+++ /dev/null\n");
    } else {
        section.push_str("--- ");
        section.push_str(&old_header);
        section.push_str("\n+++ ");
        section.push_str(&new_header);
        section.push('\n');
    }
    if let Some(patch) = patch {
        section.push_str(patch.trim_end_matches('\n'));
        section.push('\n');
    }
    section
}

fn summary_from_record(
    record: PullRequestRecord,
    project_id: &str,
    repository: &str,
    observed_at: Timestamp,
) -> PullRequestSummary {
    let (url_host, url_repository) = pull_request_url_parts(&record.url);
    let host = url_host.unwrap_or_else(|| "github.com".into());
    let repository = url_repository.as_deref().unwrap_or(repository);
    let updated_at = record
        .updated_at
        .as_deref()
        .and_then(|value| Timestamp::parse(value).ok())
        .unwrap_or_else(|| observed_at.clone());
    let closed_at = record
        .closed_at
        .as_deref()
        .and_then(|value| Timestamp::parse(value).ok());
    let merged_at = record
        .merged_at
        .as_deref()
        .and_then(|value| Timestamp::parse(value).ok());
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
        author: record
            .head_repository_owner_login
            .map(|login| PullRequestActor {
                login,
                display_name: None,
                avatar_url: None,
            }),
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
    let repository = (segments.len() >= 4 && segments[2].eq_ignore_ascii_case("pull"))
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
            let body = value
                .get("body")
                .and_then(|value| value.as_str())?
                .to_owned();
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
                url: value
                    .get("url")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
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
            "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" => {
                return PullRequestChecksState::Failure;
            }
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
                    "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" => {
                        PullRequestChecksState::Failure
                    }
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
    Timestamp::from_millis(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as i64),
    )
    .expect("the system clock is within the supported timestamp range")
}

fn parse_remote(remote: &str) -> (Option<String>, Option<String>) {
    if let Some((_, rest)) = remote.split_once('@')
        && let Some((host, path)) = rest.split_once(':')
    {
        return (
            Some(host.to_ascii_lowercase()),
            normalize_repository_path(path),
        );
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
    fn diff_pages_keep_whole_files_and_issue_a_cursor_for_the_next_page() {
        let entries = (0..DIFF_PAGE_SIZE)
            .map(|index| {
                serde_json::json!({
                    "filename": format!("src/file-{index}.rs"),
                    "status": "modified",
                    "additions": 1,
                    "deletions": 1,
                    "patch": format!("@@ -1 +1 @@\n-old-{index}\n+new-{index}\n")
                })
            })
            .collect::<Vec<_>>();
        let decoded = decode_diff_page(&Value::Array(entries), 1).unwrap();
        assert_eq!(decoded.files.len(), DIFF_PAGE_SIZE);
        assert_eq!(decoded.next_cursor.as_deref(), Some("2"));
        assert!(!decoded.truncated);
        assert!(
            decoded
                .patch
                .contains("diff --git a/src/file-0.rs b/src/file-0.rs")
        );

        let omitted = decode_diff_page(
            &serde_json::json!([{
                "filename": "image.bin",
                "status": "modified",
                "additions": 2,
                "deletions": 3
            }]),
            2,
        )
        .unwrap();
        assert!(omitted.truncated);
        assert_eq!(omitted.omitted_file_stats.unwrap()[0].additions, 2);
        assert_eq!(omitted.next_cursor, None);
    }

    #[test]
    fn diff_patch_preserves_file_kinds_renames_and_header_safe_paths() {
        let decoded = decode_diff_page(
            &serde_json::json!([
                {"filename": "src/new.ts", "status": "added", "patch": "@@ -0,0 +1 @@\n+new"},
                {"filename": "src/gone.ts", "status": "removed", "patch": "@@ -1 +0,0 @@\n-old"},
                {"filename": "src/new name.ts", "previous_filename": "src/old name.ts", "status": "renamed"},
                {"filename": "src/pure.ts", "previous_filename": "src/old-pure.ts", "status": "renamed", "patch": ""},
                {"filename": "tab\tname.ts", "status": "modified", "patch": "@@ -1 +1 @@\n-old\n+new"}
            ]),
            1,
        )
        .unwrap();
        assert!(
            decoded
                .patch
                .contains("new file mode 100644\n--- /dev/null\n+++ b/src/new.ts")
        );
        assert!(
            decoded
                .patch
                .contains("deleted file mode 100644\n--- a/src/gone.ts\n+++ /dev/null")
        );
        assert!(
            decoded
                .patch
                .contains("rename from src/old name.ts\nrename to src/new name.ts")
        );
        assert!(
            decoded
                .patch
                .contains("diff --git \"a/tab\\tname.ts\" \"b/tab\\tname.ts\"")
        );
        assert_eq!(decoded.files[3].patch, None);
        assert!(!decoded.truncated);
    }

    #[test]
    fn checks_prioritize_failure_then_pending_then_success() {
        let failure = serde_json::json!([
            {"name": "unit", "conclusion": "SUCCESS"},
            {"name": "lint", "conclusion": "FAILURE"}
        ]);
        assert_eq!(
            checks_state(Some(&failure)),
            PullRequestChecksState::Failure
        );
        let pending = serde_json::json!([{"name": "unit", "status": "IN_PROGRESS"}]);
        assert_eq!(
            checks_state(Some(&pending)),
            PullRequestChecksState::Pending
        );
        let success = serde_json::json!([{"name": "unit", "conclusion": "SUCCESS"}]);
        assert_eq!(
            checks_state(Some(&success)),
            PullRequestChecksState::Success
        );
    }

    #[test]
    fn discovery_keeps_non_github_remote_identity() {
        assert_eq!(source_control_provider("github.com"), "github");
        assert_eq!(source_control_provider("gitlab.example"), "gitlab");
        assert_eq!(source_control_provider("code.example"), "unknown");
    }

    // githubStackActions.test.ts: "refuses the entire rebase before mutation when a later fork
    // denies write access" and "allows a fork that explicitly permits maintainer updates".
    #[test]
    fn stack_permission_preflight_requires_write_or_maintainer_access() {
        let denied = serde_json::json!({
            "data": {"repository": {
                "pr2": {"headRepository": {"viewerPermission": "WRITE"}, "maintainerCanModify": false},
                "pr3": {"headRepository": {"viewerPermission": "READ"}, "maintainerCanModify": false}
            }}
        });
        assert_eq!(
            check_stack_permissions(&denied, &[2, 3]),
            Err(GhError::StackPermission)
        );
        let maintainer = serde_json::json!({
            "data": {"repository": {
                "pr2": {"headRepository": {"viewerPermission": "WRITE"}, "maintainerCanModify": false},
                "pr3": {"headRepository": {"viewerPermission": "READ"}, "maintainerCanModify": true}
            }}
        });
        assert_eq!(check_stack_permissions(&maintainer, &[2, 3]), Ok(()));
    }

    // githubStackActions.test.ts: "rejects a push after preflight without rebasing the new
    // revision", "skips current layers without submitting a rebase mutation" and the two
    // processed-head race cases.
    #[test]
    fn stack_rebase_probe_detects_processed_head_changes_and_current_layers() {
        let response = serde_json::json!({
            "data": {
                "processed": [{"headRefOid": "new-parent"}],
                "repository": {"pullRequest": {
                    "id": "PR_3",
                    "headRefOid": "ccc",
                    "baseRef": {"compare": {"behindBy": 0}}
                }}
            }
        });
        let probe = decode_rebase_probe(&response).unwrap();
        assert_eq!(probe.processed, Some(vec![Some("new-parent".into())]));
        assert_eq!(probe.behind_by, 0);
        assert_eq!(probe.id, "PR_3");
        let query = stack_rebase_query(&["PR_2".into()]);
        assert!(query.contains("processed:nodes(ids:[\"PR_2\"]){... on PullRequest{headRefOid}}"));
        assert!(!query.contains("updatePullRequestBranch"));
        assert_eq!(
            decode_rebase_mutation(&serde_json::json!({
                "data": {"updatePullRequestBranch": {"pullRequest": {"headRefOid": "rebased"}}}
            }))
            .unwrap(),
            "rebased"
        );
        assert!(
            decode_rebase_mutation(&serde_json::json!({
                "errors": [{"message": "permission denied"}]
            }))
            .is_err()
        );
    }

    // githubStackActions.test.ts: merge queue acceptance, later rejection, malformed responses,
    // and the five-minute pending bound. The parser is kept pure so command cancellation still
    // drops the in-flight `gh` process without a durable success result.
    #[test]
    fn stack_merge_status_requires_details_and_preserves_pending_uuid() {
        assert_eq!(
            merge_status(&serde_json::json!({"status": "enqueued", "details": {}})).unwrap(),
            ("enqueued", None)
        );
        assert_eq!(
            merge_status(&serde_json::json!({
                "status": "pending",
                "details": {"uuid": "operation"}
            }))
            .unwrap(),
            ("pending", Some("operation"))
        );
        assert_eq!(
            merge_status(&serde_json::json!({"status": "merged"})),
            Err(GhError::Decode(
                "GitHub returned an unreadable stack merge response."
            ))
        );
        assert_eq!(
            percent_encode_path_segment("operation/one"),
            "operation%2Fone"
        );
        assert_eq!(
            percent_encode_path_segment("docs/a?b#c"),
            "docs%2Fa%3Fb%23c"
        );
    }
}
