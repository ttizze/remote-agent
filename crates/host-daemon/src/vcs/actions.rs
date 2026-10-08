//! The Git action owner. It performs the commands and emits the protocol's
//! progress records; clients only render those records.
use super::process::{Execute, NON_INTERACTIVE_ENV, Progress, execute};
use super::pull_requests::{branch_head_context, find_open_pr};
use super::{
    VcsStatusBroadcaster, default_branch, git, github_scope, primary_remote, remote_names,
    split_remote_ref, stdout,
};
use crate::conversation::TextGenerator;
use crate::text_generation::{
    GeneratedCommitMessage, GeneratedPrContent, commit_message_prompt, commit_message_schema,
    format_commit_message, parse_custom_commit_message, pr_content_prompt, pr_content_schema,
    pull_request_template, sanitize_commit_message, sanitize_pr_content,
};
use agent_protocol::vcs::{
    ActionPhase, ActionProgressEvent, ActionProgressKind, ActionToast, ActionToastCta, BranchStep,
    BranchStepStatus, CommitStep, CommitStepStatus, PrStep, PrStepStatus, PushStep, PushStepStatus,
    RunStackedAction, StackedAction, StackedActionResult,
};
use agent_runtime::TextGenerationRequest;
use anyhow::anyhow;
use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc::{self, Receiver, Sender};

const COMMIT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const PUSH_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const OUTPUT_LIMIT: usize = 500;

#[derive(Debug)]
struct ActionError {
    phase: Option<ActionPhase>,
    message: String,
}

impl ActionError {
    fn at(phase: ActionPhase, error: impl std::fmt::Display) -> Self {
        Self {
            phase: Some(phase),
            message: error.to_string(),
        }
    }
    fn plain(error: impl std::fmt::Display) -> Self {
        Self {
            phase: None,
            message: error.to_string(),
        }
    }
}

fn event(request: &RunStackedAction, kind: ActionProgressKind) -> ActionProgressEvent {
    ActionProgressEvent {
        action_id: request.action_id.clone(),
        cwd: request.cwd.clone(),
        action: request.action,
        kind,
    }
}

fn phases(request: &RunStackedAction) -> Vec<ActionPhase> {
    let action = request.action;
    let mut phases = Vec::new();
    if request.feature_branch {
        phases.push(ActionPhase::Branch);
    }
    if action.commits() {
        phases.push(ActionPhase::Commit);
    }
    if matches!(
        action,
        StackedAction::Push | StackedAction::CommitPush | StackedAction::CommitPushPr
    ) || (action == StackedAction::CreatePr && should_push_before_pr(Path::new(&request.cwd)))
    {
        phases.push(ActionPhase::Push);
    }
    if matches!(
        action,
        StackedAction::CreatePr | StackedAction::CommitPushPr
    ) {
        phases.push(ActionPhase::Pr);
    }
    phases
}

/// Starts an action and returns its first stream event and receiver.
pub(crate) fn start(
    request: RunStackedAction,
    github: Option<crate::github::cli::GitHubCli>,
    text: Option<TextGenerator>,
    broadcaster: VcsStatusBroadcaster,
) -> (ActionProgressEvent, Receiver<ActionProgressEvent>) {
    let (sender, receiver) = mpsc::channel(64);
    let first = event(
        &request,
        ActionProgressKind::ActionStarted {
            phases: phases(&request),
        },
    );
    let task_request = request.clone();
    tokio::spawn(async move {
        let result = run(task_request.clone(), github, text, sender.clone()).await;
        match result {
            Ok(result) => {
                let _ = sender
                    .send(event(
                        &task_request,
                        ActionProgressKind::ActionFinished { result },
                    ))
                    .await;
            }
            Err(error) => {
                let _ = sender
                    .send(event(
                        &task_request,
                        ActionProgressKind::ActionFailed {
                            phase: error.phase,
                            message: error.message,
                        },
                    ))
                    .await;
            }
        }
        // Staging, branch creation, a failed commit hook, and a failed push
        // all change what the controls can offer. The reference invalidates
        // the full status cache even when an action fails.
        broadcaster.spawn_refresh(&task_request.cwd);
    });
    (first, receiver)
}

