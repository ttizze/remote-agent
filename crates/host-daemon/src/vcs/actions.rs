//! The Git action owner. It performs the commands and emits the protocol's
//! progress records; clients only render those records.
use super::process::{
    CommandCancelled, Execute, Executed, NON_INTERACTIVE_ENV, Progress, execute,
};
use super::pull_requests::{branch_head_context, find_open_pr_with_cancel};
use super::{VcsStatusBroadcaster, default_branch, git, primary_remote, remote_names, split_remote_ref, stdout};
use crate::conversation::TextGenerator;
use crate::text_generation::{
    GeneratedCommitMessage, GeneratedPrContent, commit_message_prompt, commit_message_schema,
    format_commit_message, parse_custom_commit_message, pr_content_prompt, pr_content_schema,
    pull_request_template, sanitize_commit_message, sanitize_pr_content,
};
use agent_protocol::vcs::{
    ActionPhase, ActionProgressEvent, ActionProgressKind, ActionToast, ActionToastCta,
    BranchStep, BranchStepStatus, CommitStep, CommitStepStatus, PrStep, PrStepStatus,
    PushStep, PushStepStatus, RunStackedAction, StackedAction, StackedActionResult,
};
use agent_runtime::TextGenerationRequest;
use anyhow::anyhow;
use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio_util::sync::CancellationToken;

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
    if matches!(action, StackedAction::Push | StackedAction::CommitPush | StackedAction::CommitPushPr)
        || (action == StackedAction::CreatePr
            && should_push_before_pr(Path::new(&request.cwd)))
    {
        phases.push(ActionPhase::Push);
    }
    if matches!(action, StackedAction::CreatePr | StackedAction::CommitPushPr) {
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
    session_cancel: CancellationToken,
) -> Result<(ActionProgressEvent, Receiver<ActionProgressEvent>), String> {
    let permit = broadcaster
        .admit_action()
        .ok_or_else(|| "Git actions are unavailable while the Host is shutting down.".to_owned())?;
    let (sender, receiver) = mpsc::channel(64);
    let first = event(
        &request,
        ActionProgressKind::ActionStarted {
            phases: phases(&request),
        },
    );
    let task_request = request.clone();
    tokio::spawn(async move {
        let action_cancel = CancellationToken::new();
        let run_cancel = action_cancel.clone();
        let forward_cancel = {
            let action_cancel = action_cancel.clone();
            let owner_cancel = permit.cancellation();
            tokio::spawn(async move {
                tokio::select! {
                    _ = session_cancel.cancelled() => action_cancel.cancel(),
                    _ = owner_cancel.cancelled() => action_cancel.cancel(),
                }
            })
        };
        let result = run(
            task_request.clone(),
            github,
            text,
            sender.clone(),
            run_cancel,
        )
        .await;
        match result {
            Ok(result) => {
                send_terminal(
                    &sender,
                    event(
                        &task_request,
                        ActionProgressKind::ActionFinished { result },
                    ),
                    &action_cancel,
                )
                .await;
            }
            Err(error) => {
                send_terminal(
                    &sender,
                    event(
                        &task_request,
                        ActionProgressKind::ActionFailed {
                            phase: error.phase,
                            message: error.message,
                        },
                    ),
                    &action_cancel,
                )
                .await;
            }
        }
        // Staging, branch creation, a failed commit hook, and a failed push
        // all change what the controls can offer. The reference invalidates
        // the full status cache even when an action fails. Keep the action
        // admission until this refresh settles so handoff cannot tear down
        // the Host while its own invalidation is still writing the cache.
        tokio::select! {
            _ = action_cancel.cancelled() => {}
            result = broadcaster.refresh_status(&task_request.cwd) => {
                if let Err(error) = result {
                    tracing::warn!(operation = "host.vcs.refresh", message = %format_args!("{error:#}"));
                }
            }
        }
        forward_cancel.abort();
        let _ = forward_cancel.await;
        drop(permit);
    });
    Ok((first, receiver))
}

