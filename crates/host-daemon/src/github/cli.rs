//! `gh` as the Host runs it: bounded output, a timeout, and failures sorted
//! into sign-in, rate limit, not found and plain command failures without
//! retaining the tool's output, which can carry tokens.
use agent_protocol::vcs::{ChangeRequestState, RepositoryVisibility};
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_OUTPUT_BYTES: usize = 1_000_000;
const OUTPUT_TRUNCATED_MARKER: &str = "\n\n[truncated]";
/// The `gh pr list --json` fields the status and lookups read.
const PULL_REQUEST_FIELDS: &str = "number,title,url,baseRefName,headRefName,state,isDraft,mergedAt,closedAt,updatedAt,isCrossRepository,headRepository,headRepositoryOwner";
/// GitHub prices a page of a hundred like a page of one, and a bare branch
/// name also lists same-named branches of forks, so a probe asks for a full
/// page and lets the caller pick the right head.
pub(crate) const HEAD_BRANCH_PROBE_LIMIT: u32 = 100;

/// How long a command may run and how much of its output is kept.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Budget {
    pub timeout: Duration,
    pub max_output_bytes: usize,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }
}

/// Why a `gh` command failed, with the fixed text the user reads.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum GhError {
    #[error("GitHub CLI (`gh`) is required but not available on PATH.")]
    Unavailable,
    #[error("GitHub CLI is not authenticated. Run `gh auth login` and retry.")]
    Authentication,
    #[error(
        "GitHub API rate limit exceeded. Run `gh api rate_limit` to inspect the quota and reset time."
    )]
    RateLimited,
    #[error("Pull request not found. Check the PR number or URL and try again.")]
    NotFound,
    #[error("GitHub CLI command failed.")]
    Command { exit_code: Option<i32> },
    #[error("GitHub CLI timed out after {0:?}.")]
    Timeout(Duration),
    #[error("{0}")]
    Decode(&'static str),
}

impl GhError {
    /// A lookup that found nothing, which the status reads as "no pull
    /// request" rather than a failure.
    pub(crate) fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound)
    }
}