async fn run(
    request: RunStackedAction,
    github: Option<crate::github::cli::GitHubCli>,
    text: Option<TextGenerator>,
    sender: Sender<ActionProgressEvent>,
) -> Result<StackedActionResult, ActionError> {
    let cwd = Path::new(&request.cwd);
    if !cwd.is_dir() {
        return Err(ActionError::plain("Git working directory does not exist."));
    }
    if request.feature_branch && !request.action.commits() {
        return Err(ActionError::plain(
            "Feature branch checkout is only supported for commit actions.",
        ));
    }
    if request.action == StackedAction::CreatePr
        && stdout(cwd, &["status", "--porcelain"]).is_some_and(|output| !output.is_empty())
    {
        return Err(ActionError::at(
            ActionPhase::Pr,
            "Commit local changes before creating a pull request.",
        ));
    }
    let has_staged_changes = if request.action.commits() {
        stage_selected_files(cwd, &request)
            .await
            .map_err(|error| ActionError::at(ActionPhase::Commit, error))?;
        let index = git(
            cwd,
            &["diff", "--cached", "--quiet"],
            super::Options::default(),
        )
        .map_err(|error| ActionError::at(ActionPhase::Commit, error))?;
        if request.feature_branch && index.ok() {
            return Err(ActionError::at(
                ActionPhase::Branch,
                "Cannot create a feature branch because there are no changes to commit.",
            ));
        }
        !index.ok()
    } else {
        false
    };
    let mut generated = if has_staged_changes {
        Some(
            commit_message(&request, &text)
                .await
                .map_err(|error| ActionError::at(ActionPhase::Commit, error))?,
        )
    } else {
        None
    };
    let branch = if request.action.commits() && request.feature_branch {
        phase(
            &request,
            &sender,
            ActionPhase::Branch,
            "Creating feature branch",
        )
        .await;
        let name = create_feature_branch(
            cwd,
            generated
                .as_ref()
                .map(|m| m.branch.as_str())
                .filter(|b| !b.is_empty())
                .or_else(|| generated.as_ref().map(|m| m.subject.as_str())),
        )
        .await
        .map_err(|error| ActionError::at(ActionPhase::Branch, error))?;
        BranchStep {
            status: BranchStepStatus::Created,
            name: Some(name),
        }
    } else {
        BranchStep {
            status: BranchStepStatus::SkippedNotRequested,
            name: None,
        }
    };
    let commit = if request.action.commits() {
        phase(&request, &sender, ActionPhase::Commit, "Committing changes").await;
        let message = generated.take().unwrap_or_else(|| GeneratedCommitMessage {
            subject: "Update project files".into(),
            ..Default::default()
        });
        commit(cwd, &request, &message, &sender).await?
    } else {
        CommitStep {
            status: CommitStepStatus::SkippedNotRequested,
            commit_sha: None,
            subject: None,
        }
    };
    let push_requested = matches!(
        request.action,
        StackedAction::Push | StackedAction::CommitPush | StackedAction::CommitPushPr
    ) || (request.action == StackedAction::CreatePr
        && should_push_before_pr(cwd));
    let push = if push_requested {
        phase(&request, &sender, ActionPhase::Push, "Pushing changes").await;
        push(cwd, &request, &sender).await?
    } else {
        PushStep {
            status: PushStepStatus::SkippedNotRequested,
            branch: None,
            upstream_branch: None,
            set_upstream: None,
        }
    };
    let pr = if matches!(
        request.action,
        StackedAction::CreatePr | StackedAction::CommitPushPr
    ) {
        phase(&request, &sender, ActionPhase::Pr, "Opening pull request").await;
        pull_request(cwd, &request, github.as_ref(), text.as_ref()).await?
    } else {
        PrStep {
            status: PrStepStatus::SkippedNotRequested,
            url: None,
            number: None,
            base_branch: None,
            head_branch: None,
            title: None,
        }
    };
    let toast = match pr.url.as_deref() {
        Some(url) => ActionToast {
            title: "Pull request ready".into(),
            description: pr.title.clone(),
            cta: ActionToastCta::OpenPr {
                label: "Open pull request".into(),
                url: url.into(),
            },
        },
        None => ActionToast {
            title: "Git action complete".into(),
            description: None,
            cta: ActionToastCta::None,
        },
    };
    Ok(StackedActionResult {
        action: request.action,
        branch,
        commit,
        push,
        pr,
        toast,
    })
}

fn should_push_before_pr(cwd: &Path) -> bool {
    let upstream = stdout(
        cwd,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    );
    let Some(upstream) = upstream else {
        return true;
    };
    stdout(
        cwd,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{upstream}...HEAD"),
        ],
    )
    .and_then(|counts| counts.split_whitespace().nth(1)?.parse::<u64>().ok())
    .is_some_and(|ahead| ahead > 0)
}