async fn send_terminal(
    sender: &Sender<ActionProgressEvent>,
    message: ActionProgressEvent,
    cancel: &CancellationToken,
) {
    tokio::select! {
        biased;
        _ = sender.send(message) => {}
        _ = cancel.cancelled() => {}
    }
}

async fn run(
    request: RunStackedAction,
    github: Option<crate::github::cli::GitHubCli>,
    text: Option<TextGenerator>,
    sender: Sender<ActionProgressEvent>,
    cancel: CancellationToken,
) -> Result<StackedActionResult, ActionError> {
    ensure_active(&cancel)?;
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
        && stdout(cwd, &["status", "--porcelain"])
            .is_some_and(|output| !output.is_empty())
    {
        return Err(ActionError::at(
            ActionPhase::Pr,
            "Commit local changes before creating a pull request.",
        ));
    }
    let has_staged_changes = if request.action.commits() {
        stage_selected_files(cwd, &request, &cancel)
            .await
            .map_err(|error| ActionError::at(ActionPhase::Commit, error))?;
        ensure_active(&cancel)?;
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
    }
    let mut generated = if has_staged_changes {
        Some(
            commit_message(&request, &text, &cancel)
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
            &cancel,
        )
        .await;
        ensure_active(&cancel)?;
        let name = create_feature_branch(
            cwd,
            generated
                .as_ref()
                .map(|m| m.branch.as_str())
                .filter(|b| !b.is_empty())
                .or_else(|| generated.as_ref().map(|m| m.subject.as_str())),
            &cancel,
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
        phase(
            &request,
            &sender,
            ActionPhase::Commit,
            "Committing changes",
            &cancel,
        )
        .await;
        ensure_active(&cancel)?;
        let message = generated
            .take()
            .unwrap_or_else(|| GeneratedCommitMessage {
                subject: "Update project files".into(),
                ..Default::default()
            });
        commit(cwd, &request, &message, &sender, &cancel).await?
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
    ) || (request.action == StackedAction::CreatePr && should_push_before_pr(cwd));
    let push = if push_requested {
        phase(
            &request,
            &sender,
            ActionPhase::Push,
            "Pushing changes",
            &cancel,
        )
        .await;
        ensure_active(&cancel)?;
        push(cwd, &request, &sender, &cancel).await?
    } else {
        PushStep {
            status: PushStepStatus::SkippedNotRequested,
            branch: None,
            upstream_branch: None,
            set_upstream: None,
        }
    };
    let pr = if matches!(request.action, StackedAction::CreatePr | StackedAction::CommitPushPr) {
        phase(
            &request,
            &sender,
            ActionPhase::Pr,
            "Opening pull request",
            &cancel,
        )
        .await;
        ensure_active(&cancel)?;
        pull_request(cwd, &request, github.as_ref(), text.as_ref(), &cancel).await?
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
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"],
    );
    let Some(upstream) = upstream else {
        return true;
    };
    stdout(
        cwd,
        &["rev-list", "--left-right", "--count", &format!("{upstream}...HEAD")],
    )
    .and_then(|counts| counts.split_whitespace().nth(1)?.parse::<u64>().ok())
    .is_some_and(|ahead| ahead > 0)
}

async fn stage_selected_files(
    cwd: &Path,
    request: &RunStackedAction,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    let mut add_args = vec!["add".to_owned(), "--".to_owned()];
    match request.file_paths.as_ref().filter(|paths| !paths.is_empty()) {
        Some(paths) => add_args.extend(paths.iter().cloned()),
        None => add_args.push(".".into()),
    }
    let add_refs: Vec<&str> = add_args.iter().map(String::as_str).collect();
    let staged = execute_with_cancel(cancel, Execute {
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
    cancel: &CancellationToken,
) {
    let message = event(
        request,
        ActionProgressKind::PhaseStarted {
            phase,
            label: label.into(),
        },
    );
    tokio::select! {
        _ = cancel.cancelled() => {}
        _ = sender.send(message) => {}
    }
}

fn ensure_active(cancel: &CancellationToken) -> Result<(), ActionError> {
    if cancel.is_cancelled() {
        Err(ActionError::plain("Git action cancelled."))
    } else {
        Ok(())
    }
}

async fn execute_with_cancel<'a>(
    cancel: &CancellationToken,
    mut input: Execute<'a>,
) -> anyhow::Result<Executed> {
    // Cancellation belongs to the process owner. Dropping `execute(input)`
    // here would only drop the future and could leave its child and pipe
    // readers detached from the action permit.
    input.cancel = Some(cancel.clone());
    execute(input).await.map_err(|error| {
        if error.downcast_ref::<CommandCancelled>().is_some() {
            anyhow!("Git action cancelled.")
        } else {
            error
        }
    })
}

async fn commit_message(
    request: &RunStackedAction,
    text: &Option<TextGenerator>,
    cancel: &CancellationToken,
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
    let summary = stdout(Path::new(&request.cwd), &["diff", "--cached", "--stat"])
        .unwrap_or_default();
    let patch = stdout(Path::new(&request.cwd), &["diff", "--cached", "--no-color"])
        .unwrap_or_default();
    let generated = tokio::select! {
        _ = cancel.cancelled() => return Err("Git action cancelled.".into()),
        generated = text.generate(TextGenerationRequest {
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
                output_schema: commit_message_schema(request.feature_branch),
            }) => generated,
    };
    let Ok(generated) = generated else {
        return Ok(fallback);
    };
    serde_json::from_str::<GeneratedCommitMessage>(&generated)
        .map(sanitize_commit_message)
        .map_err(|_| "Text provider returned invalid commit message JSON.".into())
}

async fn create_feature_branch(
    cwd: &Path,
    fragment: Option<&str>,
    cancel: &CancellationToken,
) -> anyhow::Result<String> {
    let base = feature_branch_base(fragment);
    let mut branch = base.clone();
    for suffix in 2..102 {
        if cancel.is_cancelled() {
            return Err(anyhow!("Git action cancelled."));
        }
        let exists = git(
            cwd,
            &["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")],
            super::Options::default(),
        )
        .is_ok_and(|result| result.ok());
        if !exists {
            let args = ["switch", "-c", branch.as_str()];
            let created = execute_with_cancel(cancel, Execute {
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
    cancel: &CancellationToken,
) -> Result<CommitStep, ActionError> {
    ensure_active(cancel)?;
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
    let args = vec!["commit".to_owned(), "-m".to_owned(), message];
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
    let committed = execute_with_cancel(cancel, Execute {
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
    use super::{event, feature_branch_base, run, send_terminal};
    use agent_protocol::vcs::{
        ActionProgressEvent, ActionProgressKind, RunStackedAction, StackedAction,
    };
    use std::time::Duration;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

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

    #[tokio::test]
    async fn canceled_action_reaches_run_before_any_fake_git_command() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (sender, _receiver) = mpsc::channel(1);
        let error = run(
            RunStackedAction {
                action_id: "action:test".into(),
                cwd: "/missing/fake-git-worktree".into(),
                action: StackedAction::Commit,
                commit_message: None,
                feature_branch: false,
                file_paths: None,
                thread_id: None,
                project_id: None,
            },
            None,
            None,
            sender,
            cancel,
        )
        .await
        .expect_err("a canceled action must not run Git");
        assert_eq!(error.message, "Git action cancelled.");
    }

    #[tokio::test]
    async fn terminal_event_waits_for_a_full_progress_queue() {
        let request = RunStackedAction {
            action_id: "action:test".into(),
            cwd: "/tmp/worktree".into(),
            action: StackedAction::Commit,
            commit_message: None,
            feature_branch: false,
            file_paths: None,
            thread_id: None,
            project_id: None,
        };
        let (sender, mut receiver) = mpsc::channel(1);
        sender
            .send(event(
                &request,
                ActionProgressKind::PhaseStarted {
                    phase: agent_protocol::vcs::ActionPhase::Commit,
                    label: "busy".into(),
                },
            ))
            .await
            .unwrap();
        let cancel = CancellationToken::new();
        let terminal = event(
            &request,
            ActionProgressKind::ActionFailed {
                phase: None,
                message: "failed".into(),
            },
        );
        let task_sender = sender.clone();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            send_terminal(&task_sender, terminal, &task_cancel).await;
        });
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        let _ = receiver.recv().await;
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("terminal delivery stayed blocked after queue capacity returned")
            .unwrap();
        assert!(matches!(
            receiver.recv().await,
            Some(ActionProgressEvent {
                kind: ActionProgressKind::ActionFailed { .. },
                ..
            })
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn admitted_action_cancels_an_active_hook_before_releasing_its_permit() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;
        use std::sync::Arc;

        let directory = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(directory.path())
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}: {output:?}");
        };
        git(&["init", "--quiet"]);
        git(&["config", "user.email", "test@test.com"]);
        git(&["config", "user.name", "Test"]);
        let hooks = directory.path().join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        git(&["config", "core.hooksPath", "hooks"]);
        let ready = directory.path().join("hook-ready");
        let release = directory.path().join("hook-release");
        let hook = hooks.join("pre-commit");
        std::fs::write(
            &hook,
            format!(
                "#!/bin/sh\nprintf 'started\\n'\ntouch '{}'\nwhile [ ! -f '{}' ]; do sleep 0.01; done\nexit 1\n",
                ready.display(),
                release.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(directory.path().join("file.txt"), "change\n").unwrap();
        git(&["add", "file.txt"]);

        let broadcaster = super::VcsStatusBroadcaster::new(
            None,
            Arc::new(|| Duration::from_secs(3600)),
        );
        let session_cancel = CancellationToken::new();
        let request = RunStackedAction {
            action_id: "action:active-hook".into(),
            cwd: directory.path().to_string_lossy().into_owned(),
            action: StackedAction::Commit,
            commit_message: Some("Test cancellation".into()),
            feature_branch: false,
            file_paths: None,
            thread_id: None,
            project_id: None,
        };
        let (first, mut receiver) = super::start(
            request,
            None,
            None,
            broadcaster.clone(),
            session_cancel.clone(),
        )
        .unwrap();
        assert!(matches!(first.kind, ActionProgressKind::ActionStarted { .. }));
        for _ in 0..200 {
            if ready.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(ready.exists(), "the admitted action never reached its hook");
        assert!(broadcaster.has_active_actions());

        let (terminal_tx, terminal_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            while let Some(event) = receiver.recv().await {
                if matches!(
                    &event.kind,
                    ActionProgressKind::ActionFinished { .. }
                        | ActionProgressKind::ActionFailed { .. }
                ) {
                    let _ = terminal_tx.send(event);
                    return;
                }
            }
        });
        session_cancel.cancel();
        // Let the hook leave if Git has already been killed. The action's
        // terminal event still has to pass through the bounded progress path.
        std::fs::write(&release, "release\n").unwrap();
        let terminal = tokio::time::timeout(Duration::from_secs(3), terminal_rx)
            .await
            .expect("active cancellation did not produce a terminal event")
            .expect("terminal event watcher dropped");
        assert!(matches!(
            terminal.kind,
            ActionProgressKind::ActionFailed { .. }
        ));
        tokio::time::timeout(Duration::from_secs(3), async {
            while broadcaster.has_active_actions() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the action permit was not released after terminal delivery");
    }
}

async fn push(
    cwd: &Path,
    request: &RunStackedAction,
    sender: &Sender<ActionProgressEvent>,
    cancel: &CancellationToken,
) -> Result<PushStep, ActionError> {
    ensure_active(cancel)?;
    let branch = stdout(cwd, &["branch", "--show-current"])
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| ActionError::at(ActionPhase::Push, "Cannot push a detached HEAD."))?;
    let upstream = stdout(cwd, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"]);
    let upstream_branch = upstream
        .as_deref()
        .and_then(|value| split_remote_ref(value, &remote_names(cwd)))
        .map(|(_, branch)| branch)
        .or_else(|| upstream.clone());
    if upstream.is_some() {
        let counts = stdout(cwd, &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"])
            .unwrap_or_default();
        let mut numbers = counts.split_whitespace().filter_map(|part| part.parse::<u64>().ok());
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
    let pushed = execute_with_cancel(cancel, Execute {
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
    let current_upstream = stdout(cwd, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"]);
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
    cancel: &CancellationToken,
) -> Result<PrStep, ActionError> {
    ensure_active(cancel)?;
    let github = github.ok_or_else(|| ActionError::at(ActionPhase::Pr, "GitHub CLI is unavailable."))?;
    let branch = stdout(cwd, &["branch", "--show-current"])
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| ActionError::at(ActionPhase::Pr, "Cannot create a pull request from a detached HEAD."))?;
    let upstream = stdout(cwd, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"]);
    if upstream.is_none() {
        return Err(ActionError::at(ActionPhase::Pr, "Push the branch with an upstream before creating a pull request."));
    }
    let default = github
        .default_branch_with_cancel(cwd, Some(cancel))
        .await
        .map_err(|error| ActionError::at(ActionPhase::Pr, error))?
        .or_else(|| default_branch(cwd, &primary_remote(cwd).unwrap_or_else(|| "origin".into())))
        .unwrap_or_else(|| "main".into());
    let context = branch_head_context(cwd, &branch, upstream.as_deref(), None);
    let existing = find_open_pr_with_cancel(github, cwd, &context, Some(cancel))
        .await
        .map_err(|error| ActionError::at(ActionPhase::Pr, error))?;
    if let Some(existing) = existing {
        return Ok(PrStep {
            status: PrStepStatus::OpenedExisting,
            url: Some(existing.url),
            number: Some(existing.number),
            base_branch: Some(existing.base_ref_name),
            head_branch: Some(existing.head_ref_name),
            title: Some(existing.title),
        });
    }
    let commit_subject = stdout(cwd, &["log", "-1", "--format=%s"]).unwrap_or_else(|| "Update project files".into());
    let template = pull_request_template(cwd, &default).ok().flatten();
    let commits = stdout(cwd, &["log", "--format=%s", &format!("{default}..HEAD")]).unwrap_or_default();
    let stat = stdout(cwd, &["diff", "--stat", &format!("{default}...HEAD")]).unwrap_or_default();
    let patch = stdout(cwd, &["diff", "--no-color", &format!("{default}...HEAD")]).unwrap_or_default();
    let mut content = GeneratedPrContent {
        title: commit_subject,
        body: template.clone().unwrap_or_else(|| "## Summary\n\n## Testing\n".into()),
    };
    if let Some(text) = text {
        let raw = tokio::select! {
            _ = cancel.cancelled() => return Err(ActionError::plain("Git action cancelled.")),
            raw = text.generate(TextGenerationRequest {
                    operation: "git-pull-request-content",
                    project: request.project_id.clone().unwrap_or_default(),
                    cwd: request.cwd.clone(),
                    prompt: pr_content_prompt(&default, &branch, &commits, &stat, &patch, template.as_deref()),
                    attachments: vec![],
                    output_schema: pr_content_schema(),
                }) => raw,
        };
        if let Ok(raw) = raw {
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
        .create_pull_request_with_cancel(
            cwd,
            &default,
            &branch,
            &content.title,
            body.path(),
            Some(cancel),
        )
        .await
        .map_err(|error| ActionError::at(ActionPhase::Pr, error))?;
    let created = find_open_pr_with_cancel(github, cwd, &context, Some(cancel))
        .await
        .map_err(|error| ActionError::at(ActionPhase::Pr, error))?
        .ok_or_else(|| ActionError::at(ActionPhase::Pr, "GitHub did not return the created pull request."))?;
    Ok(PrStep {
        status: PrStepStatus::Created,
        url: Some(created.url),
        number: Some(created.number),
        base_branch: Some(created.base_ref_name),
        head_branch: Some(created.head_ref_name),
        title: Some(created.title),
    })
}