/// `gh pr list --json` and `gh pr view --json` rows, normalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PullRequestRecord {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub base_ref_name: String,
    pub head_ref_name: String,
    pub state: ChangeRequestState,
    pub is_draft: bool,
    pub closed_at: Option<String>,
    pub merged_at: Option<String>,
    /// ISO 8601, when the field was asked for.
    pub updated_at: Option<String>,
    pub is_cross_repository: Option<bool>,
    pub head_repository_name_with_owner: Option<String>,
    pub head_repository_owner_login: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPullRequest {
    number: u64,
    title: String,
    url: String,
    base_ref_name: String,
    head_ref_name: String,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    is_draft: Option<bool>,
    #[serde(default)]
    closed_at: Option<String>,
    #[serde(default)]
    merged_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    is_cross_repository: Option<bool>,
    // Older gh exports headRepository as {id, name} only; both stay optional
    // so a version-drifted gh can never fail the decode.
    #[serde(default)]
    head_repository: Option<RawRepository>,
    #[serde(default)]
    head_repository_owner: Option<RawOwner>,
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawRepository {
    #[serde(default)]
    name_with_owner: Option<String>,
    #[serde(default)]
    name: Option<String>,
}
#[derive(Deserialize, Default)]
struct RawOwner {
    #[serde(default)]
    login: Option<String>,
}

fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// A non-empty title, URL and ref names make a row; `None` otherwise.
fn normalize(raw: RawPullRequest) -> Option<PullRequestRecord> {
    let title = trimmed(Some(&raw.title))?;
    let url = trimmed(Some(&raw.url))?;
    let base_ref_name = trimmed(Some(&raw.base_ref_name))?;
    let head_ref_name = trimmed(Some(&raw.head_ref_name))?;
    if raw.number == 0 {
        return None;
    }
    let merged = raw
        .merged_at
        .as_deref()
        .is_some_and(|at| !at.trim().is_empty());
    let state = match raw
        .state
        .as_deref()
        .map(|state| state.trim().to_uppercase())
    {
        _ if merged => ChangeRequestState::Merged,
        Some(state) if state == "MERGED" => ChangeRequestState::Merged,
        Some(state) if state == "CLOSED" => ChangeRequestState::Closed,
        _ => ChangeRequestState::Open,
    };
    let explicit_name_with_owner = raw
        .head_repository
        .as_ref()
        .and_then(|repository| trimmed(repository.name_with_owner.as_deref()));
    let repository_name = raw
        .head_repository
        .as_ref()
        .and_then(|repository| trimmed(repository.name.as_deref()));
    let owner_login = raw
        .head_repository_owner
        .as_ref()
        .and_then(|owner| trimmed(owner.login.as_deref()))
        .or_else(|| {
            explicit_name_with_owner
                .as_deref()
                .filter(|name| name.contains('/'))
                .and_then(|name| name.split('/').next())
                .map(str::to_owned)
        });
    let name_with_owner = explicit_name_with_owner.or_else(|| {
        Some(format!(
            "{}/{}",
            owner_login.as_deref()?,
            repository_name.as_deref()?
        ))
    });
    Some(PullRequestRecord {
        number: raw.number,
        title,
        url,
        base_ref_name,
        head_ref_name,
        state,
        is_draft: raw.is_draft == Some(true),
        closed_at: raw.closed_at,
        merged_at: raw.merged_at,
        updated_at: raw.updated_at,
        is_cross_repository: raw.is_cross_repository,
        head_repository_name_with_owner: name_with_owner,
        head_repository_owner_login: owner_login,
    })
}

/// Rows of a `gh --json` list; a row that does not decode is skipped, so one
/// malformed pull request cannot hide the rest.
pub(crate) fn decode_pull_request_entries(entries: &[Value]) -> Vec<PullRequestRecord> {
    entries
        .iter()
        .filter_map(|entry| serde_json::from_value::<RawPullRequest>(entry.clone()).ok())
        .filter_map(normalize)
        .collect()
}

pub(crate) fn decode_pull_request_list(raw: &str) -> Result<Vec<PullRequestRecord>, GhError> {
    if raw.trim().is_empty() {
        return Ok(vec![]);
    }
    let entries: Vec<Value> = serde_json::from_str(raw)
        .map_err(|_| GhError::Decode("GitHub CLI returned invalid change request JSON."))?;
    Ok(decode_pull_request_entries(&entries))
}

pub(crate) fn decode_pull_request(raw: &str) -> Result<PullRequestRecord, GhError> {
    let decode = || GhError::Decode("GitHub CLI returned invalid pull request JSON.");
    let raw: RawPullRequest = serde_json::from_str(raw).map_err(|_| decode())?;
    normalize(raw).ok_or_else(decode)
}

/// The sign-in state `gh auth status --json hosts` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitHubAuth {
    pub status: AuthStatus,
    pub account: Option<String>,
    pub host: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthStatus {
    Authenticated,
    Unauthenticated,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthAccount {
    pub host: String,
    pub account: String,
    pub authenticated: bool,
    pub active: bool,
    pub error: Option<String>,
}

#[derive(Deserialize)]
struct RawAuthStatus {
    hosts: std::collections::BTreeMap<String, Vec<RawAuthAccount>>,
}
#[derive(Deserialize)]
struct RawAuthAccount {
    state: String,
    #[serde(default)]
    error: Option<String>,
    active: bool,
    host: String,
    login: String,
}

/// `None` when the text is not the JSON `gh auth status --json hosts` prints.
pub(crate) fn parse_auth_status(text: &str) -> Option<Vec<AuthAccount>> {
    let status: RawAuthStatus = serde_json::from_str(text).ok()?;
    Some(
        status
            .hosts
            .into_values()
            .flatten()
            .filter_map(|account| {
                let host = trimmed(Some(&account.host))?;
                let login = trimmed(Some(&account.login))?;
                Some(AuthAccount {
                    host: host.to_lowercase(),
                    account: login,
                    authenticated: account.state == "success",
                    active: account.active,
                    error: account
                        .error
                        .as_deref()
                        .and_then(|error| trimmed(Some(error))),
                })
            })
            .collect(),
    )
}

/// The active signed-in account, else any signed-in one.
pub(crate) fn authenticated_account(accounts: &[AuthAccount]) -> Option<&AuthAccount> {
    accounts
        .iter()
        .find(|account| account.authenticated && account.active)
        .or_else(|| accounts.iter().find(|account| account.authenticated))
}

/// `text` without terminal control sequences (CSI and OSC) and control
/// characters.
fn strip_terminal_controls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if !c.is_control() {
                out.push(c);
            }
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                while let Some(next) = chars.next() {
                    if next == '\u{7}' || (next == '\u{1b}' && chars.peek() == Some(&'\\')) {
                        chars.next_if_eq(&'\\');
                        break;
                    }
                }
            }
            _ => {
                chars.next();
            }
        }
    }
    out
}