async fn stage_selected_files(cwd: &Path, request: &RunStackedAction) -> anyhow::Result<()> {
    let mut add_args = vec!["add".to_owned(), "--".to_owned()];
    match request
        .file_paths
        .as_ref()
        .filter(|paths| !paths.is_empty())
    {
        Some(paths) => add_args.extend(paths.iter().cloned()),
        None => add_args.push(".".into()),
    }
    let add_refs: Vec<&str> = add_args.iter().map(String::as_str).collect();
    let staged = execute(Execute {
        env: &NON_INTERACTIVE_ENV,
        timeout: Some(Duration::from_secs(60)),
        ..Execute::new(cwd, &add_refs)
    })
    .await?;
    staged
        .ok()
        .then_some(())
        .ok_or_else(|| anyhow!("Git could not stage the selected files."))
}

async fn phase(
    request: &RunStackedAction,
    sender: &Sender<ActionProgressEvent>,
    phase: ActionPhase,
    label: &str,
) {
    let _ = sender
        .send(event(
            request,
            ActionProgressKind::PhaseStarted {
                phase,
                label: label.into(),
            },
        ))
        .await;
}

async fn commit_message(
    request: &RunStackedAction,
    text: &Option<TextGenerator>,
) -> Result<GeneratedCommitMessage, String> {
    if let Some(custom) = request
        .commit_message
        .as_deref()
        .filter(|message| !message.trim().is_empty())
    {
        return Ok(sanitize_commit_message(parse_custom_commit_message(custom)));
    }
    let fallback = GeneratedCommitMessage {
        subject: "Update project files".into(),
        body: String::new(),
        branch: "update-project-files".into(),
    };
    let Some(text) = text else {
        return Ok(fallback);
    };
    let summary =
        stdout(Path::new(&request.cwd), &["diff", "--cached", "--stat"]).unwrap_or_default();
    let patch =
        stdout(Path::new(&request.cwd), &["diff", "--cached", "--no-color"]).unwrap_or_default();
    let generation = text.generation_settings(
        request.project_id.as_deref().unwrap_or_default(),
        "generateCommitMessage",
    );
    let generated = text
        .generate(TextGenerationRequest {
            operation: "git-commit-message",
            project: request.project_id.clone().unwrap_or_default(),
            cwd: request.cwd.clone(),
            prompt: commit_message_prompt(
                stdout(Path::new(&request.cwd), &["branch", "--show-current"]).as_deref(),
                &summary,
                &patch,
                request.feature_branch,
            ),
            attachments: vec![],
            model: generation.model,
            instructions: generation.instructions,
            output_schema: commit_message_schema(request.feature_branch),
        })
        .await;
    let Ok(generated) = generated else {
        return Ok(fallback);
    };
    serde_json::from_str::<GeneratedCommitMessage>(&generated)
        .map(sanitize_commit_message)
        .map_err(|_| "Text provider returned invalid commit message JSON.".into())
}

async fn create_feature_branch(cwd: &Path, fragment: Option<&str>) -> anyhow::Result<String> {
    let base = feature_branch_base(fragment);
    let mut branch = base.clone();
    for suffix in 2..102 {
        let exists = git(
            cwd,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
            super::Options::default(),
        )
        .is_ok_and(|result| result.ok());
        if !exists {
            let args = ["switch", "-c", branch.as_str()];
            let created = execute(Execute {
                env: &NON_INTERACTIVE_ENV,
                timeout: Some(Duration::from_secs(30)),
                ..Execute::new(cwd, &args)
            })
            .await?;
            if created.ok() {
                return Ok(branch);
            }
            return Err(anyhow!("Git could not create the feature branch."));
        }
        branch = format!("{base}-{suffix}");
    }
    Err(anyhow!("Could not find an available feature branch name."))
}

fn feature_branch_base(fragment: Option<&str>) -> String {
    let fragment = fragment.unwrap_or("update-project-files");
    let branch_fragment = agent_runtime::sanitize_branch_fragment(fragment);
    if branch_fragment.starts_with("feature/") {
        branch_fragment
    } else {
        format!("feature/{branch_fragment}")
    }
}

