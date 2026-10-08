//! Thread-scoped worktree inspection and handoff tools.
//!
//! A handoff creates the checkout through the Host's worktree owner, records
//! the binding through the durable thread command, and only then queues any
//! continuation. The provider session is detached by that metadata change;
//! the next turn therefore starts with the new checkout as its cwd.
use super::{
    AgentTools, Outcome, Scope, ToolError, decode, failure, invalid, new_command, unavailable,
};
use agent_domain::{
    Command, DispatchMode, InteractionMode, MessageAuthor, MessageId, RuntimeMode, SendMessage,
};
use agent_protocol::workspace::{ListRefs, RefKind};
use agent_runtime::{SetupProgress, SetupRequest, SetupRun, WorktreeRequest};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

struct HandoffGuard {
    key: String,
    handoffs: Arc<Mutex<std::collections::HashSet<String>>>,
}

impl Drop for HandoffGuard {
    fn drop(&mut self) {
        self.handoffs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.key);
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListInput {
    query: Option<String>,
    cursor: Option<u32>,
    limit: Option<u32>,
    ref_kind: Option<RefKind>,
    include_matching_remote_refs: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HandoffInput {
    branch: String,
    base_ref: Option<String>,
    start_from_origin: Option<bool>,
    path: Option<String>,
    run_setup_script: Option<bool>,
    continuation_prompt: Option<String>,
}

fn project_for(tools: &AgentTools, project: &str) -> Result<agent_runtime::HostProject, ToolError> {
    tools
        .backend
        .projects()
        .into_iter()
        .find(|candidate| candidate.id == project)
        .ok_or_else(|| {
            failure(
                "project_not_found",
                format!("Project '{project}' was not found."),
            )
        })
}

fn nonempty(field: &str, value: &str) -> Result<String, ToolError> {
    super::trimmed(field, value, Some(512))
}

fn setup_result(setup: SetupRun) -> Value {
    match setup {
        SetupRun::NoScript => json!({"status": "no-script"}),
        SetupRun::Started(started) => {
            // The terminal owns the script after it has been started. Dropping
            // the optional observer leaves the command running while keeping
            // the handoff call short enough for the provider detach.
            let _completion = started.completion;
            json!({
                "status": "started",
                "scriptName": started.name,
                "terminalId": started.terminal_id,
            })
        }
    }
}

impl AgentTools {
    async fn rollback_worktree(&self, project_root: &str, path: &str, branch: &str) {
        let _ = self
            .backend
            .remove_thread_worktree(
                project_root.to_owned(),
                path.to_owned(),
                Some(branch.to_owned()),
            )
            .await;
    }

    /// Lists the refs visible from the calling thread's project checkout.
    pub(crate) async fn worktree_list(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: ListInput = decode(input)?;
        if input
            .query
            .as_deref()
            .is_some_and(|query| query.chars().count() > 256)
        {
            return Err(invalid("query must be at most 256 characters"));
        }
        if input.limit.is_some_and(|limit| !(1..=200).contains(&limit)) {
            return Err(invalid("limit must be between 1 and 200"));
        }
        let caller = self.read_caller(scope).await?;
        let thread = caller.thread.as_ref().expect("loaded");
        let project = project_for(self, &thread.project)?;
        let cwd = thread
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.worktree_path.clone())
            .unwrap_or(project.root);
        let refs = self
            .backend
            .vcs_refs(ListRefs {
                cwd,
                query: input.query,
                cursor: input.cursor,
                include_matching_remote_refs: input.include_matching_remote_refs.unwrap_or(false),
                ref_kind: input.ref_kind.unwrap_or(RefKind::All),
                limit: input.limit,
            })
            .await
            .map_err(|_| unavailable())?;
        serde_json::to_value(refs).map_err(|error| unavailable_with(error.to_string()))
    }

    /// Reports the calling thread's durable worktree binding and the Host's
    /// default for new handoffs.
    pub(crate) async fn worktree_status(&self, scope: Scope<'_>) -> Outcome {
        let caller = self.read_caller(scope).await?;
        let thread = caller.thread.as_ref().expect("loaded");
        let project = project_for(self, &thread.project)?;
        let workspace = thread.workspace.as_ref();
        let worktree_path = workspace.and_then(|workspace| workspace.worktree_path.clone());
        Ok(json!({
            "attached": worktree_path.is_some(),
            "worktreePath": worktree_path,
            "branch": workspace.and_then(|workspace| workspace.branch.clone()),
            "projectWorkspaceRoot": project.root,
            "defaultStartFromOrigin": self.backend.worktree_start_from_origin(&thread.project),
        }))
    }

    /// Creates and durably binds a worktree to the calling thread.
    pub(crate) async fn worktree_handoff(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: HandoffInput = decode(input)?;
        let key = scope.thread.to_string();
        {
            let mut handoffs = self
                .handoffs
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !handoffs.insert(key.clone()) {
                return Err(failure(
                    "handoff_in_progress",
                    format!("A worktree handoff is already in progress for thread '{key}'."),
                ));
            }
        }
        let _guard = HandoffGuard {
            key,
            handoffs: self.handoffs.clone(),
        };
        let caller = self.read_mutation_caller(scope).await?;
        let thread = caller.thread.as_ref().expect("loaded");
        if thread.runtime_mode != RuntimeMode::FullAccess
            || thread.interaction_mode != InteractionMode::Default
        {
            return Err(failure(
                "capability_denied",
                "Worktree handoff requires a live full-access/default calling thread.",
            ));
        }
        if let Some(path) = thread
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.worktree_path.as_deref())
        {
            return Err(failure(
                "already_in_worktree",
                format!(
                    "Thread '{}' is already attached to worktree '{path}'.",
                    thread.id
                ),
            ));
        }
        if thread.archived_at.is_some() {
            return Err(failure(
                "invalid_request",
                "Archived threads cannot be handed off to a worktree.",
            ));
        }
        let project = project_for(self, &thread.project)?;
        let branch = nonempty("branch", &input.branch)?;
        let base_ref = input
            .base_ref
            .as_deref()
            .map(|base| nonempty("baseRef", base))
            .transpose()?;
        if let Some(path) = input.path.as_deref()
            && !Path::new(path).is_absolute()
        {
            return Err(invalid("path must be an absolute filesystem path"));
        }
        let local = self
            .backend
            .vcs_status(project.root.clone())
            .await
            .map_err(|error| {
                failure(
                    "operation_failed",
                    format!("Unable to read git status: {error}"),
                )
            })?;
        if !local.is_repo {
            return Err(failure(
                "invalid_request",
                format!(
                    "Project workspace '{}' is not a git repository.",
                    project.root
                ),
            ));
        }
        let refs = self
            .backend
            .vcs_refs(ListRefs {
                cwd: project.root.clone(),
                query: Some(branch.clone()),
                cursor: None,
                include_matching_remote_refs: true,
                ref_kind: RefKind::Local,
                limit: Some(200),
            })
            .await
            .map_err(|error| {
                failure(
                    "operation_failed",
                    format!("Unable to list branches: {error}"),
                )
            })?;
        if refs.refs.iter().any(|reference| reference.name == branch) {
            return Err(failure(
                "invalid_request",
                format!("Branch '{branch}' already exists."),
            ));
        }
        let base_ref = base_ref.or_else(|| local.ref_name.clone()).ok_or_else(|| {
            failure(
                "invalid_request",
                "Could not determine the current branch (detached HEAD). Pass baseRef explicitly.",
            )
        })?;
        let continuation_prompt = input
            .continuation_prompt
            .as_deref()
            .map(|prompt| super::trimmed("continuationPrompt", prompt, Some(120_000)))
            .transpose()?;
        let has_continuation = continuation_prompt.is_some();
        let start_from_origin = input
            .start_from_origin
            .unwrap_or_else(|| self.backend.worktree_start_from_origin(&thread.project));
        let thread_id = thread.id.clone();
        let created = self
            .backend
            .create_thread_worktree(
                WorktreeRequest {
                    thread: thread_id.clone(),
                    project: thread.project.clone(),
                    project_root: project.root.clone(),
                    base_ref: base_ref.clone(),
                    branch: Some(branch.clone()),
                    start_from_origin,
                    progress: SetupProgress::default(),
                    cancel: CancellationToken::new(),
                },
                input.path.clone(),
            )
            .await
            .map_err(|error| {
                failure(
                    "operation_failed",
                    format!("Unable to create the worktree: {error}"),
                )
            })?;
        let worktree_path = created.path;
        let branch = created.branch.unwrap_or(branch);
        let recheck = match self.read_caller(scope).await {
            Ok(recheck) => recheck,
            Err(error) => {
                self.rollback_worktree(&project.root, &worktree_path, &branch)
                    .await;
                return Err(error);
            }
        };
        let rechecked_thread = recheck.thread.as_ref().expect("loaded");
        if let Some(path) = rechecked_thread
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.worktree_path.as_deref())
        {
            self.rollback_worktree(&project.root, &worktree_path, &branch)
                .await;
            return Err(failure(
                "already_in_worktree",
                format!(
                    "Thread '{}' is already attached to worktree '{path}'.",
                    thread.id
                ),
            ));
        }
        if rechecked_thread.archived_at.is_some() {
            self.rollback_worktree(&project.root, &worktree_path, &branch)
                .await;
            return Err(failure(
                "invalid_request",
                "The thread was archived while the worktree was being created; the handoff was rolled back.",
            ));
        }
        let binding = self
            .dispatch(
                &thread_id,
                new_command(),
                Command::UpdateMetadata {
                    title: None,
                    regenerate_title: None,
                    branch: Some(Some(branch.clone())),
                    worktree_path: Some(Some(worktree_path.clone())),
                    expected_worktree_path: Some(None),
                    expected_empty: false,
                    limit_recovery: None,
                    linked_pull_request: None,
                    project_root: Some(project.root.clone()),
                },
            )
            .await;
        match binding {
            Ok(_) => {}
            Err(error) => {
                self.rollback_worktree(&project.root, &worktree_path, &branch)
                    .await;
                return Err(failure(
                    "operation_failed",
                    format!("Unable to bind the thread to the worktree: {error}"),
                ));
            }
        }
        let continuation = if let Some(prompt) = continuation_prompt {
            let command = new_command();
            let message_id = MessageId::new(format!("{command}:message")).expect("derived id");
            match self
                .dispatch(
                    &thread_id,
                    command,
                    Command::Send(SendMessage {
                        created_by: MessageAuthor::Agent,
                        creation_source: "mcp".into(),
                        id: message_id,
                        text: prompt,
                        attachments: vec![],
                        selection: None,
                        mode: DispatchMode::QueueAfterActive,
                        scheduled_task: None,
                        intent: None,
                        source_plan: None,
                        resolved_plan: None,
                        continuation: None,
                        title_seed: None,
                        context: None,
                    }),
                )
                .await
            {
                Ok(_) => json!({"status": "scheduled", "delivery": "queued"}),
                Err(error) => json!({"status": "failed", "detail": error}),
            }
        } else {
            json!({"status": "skipped"})
        };
        let setup_script = if input.run_setup_script.unwrap_or(true) {
            match self
                .backend
                .run_thread_setup(SetupRequest {
                    thread: thread_id,
                    project: project.id.clone(),
                    project_root: project.root.clone(),
                    cwd: worktree_path.clone(),
                    observe: SetupProgress::default(),
                })
                .await
            {
                Ok(setup) => setup_result(setup),
                Err(error) => json!({"status": "failed", "detail": error}),
            }
        } else {
            json!({"status": "skipped"})
        };
        Ok(json!({
            "worktreePath": worktree_path,
            "branch": branch,
            "baseRef": base_ref,
            "startedFromOrigin": start_from_origin,
            "setupScript": setup_script,
            "continuation": continuation,
            "note": if has_continuation {
                "Handoff recorded; the current provider session will detach and the queued continuation will resume inside the worktree."
            } else {
                "Handoff recorded; the current provider session will detach and the next message will resume inside the worktree."
            },
        }))
    }
}

fn unavailable_with(detail: String) -> ToolError {
    failure("orchestration_error", detail)
}

#[cfg(test)]
mod tests {
    use super::nonempty;

    #[test]
    fn handoff_fields_trim_and_reject_empty_values() {
        assert_eq!(
            nonempty("branch", " feature/demo ").unwrap(),
            "feature/demo"
        );
        assert!(nonempty("branch", "  ").is_err());
    }
}