/// The first non-empty line of the output with terminal controls removed.
fn first_safe_line(text: &str) -> Option<String> {
    strip_terminal_controls(text)
        .lines()
        .map(|line| line.trim().to_owned())
        .find(|line| !line.is_empty())
}

/// The sign-in state from the probe's output, like GitHubSourceControlProvider's
/// parseGitHubAuth.
pub(crate) fn auth_from_probe(stdout: &str, stderr: &str, exit_code: Option<i32>) -> GitHubAuth {
    let output = format!("{stdout}\n{stderr}");
    let parsed = parse_auth_status(stdout);
    let accounts = parsed.clone().unwrap_or_default();
    if let Some(account) = authenticated_account(&accounts) {
        return GitHubAuth {
            status: AuthStatus::Authenticated,
            account: Some(account.account.clone()),
            host: Some(account.host.clone()),
            detail: None,
        };
    }
    let failed = accounts
        .iter()
        .find(|account| account.active)
        .or(accounts.first());
    if parsed.is_some() {
        return GitHubAuth {
            status: AuthStatus::Unauthenticated,
            account: None,
            host: failed.map(|account| account.host.clone()),
            detail: Some(
                failed
                    .and_then(|account| account.error.clone())
                    .unwrap_or_else(|| {
                        "Run `gh auth login` to authenticate GitHub CLI with an active account."
                            .into()
                    }),
            ),
        };
    }
    // gh gained `auth status --json` in 2.81.0. Older versions reject the flag
    // and exit non-zero, which reads exactly like a signed-out CLI.
    if exit_code != Some(0) && output.contains("unknown flag: --json") {
        return GitHubAuth {
            status: AuthStatus::Unknown,
            account: None,
            host: None,
            detail: Some(
                "GitHub CLI is too old to report sign-in status. Update `gh` to 2.81.0 or newer (for example `brew upgrade gh`) and rescan."
                    .into(),
            ),
        };
    }
    if exit_code != Some(0) {
        return GitHubAuth {
            status: AuthStatus::Unauthenticated,
            account: None,
            host: None,
            detail: Some(
                first_safe_line(&output)
                    .unwrap_or_else(|| "Run `gh auth login` to authenticate GitHub CLI.".into()),
            ),
        };
    }
    GitHubAuth {
        status: AuthStatus::Unknown,
        account: None,
        host: None,
        detail: Some(
            first_safe_line(&output)
                .unwrap_or_else(|| "GitHub CLI auth status could not be parsed.".into()),
        ),
    }
}