async fn commit(
    cwd: &Path,
    request: &RunStackedAction,
    message: &GeneratedCommitMessage,
    sender: &Sender<ActionProgressEvent>,
) -> Result<CommitStep, ActionError> {
    let index = git(
        cwd,
        &["diff", "--cached", "--quiet"],
        super::Options::default(),
    )
    .map_err(|error| ActionError::at(ActionPhase::Commit, error))?;
    if index.ok() {
        return Ok(CommitStep {
            status: CommitStepStatus::SkippedNoChanges,
            commit_sha: None,
            subject: None,
        });
    }
    let message = format_commit_message(message);
    let args = vec!["commit".to_owned(), "-m".to_owned(), message.clone()];
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut report = |progress: Progress| {
        let kind = match progress {
            Progress::Output { stream, line } => ActionProgressKind::HookOutput {
                hook_name: None,
                stream,
                text: line.chars().take(OUTPUT_LIMIT).collect(),
            },
            Progress::HookStarted { hook_name } => ActionProgressKind::HookStarted { hook_name },
            Progress::HookFinished {
                hook_name,
                exit_code,
                duration_ms,
            } => ActionProgressKind::HookFinished {
                hook_name,
                exit_code,
                duration_ms,
            },
        };
        let _ = sender.try_send(event(request, kind));
    };
    let committed = execute(Execute {
        env: &NON_INTERACTIVE_ENV,
        timeout: Some(COMMIT_TIMEOUT),
        progress: Some(&mut report),
        trace_hooks: true,
        ..Execute::new(cwd, &refs)
    })
    .await
    .map_err(|error| ActionError::at(ActionPhase::Commit, error))?;
    if !committed.ok() {
        return Err(ActionError::at(ActionPhase::Commit, "Git commit failed."));
    }
    Ok(CommitStep {
        status: CommitStepStatus::Created,
        commit_sha: stdout(cwd, &["rev-parse", "HEAD"]),
        subject: Some(message.lines().next().unwrap_or_default().to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::feature_branch_base;

    #[test]
    fn feature_branch_base_uses_a_stable_fallback() {
        assert_eq!(feature_branch_base(None), "feature/update-project-files");
        assert_eq!(feature_branch_base(Some("---")), "feature/update");
    }

    #[test]
    fn feature_branch_base_preserves_nested_namespaces() {
        assert_eq!(
            feature_branch_base(Some("Feature/Branch Naming")),
            "feature/branch-naming"
        );
        assert_eq!(
            feature_branch_base(Some("branch / naming")),
            "feature/branch-/-naming"
        );
    }
}

async fn push(
    cwd: &Path,
    request: &RunStackedAction,
    sender: &Sender<ActionProgressEvent>,
) -> Result<PushStep, ActionError> {
    let branch = stdout(cwd, &["branch", "--show-current"])
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| ActionError::at(ActionPhase::Push, "Cannot push a detached HEAD."))?;
    let upstream = stdout(
        cwd,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    );
    let upstream_branch = upstream
        .as_deref()
        .and_then(|value| split_remote_ref(value, &remote_names(cwd)))
        .map(|(_, branch)| branch)
        .or_else(|| upstream.clone());
    if upstream.is_some() {
        let counts = stdout(
            cwd,
            &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
        )
        .unwrap_or_default();
        let mut numbers = counts
            .split_whitespace()
            .filter_map(|part| part.parse::<u64>().ok());
        let _behind = numbers.next().unwrap_or(0);
        let ahead = numbers.next().unwrap_or(1);
        if ahead == 0 {
            return Ok(PushStep {
                status: PushStepStatus::SkippedUpToDate,
                branch: Some(branch),
                upstream_branch,
                set_upstream: Some(false),
            });
        }
    }
    let mut args = vec!["push".to_owned()];
    if upstream.is_none() {
        let remote = primary_remote(cwd)
            .ok_or_else(|| ActionError::at(ActionPhase::Push, "No Git remote is configured."))?;
        args.extend(["--set-upstream".into(), remote, branch.clone()]);
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut report = |progress: Progress| {
        if let Progress::Output { stream, line } = progress {
            let _ = sender.try_send(event(
                request,
                ActionProgressKind::HookOutput {
                    hook_name: None,
                    stream,
                    text: line.chars().take(OUTPUT_LIMIT).collect(),
                },
            ));
        }
    };
    let pushed = execute(Execute {
        env: &NON_INTERACTIVE_ENV,
        timeout: Some(PUSH_TIMEOUT),
        progress: Some(&mut report),
        ..Execute::new(cwd, &refs)
    })
    .await
    .map_err(|error| ActionError::at(ActionPhase::Push, error))?;
    if !pushed.ok() {
        return Err(ActionError::at(ActionPhase::Push, "Git push failed."));
    }
    let current_upstream = stdout(
        cwd,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    );
    Ok(PushStep {
        status: PushStepStatus::Pushed,
        branch: Some(branch),
        upstream_branch: current_upstream
            .as_deref()
            .and_then(|value| split_remote_ref(value, &remote_names(cwd)))
            .map(|(_, branch)| branch)
            .or(current_upstream),
        set_upstream: Some(upstream.is_none()),
    })
}

async fn pull_request(
    cwd: &Path,
    request: &RunStackedAction,
    github: Option<&crate::github::cli::GitHubCli>,
    text: Option<&TextGenerator>,
) -> Result<PrStep, ActionError> {
    let github =
        github.ok_or_else(|| ActionError::at(ActionPhase::Pr, "GitHub CLI is unavailable."))?;
    let branch = stdout(cwd, &["branch", "--show-current"])
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| {
            ActionError::at(
                ActionPhase::Pr,
                "Cannot create a pull request from a detached HEAD.",
            )
        })?;
    let upstream = stdout(
        cwd,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    );
    if upstream.is_none() {
        return Err(ActionError::at(
            ActionPhase::Pr,
            "Push the branch with an upstream before creating a pull request.",
        ));
    }
    let context = branch_head_context(cwd, &branch, upstream.as_deref(), None);
    let (repository, host) = github_scope(cwd);
    let default = match repository.as_deref() {
        Some(repository) => github
            .default_branch(cwd, repository, host.as_deref())
            .await
            .map_err(|error| ActionError::at(ActionPhase::Pr, error))?,
        None => None,
    }
    .or_else(|| default_branch(cwd, &primary_remote(cwd).unwrap_or_else(|| "origin".into())))
    .unwrap_or_else(|| "main".into());
    if let Some(existing) = find_open_pr(github, cwd, &context, host.as_deref())
        .await
        .map_err(|error| ActionError::at(ActionPhase::Pr, error))?
    {
        return Ok(PrStep {
            status: PrStepStatus::OpenedExisting,
            url: Some(existing.url),
            number: Some(existing.number),
            base_branch: Some(existing.base_ref_name),
            head_branch: Some(existing.head_ref_name),
            title: Some(existing.title),
        });
    }
    let commit_subject =
        stdout(cwd, &["log", "-1", "--format=%s"]).unwrap_or_else(|| "Update project files".into());
    let template = pull_request_template(cwd, &default).ok().flatten();
    let commits =
        stdout(cwd, &["log", "--format=%s", &format!("{default}..HEAD")]).unwrap_or_default();
    let stat = stdout(cwd, &["diff", "--stat", &format!("{default}...HEAD")]).unwrap_or_default();
    let patch =
        stdout(cwd, &["diff", "--no-color", &format!("{default}...HEAD")]).unwrap_or_default();
    let generation = text
        .map(|text| {
            text.generation_settings(
                request.project_id.as_deref().unwrap_or_default(),
                "generatePullRequestContent",
            )
        })
        .unwrap_or_default();
    let mut content = GeneratedPrContent {
        title: commit_subject,
        body: template
            .clone()
            .unwrap_or_else(|| "## Summary\n\n## Testing\n".into()),
    };
    if let Some(text) = text {
        if let Ok(raw) = text
            .generate(TextGenerationRequest {
                operation: "git-pull-request-content",
                project: request.project_id.clone().unwrap_or_default(),
                cwd: request.cwd.clone(),
                prompt: pr_content_prompt(
                    &default,
                    &branch,
                    &commits,
                    &stat,
                    &patch,
                    template.as_deref(),
                ),
                attachments: vec![],
                model: generation.model,
                instructions: generation.instructions,
                output_schema: pr_content_schema(),
            })
            .await
        {
            if let Ok(generated) = serde_json::from_str::<GeneratedPrContent>(&raw) {
                content = generated;
            }
        }
    }
    content = sanitize_pr_content(content);
    let body = tempfile::NamedTempFile::new_in(cwd)
        .map_err(|error| ActionError::at(ActionPhase::Pr, error))?;
    std::fs::write(body.path(), &content.body)
        .map_err(|error| ActionError::at(ActionPhase::Pr, error))?;
    github
        .create_pull_request(
            cwd,
            &default,
            &branch,
            &content.title,
            body.path(),
            repository.as_deref(),
            host.as_deref(),
        )
        .await
        .map_err(|error| ActionError::at(ActionPhase::Pr, error))?;
    let created = find_open_pr(github, cwd, &context, host.as_deref())
        .await
        .map_err(|error| ActionError::at(ActionPhase::Pr, error))?
        .ok_or_else(|| {
            ActionError::at(
                ActionPhase::Pr,
                "GitHub did not return the created pull request.",
            )
        })?;
    Ok(PrStep {
        status: PrStepStatus::Created,
        url: Some(created.url),
        number: Some(created.number),
        base_branch: Some(created.base_ref_name),
        head_branch: Some(created.head_ref_name),
        title: Some(created.title),
    })
}