/// What a non-zero exit of `gh` means, from its stderr (VcsProcess's
/// classifyNonZeroExit for `gh`).
pub(crate) fn classify_failure(stderr: &str, exit_code: Option<i32>) -> GhError {
    let normalized = stderr.to_lowercase();
    if [
        "authentication failed",
        "not logged in",
        "gh auth login",
        "no oauth token",
        "unauthorized",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
    {
        return GhError::Authentication;
    }
    if [
        "api rate limit",
        "rate limit exceeded",
        "secondary rate limit",
        "too many requests",
        "http 429",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
    {
        return GhError::RateLimited;
    }
    if [
        "could not resolve to a pullrequest",
        "repository.pullrequest",
        "no pull requests found for branch",
        "pull request not found",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
    {
        return GhError::NotFound;
    }
    GhError::Command { exit_code }
}

/// What a command printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Output {
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
}

/// The URLs of a repository on GitHub.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CloneUrls {
    pub name_with_owner: String,
    pub url: String,
    pub ssh_url: String,
}

/// `gh repo create` prints the new repository's URL; reading it avoids a
/// follow-up `gh repo view`, which can race GitHub's eventual consistency.
pub(crate) fn clone_urls_from_create_output(stdout: &str, repository: &str) -> CloneUrls {
    let fallback = || CloneUrls {
        name_with_owner: repository.to_owned(),
        url: format!("https://github.com/{repository}"),
        ssh_url: format!("git@github.com:{repository}.git"),
    };
    let Some(start) = stdout.find("https://").or_else(|| stdout.find("http://")) else {
        return fallback();
    };
    let printed: String = stdout[start..]
        .chars()
        .take_while(|c| !c.is_whitespace())
        .collect();
    let cleaned = printed.strip_suffix(".git").unwrap_or(&printed);
    let Ok(url) = url::Url::parse(cleaned) else {
        return fallback();
    };
    let segments: Vec<&str> = url
        .path()
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let (Some(host), [owner, name]) = (url.host_str(), segments.as_slice()) else {
        return fallback();
    };
    let name_with_owner = format!("{owner}/{name}");
    let host = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };
    CloneUrls {
        url: format!("{}://{host}/{name_with_owner}", url.scheme()),
        ssh_url: format!("git@{host}:{name_with_owner}.git"),
        name_with_owner,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PullRequestListState {
    Open,
    Closed,
    Merged,
    All,
}
impl PullRequestListState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Merged => "merged",
            Self::All => "all",
        }
    }
}

/// The `gh` executable.
#[derive(Debug, Clone)]
pub(crate) struct GitHubCli {
    program: PathBuf,
}

impl GitHubCli {
    /// `gh` on the Host's `PATH`, or `None` when it is not installed.
    pub(crate) fn locate() -> Option<Self> {
        let path = std::env::var_os("PATH")?;
        let names: &[&str] = if cfg!(windows) {
            &["gh.exe", "gh.cmd", "gh"]
        } else {
            &["gh"]
        };
        std::env::split_paths(&path)
            .flat_map(|directory| names.iter().map(move |name| directory.join(name)))
            .find(|candidate| candidate.is_file())
            .map(Self::at)
    }

    pub(crate) fn at(program: PathBuf) -> Self {
        Self { program }
    }

    /// Runs `gh` in `cwd`; a non-zero exit is classified from stderr.
    pub(crate) async fn run(
        &self,
        cwd: &Path,
        args: &[&str],
        budget: Budget,
    ) -> Result<Output, GhError> {
        let mut command = tokio::process::Command::new(&self.program);
        command
            .args(args)
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if cwd.is_dir() {
            command.current_dir(cwd);
        }
        let output = match tokio::time::timeout(budget.timeout, command.output()).await {
            Err(_) => return Err(GhError::Timeout(budget.timeout)),
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(GhError::Unavailable);
            }
            Ok(Err(_)) => return Err(GhError::Command { exit_code: None }),
            Ok(Ok(output)) => output,
        };
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !output.status.success() {
            return Err(classify_failure(&stderr, output.status.code()));
        }
        let truncated = output.stdout.len() > budget.max_output_bytes;
        let mut stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if truncated {
            let mut end = budget.max_output_bytes;
            while end > 0 && !stdout.is_char_boundary(end) {
                end -= 1;
            }
            stdout.truncate(end);
            stdout.push_str(OUTPUT_TRUNCATED_MARKER);
        }
        Ok(Output {
            stdout,
            stderr,
            stdout_truncated: truncated,
        })
    }

    /// Runs `gh` and parses its stdout as JSON.
    pub(crate) async fn run_json(
        &self,
        cwd: &Path,
        args: &[&str],
        budget: Budget,
    ) -> Result<Value, GhError> {
        let output = self.run(cwd, args, budget).await?;
        if output.stdout_truncated {
            return Err(GhError::Decode("GitHub CLI output was too large to read."));
        }
        serde_json::from_str(&output.stdout)
            .map_err(|_| GhError::Decode("GitHub CLI returned invalid JSON."))
    }

    /// `gh auth status --json hosts`.
    pub(crate) async fn auth_status(&self, cwd: &Path) -> GitHubAuth {
        let mut command = tokio::process::Command::new(&self.program);
        command
            .args(["auth", "status", "--json", "hosts"])
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        if cwd.is_dir() {
            command.current_dir(cwd);
        }
        match tokio::time::timeout(Duration::from_secs(5), command.output()).await {
            Ok(Ok(output)) => auth_from_probe(
                &String::from_utf8_lossy(&output.stdout),
                &String::from_utf8_lossy(&output.stderr),
                output.status.code(),
            ),
            _ => GitHubAuth {
                status: AuthStatus::Unknown,
                account: None,
                host: None,
                detail: Some("GitHub CLI auth status could not be read.".into()),
            },
        }
    }

    /// Pull requests whose head is `head_selector` (a branch, or
    /// `owner:branch`), newest first.
    pub(crate) async fn list_pull_requests_by_head(
        &self,
        cwd: &Path,
        head_selector: &str,
        state: PullRequestListState,
        limit: u32,
    ) -> Result<Vec<PullRequestRecord>, GhError> {
        let limit = limit.clamp(1, 100).to_string();
        let output = self
            .run(
                cwd,
                &[
                    "pr",
                    "list",
                    "--head",
                    head_selector,
                    "--state",
                    state.as_str(),
                    "--limit",
                    &limit,
                    "--json",
                    PULL_REQUEST_FIELDS,
                ],
                Budget::default(),
            )
            .await?;
        decode_pull_request_list(&output.stdout)
    }

    /// `gh pr view <reference> --json`; `reference` is a number or URL.
    pub(crate) async fn pull_request(
        &self,
        cwd: &Path,
        reference: &str,
    ) -> Result<PullRequestRecord, GhError> {
        let output = self
            .run(
                cwd,
                &["pr", "view", reference, "--json", PULL_REQUEST_FIELDS],
                Budget::default(),
            )
            .await?;
        decode_pull_request(&output.stdout)
    }

    pub(crate) async fn create_pull_request(
        &self,
        cwd: &Path,
        base_branch: &str,
        head_selector: &str,
        title: &str,
        body_file: &Path,
    ) -> Result<(), GhError> {
        self.run(
            cwd,
            &[
                "pr",
                "create",
                "--base",
                base_branch,
                "--head",
                head_selector,
                "--title",
                title,
                "--body-file",
                &body_file.to_string_lossy(),
            ],
            Budget::default(),
        )
        .await
        .map(|_| ())
    }

    /// The repository's default branch as GitHub records it.
    pub(crate) async fn default_branch(&self, cwd: &Path) -> Result<Option<String>, GhError> {
        let output = self
            .run(
                cwd,
                &[
                    "repo",
                    "view",
                    "--json",
                    "defaultBranchRef",
                    "--jq",
                    ".defaultBranchRef.name",
                ],
                Budget::default(),
            )
            .await?;
        Ok(trimmed(Some(&output.stdout)))
    }

    pub(crate) async fn repository_clone_urls(
        &self,
        cwd: &Path,
        repository: &str,
    ) -> Result<CloneUrls, GhError> {
        let value = self
            .run_json(
                cwd,
                &[
                    "repo",
                    "view",
                    repository,
                    "--json",
                    "nameWithOwner,url,sshUrl",
                ],
                Budget::default(),
            )
            .await?;
        serde_json::from_value(value)
            .map_err(|_| GhError::Decode("GitHub CLI returned invalid repository JSON."))
    }

    pub(crate) async fn create_repository(
        &self,
        cwd: &Path,
        repository: &str,
        visibility: RepositoryVisibility,
    ) -> Result<CloneUrls, GhError> {
        let flag = match visibility {
            RepositoryVisibility::Private => "--private",
            RepositoryVisibility::Public => "--public",
        };
        let output = self
            .run(
                cwd,
                &["repo", "create", repository, flag],
                Budget::default(),
            )
            .await?;
        Ok(clone_urls_from_create_output(&output.stdout, repository))
    }

    pub(crate) async fn checkout_pull_request(
        &self,
        cwd: &Path,
        reference: &str,
        force: bool,
    ) -> Result<(), GhError> {
        let mut args = vec!["pr", "checkout", reference];
        if force {
            args.push("--force");
        }
        self.run(cwd, &args, Budget::default()).await.map(|_| ())
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A `gh` stand-in for tests: a script that answers from a case table
    //! and records every invocation.
    use super::GitHubCli;
    use std::path::{Path, PathBuf};

    pub(crate) struct FakeGh {
        pub directory: tempfile::TempDir,
        pub program: PathBuf,
        log: PathBuf,
    }

    impl FakeGh {
        /// `cases` are `sh` `case` patterns over `"$*"` and the body to run.
        pub(crate) fn new(cases: &[(&str, &str)]) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let log = directory.path().join("calls.log");
            let program = directory.path().join("gh");
            let mut script = String::from("#!/bin/sh\n");
            script.push_str(&format!("printf '%s\\n' \"$*\" >> '{}'\n", log.display()));
            script.push_str("case \"$*\" in\n");
            for (pattern, body) in cases {
                // Literal parts are quoted; `*` stays a wildcard.
                let pattern = format!("\"{}\"", pattern.replace('*', "\"*\""));
                script.push_str(&format!("{pattern})\n{body}\n;;\n"));
            }
            script.push_str("*)\necho \"unexpected gh $*\" >&2\nexit 1\n;;\nesac\n");
            std::fs::write(&program, script).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            Self {
                directory,
                program,
                log,
            }
        }
        pub(crate) fn cli(&self) -> GitHubCli {
            GitHubCli::at(self.program.clone())
        }
        pub(crate) fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }
        /// A `PATH` with this `gh` first, for code that locates `gh` itself.
        pub(crate) fn path_env(&self) -> std::ffi::OsString {
            let mut paths = vec![self.directory.path().to_path_buf()];
            if let Some(existing) = std::env::var_os("PATH") {
                paths.extend(std::env::split_paths(&existing));
            }
            std::env::join_paths(paths).unwrap()
        }
        pub(crate) fn directory(&self) -> &Path {
            self.directory.path()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeGh;
    use super::*;

    fn cwd() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    // gitHubPullRequests.ts: state comes from mergedAt first, then the state
    // field; the head repository is assembled from whichever fields gh sent.
    #[test]
    fn pull_request_rows_normalize_their_state_and_head_repository() {
        let rows = decode_pull_request_list(
            r#"[
              {"number": 1, "title": "Merged", "url": "https://github.com/a/b/pull/1", "baseRefName": "main", "headRefName": "f", "state": "CLOSED", "mergedAt": "2026-01-01T00:00:00Z"},
              {"number": 2, "title": "Closed", "url": "https://github.com/a/b/pull/2", "baseRefName": "main", "headRefName": "f", "state": "closed", "isDraft": true},
              {"number": 3, "title": "Fork", "url": "https://github.com/a/b/pull/3", "baseRefName": "main", "headRefName": "f", "isCrossRepository": true, "headRepository": {"name": "b"}, "headRepositoryOwner": {"login": "Other"}},
              {"number": 4, "title": "", "url": "https://github.com/a/b/pull/4", "baseRefName": "main", "headRefName": "f"},
              {"garbage": true}
            ]"#,
        )
        .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].state, ChangeRequestState::Merged);
        assert_eq!(rows[1].state, ChangeRequestState::Closed);
        assert!(rows[1].is_draft);
        assert_eq!(rows[2].state, ChangeRequestState::Open);
        assert_eq!(
            rows[2].head_repository_name_with_owner.as_deref(),
            Some("Other/b")
        );
        assert_eq!(
            rows[2].head_repository_owner_login.as_deref(),
            Some("Other")
        );
        assert_eq!(decode_pull_request_list("  ").unwrap(), vec![]);
        assert_eq!(
            decode_pull_request_list("{").unwrap_err(),
            GhError::Decode("GitHub CLI returned invalid change request JSON.")
        );
        let one = decode_pull_request(
            r#"{"number": 7, "title": "One", "url": "u", "baseRefName": "main", "headRefName": "f", "headRepository": {"nameWithOwner": "Org/Repo"}}"#,
        )
        .unwrap();
        assert_eq!(
            one.head_repository_name_with_owner.as_deref(),
            Some("Org/Repo")
        );
        assert_eq!(one.head_repository_owner_login.as_deref(), Some("Org"));
    }

    // gitHubAuthStatus.ts and GitHubSourceControlProvider.parseGitHubAuth.
    #[test]
    fn auth_status_prefers_the_active_signed_in_account_and_names_an_old_cli() {
        let signed_in = r#"{"hosts":{"github.com":[{"state":"error","error":"token expired","active":true,"host":"GitHub.com","login":"old"},{"state":"success","active":false,"host":"github.com","login":"me"}]}}"#;
        let auth = auth_from_probe(signed_in, "", Some(0));
        assert_eq!(auth.status, AuthStatus::Authenticated);
        assert_eq!(auth.account.as_deref(), Some("me"));
        assert_eq!(auth.host.as_deref(), Some("github.com"));
        let signed_out = r#"{"hosts":{"github.com":[{"state":"error","error":"token expired","active":true,"host":"github.com","login":"old"}]}}"#;
        let auth = auth_from_probe(signed_out, "", Some(1));
        assert_eq!(auth.status, AuthStatus::Unauthenticated);
        assert_eq!(auth.detail.as_deref(), Some("token expired"));
        let empty = r#"{"hosts":{}}"#;
        assert_eq!(
            auth_from_probe(empty, "", Some(1)).detail.as_deref(),
            Some("Run `gh auth login` to authenticate GitHub CLI with an active account.")
        );
        let old = auth_from_probe("", "unknown flag: --json\n", Some(1));
        assert_eq!(old.status, AuthStatus::Unknown);
        assert!(old.detail.unwrap().contains("2.81.0"));
        let plain = auth_from_probe(
            "",
            "\u{1b}[31mYou are not logged into any GitHub hosts.\u{1b}[0m\n",
            Some(1),
        );
        assert_eq!(plain.status, AuthStatus::Unauthenticated);
        assert_eq!(
            plain.detail.as_deref(),
            Some("You are not logged into any GitHub hosts.")
        );
        assert_eq!(
            auth_from_probe("something else", "", Some(0)).status,
            AuthStatus::Unknown
        );
    }

    // VcsProcess.test.ts "classifies authentication failures", "classifies API
    // rate limits", "classifies HTTP 429 responses as rate limits".
    #[test]
    fn failures_are_classified_without_retaining_stderr() {
        assert_eq!(
            classify_failure(
                "authentication failed for token super-secret-token",
                Some(1)
            ),
            GhError::Authentication
        );
        assert_eq!(
            classify_failure(
                "GraphQL: API rate limit already exceeded for user ID 51714798 and token secret-value.",
                Some(1)
            ),
            GhError::RateLimited
        );
        assert_eq!(
            classify_failure(
                "HTTP 429: Too Many Requests. request-id=secret-value",
                Some(1)
            ),
            GhError::RateLimited
        );
        assert_eq!(
            classify_failure(
                "Could not resolve to a PullRequest with the number of 9.",
                Some(1)
            ),
            GhError::NotFound
        );
        let command = classify_failure("remote rejected super-secret-token", Some(2));
        assert_eq!(command, GhError::Command { exit_code: Some(2) });
        assert!(!command.to_string().contains("secret"));
    }

    // GitHubCli.ts deriveRepositoryCloneUrlsFromCreateOutput.
    #[test]
    fn create_output_gives_the_repository_urls_or_falls_back_to_the_name() {
        assert_eq!(
            clone_urls_from_create_output(
                "✓ Created repository Acme/Tool on GitHub\nhttps://github.com/Acme/Tool.git\n",
                "acme/tool"
            ),
            CloneUrls {
                name_with_owner: "Acme/Tool".into(),
                url: "https://github.com/Acme/Tool".into(),
                ssh_url: "git@github.com:Acme/Tool.git".into(),
            }
        );
        assert_eq!(
            clone_urls_from_create_output("done\n", "acme/tool"),
            CloneUrls {
                name_with_owner: "acme/tool".into(),
                url: "https://github.com/acme/tool".into(),
                ssh_url: "git@github.com:acme/tool.git".into(),
            }
        );
    }

    #[tokio::test]
    async fn runs_the_located_tool_and_reports_a_missing_one() {
        let gh = FakeGh::new(&[(
            "pr list --head feature --state open --limit 100 --json *",
            r#"echo '[{"number": 5, "title": "Five", "url": "https://github.com/a/b/pull/5", "baseRefName": "main", "headRefName": "feature", "state": "OPEN"}]'"#,
        )]);
        let directory = cwd();
        let rows = gh
            .cli()
            .list_pull_requests_by_head(
                directory.path(),
                "feature",
                PullRequestListState::Open,
                HEAD_BRANCH_PROBE_LIMIT,
            )
            .await
            .unwrap();
        assert_eq!(rows[0].number, 5);
        assert_eq!(gh.calls().len(), 1);
        let missing = GitHubCli::at(gh.directory().join("missing-gh"));
        assert_eq!(
            missing.default_branch(directory.path()).await.unwrap_err(),
            GhError::Unavailable
        );
        let failing = FakeGh::new(&[("repo view *", "echo 'gh auth login first' >&2; exit 4")]);
        assert_eq!(
            failing
                .cli()
                .default_branch(directory.path())
                .await
                .unwrap_err(),
            GhError::Authentication
        );
    }

    #[tokio::test]
    async fn bounded_output_is_marked_truncated() {
        let gh = FakeGh::new(&[("api *", "head -c 5000 /dev/zero | tr '\\0' 'x'")]);
        let directory = cwd();
        let output = gh
            .cli()
            .run(
                directory.path(),
                &["api", "x"],
                Budget {
                    timeout: Duration::from_secs(5),
                    max_output_bytes: 128,
                },
            )
            .await
            .unwrap();
        assert!(output.stdout_truncated);
        assert!(output.stdout.ends_with(OUTPUT_TRUNCATED_MARKER));
        assert_eq!(output.stdout.len(), 128 + OUTPUT_TRUNCATED_MARKER.len());
    }
}
