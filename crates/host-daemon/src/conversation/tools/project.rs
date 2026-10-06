//! The project toolkit: thread launch and the registered projects.
use super::backend::ProjectFailure;
use super::orchestrator::{parse_interaction_mode, parse_runtime_mode};
use super::thread::{SelectionInput, model_selection_json};
use super::{
    AgentTools, Outcome, Scope, ToolError, decode, failure, invalid, new_command, unavailable,
};
use crate::conversation::operations::CHATS_PROJECT;
use agent_domain::{InteractionMode, MessageAuthor, MessageId, RuntimeMode, ThreadId};
use agent_protocol::models::ProjectScript;
use agent_runtime::{HostProject, InitialMessage, LaunchThread, WorkspaceStrategy};
use serde::Deserialize;
use serde_json::{Value, json};

/// The Host keeps no project timestamps or default model.
fn project_json(project: &HostProject, scripts: Vec<ProjectScript>) -> Value {
    json!({
        "id": project.id,
        "title": project.name,
        "workspaceRoot": project.root,
        "defaultModelSelection": null,
        "scripts": scripts,
        "deletedAt": null,
    })
}
fn project_failure(error: ProjectFailure) -> ToolError {
    match error {
        ProjectFailure::Operation(_) => unavailable(),
        ProjectFailure::Conflict => failure(
            "invalid_request",
            "The workspace is already registered to a project.",
        ),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LaunchInput {
    project_id: Option<String>,
    scratch: Option<bool>,
    title: String,
    model_selection: Option<SelectionInput>,
    runtime_mode: Option<String>,
    interaction_mode: Option<String>,
    workspace_strategy: Option<Value>,
    message: Option<String>,
    attachments: Option<Vec<Value>>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListInput {
    cursor: Option<u64>,
    limit: Option<u64>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadInput {
    project_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateInput {
    title: String,
    workspace_root: Option<String>,
    create_workspace_root_if_missing: Option<bool>,
    default_model_selection: Option<Value>,
    scripts: Option<Vec<ProjectScript>>,
}

fn workspace_strategy(value: Option<&Value>) -> Result<WorkspaceStrategy, ToolError> {
    let Some(value) = value else {
        return Ok(WorkspaceStrategy::Root { branch: None });
    };
    let text = |key: &str| -> Result<Option<String>, ToolError> {
        value[key]
            .as_str()
            .map(|text| super::trimmed(key, text, None))
            .transpose()
    };
    match value["type"].as_str() {
        Some("root") => Ok(WorkspaceStrategy::Root {
            branch: text("branch")?,
        }),
        Some("existing_worktree") => Ok(WorkspaceStrategy::ExistingWorktree {
            path: text("worktreePath")?.ok_or_else(|| invalid("worktreePath is required"))?,
            branch: text("branch")?,
        }),
        Some("worktree") => Ok(WorkspaceStrategy::Worktree {
            base_ref: text("baseRef")?.ok_or_else(|| invalid("baseRef is required"))?,
            branch: text("branch")?,
            start_from_origin: value["startFromOrigin"].as_bool().unwrap_or(false),
        }),
        _ => Err(invalid("Invalid workspaceStrategy")),
    }
}

impl AgentTools {
    pub(crate) async fn launch(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: LaunchInput = decode(input)?;
        let title = super::trimmed("title", &input.title, None)?;
        if input
            .message
            .as_ref()
            .is_some_and(|message| message.encode_utf16().count() > 120_000)
        {
            return Err(invalid("message must be at most 120000 characters"));
        }
        let attachments = input.attachments.unwrap_or_default();
        if attachments.len() > 8 {
            return Err(invalid("attachments must contain at most 8 entries"));
        }
        let runtime_mode = input
            .runtime_mode
            .as_deref()
            .map(|mode| parse_runtime_mode(mode).ok_or_else(|| invalid("Invalid runtimeMode")))
            .transpose()?;
        let interaction_mode = input
            .interaction_mode
            .as_deref()
            .map(|mode| {
                parse_interaction_mode(mode).ok_or_else(|| invalid("Invalid interactionMode"))
            })
            .transpose()?;
        let strategy = workspace_strategy(input.workspace_strategy.as_ref())?;
        let caller_state = self.read_mutation_caller(scope).await?;
        let caller = caller_state.thread.as_ref().expect("loaded");
        if caller.runtime_mode != RuntimeMode::FullAccess
            || caller.interaction_mode != InteractionMode::Default
        {
            return Err(failure(
                "capability_denied",
                "Project launches require a full-access/default calling thread.",
            ));
        }
        let command = new_command();
        let thread = ThreadId::new(command.as_str()).expect("derived id");
        let message = MessageId::new(command.as_str()).expect("derived id");
        if !attachments.is_empty() {
            return Err(failure(
                "invalid_request",
                "A new thread accepts only pending attachment uploads.",
            ));
        }
        let scratch = input.scratch == Some(true);
        if scratch && (input.project_id.is_some() || input.workspace_strategy.is_some()) {
            return Err(failure(
                "invalid_request",
                "scratch:true picks its own project and folder; omit projectId and workspaceStrategy.",
            ));
        }
        let project = if scratch {
            CHATS_PROJECT.to_owned()
        } else {
            input.project_id.unwrap_or_else(|| caller.project.clone())
        };
        let selection = match &input.model_selection {
            Some(selection) => self.selection(selection).await?,
            None => caller.selection.clone(),
        };
        let launched = self
            .backend
            .launch(LaunchThread {
                command,
                thread: Some(thread),
                project,
                title,
                generate_title: false,
                selection,
                runtime_mode: runtime_mode.unwrap_or(caller.runtime_mode),
                interaction_mode: interaction_mode.unwrap_or(caller.interaction_mode),
                workspace: strategy,
                initial_message: input.message.map(|text| InitialMessage {
                    id: Some(message.clone()),
                    text,
                    attachments: vec![],
                    created_by: MessageAuthor::Agent,
                    creation_source: "mcp".into(),
                    context: None,
                }),
                created_by: MessageAuthor::Agent,
                creation_source: "mcp".into(),
            })
            .await
            .map_err(|_| unavailable())?;
        let state = self.state(&launched).await.map_err(|_| unavailable())?;
        let thread = state.thread.as_ref().ok_or_else(unavailable)?;
        let run = state.runs.iter().find(|run| run.message == message);
        Ok(json!({
            "threadId": thread.id,
            "projectId": thread.project,
            "modelSelection": model_selection_json(&thread.selection),
            "runId": run.map(|run| &run.id),
            "status": run.map(|run| run.status),
        }))
    }

    pub(crate) async fn project_list(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: ListInput = decode(input)?;
        if let Some(limit) = input.limit {
            super::bounded("limit", limit, 1, Some(100))?;
        }
        self.read_caller(scope).await?;
        let projects = self.backend.projects();
        let start = input.cursor.unwrap_or(0) as usize;
        let end = start + input.limit.unwrap_or(20) as usize;
        Ok(json!({
            "projects": projects
                .iter()
                .skip(start)
                .take(end.saturating_sub(start))
                .map(|project| project_json(project, self.backend.project_scripts(&project.id)))
                .collect::<Vec<_>>(),
            "nextCursor": (end < projects.len()).then_some(end),
        }))
    }

    pub(crate) async fn project_read(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: ReadInput = decode(input)?;
        self.read_caller(scope).await?;
        self.backend
            .projects()
            .iter()
            .find(|project| project.id == input.project_id)
            .map(|project| project_json(project, self.backend.project_scripts(&project.id)))
            .ok_or_else(|| failure("invalid_request", "The project was not found."))
    }

    pub(crate) async fn project_create(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: CreateInput = decode(input)?;
        let title = super::trimmed("title", &input.title, None)?;
        let root = input
            .workspace_root
            .as_deref()
            .map(|root| super::trimmed("workspaceRoot", root, None))
            .transpose()?;
        let caller_state = self.read_mutation_caller(scope).await?;
        let caller = caller_state.thread.as_ref().expect("loaded");
        if caller.archived_at.is_some()
            || caller.runtime_mode != RuntimeMode::FullAccess
            || caller.interaction_mode != InteractionMode::Default
        {
            return Err(failure(
                "capability_denied",
                "Project changes require a live full-access/default calling thread.",
            ));
        }
        let Some(root) = root else {
            if input.scripts.is_some()
                || input.create_workspace_root_if_missing.is_some()
                || input.default_model_selection.is_some()
            {
                return Err(failure(
                    "invalid_request",
                    "A project started from its title takes only a title.",
                ));
            }
            return Err(failure(
                "orchestration_error",
                "This Host cannot start a project from just its title.",
            ));
        };
        if input.default_model_selection.is_some() {
            return Err(failure(
                "invalid_request",
                "This Host does not keep a project default model selection.",
            ));
        }
        let scripts = crate::projects::valid_scripts(input.scripts.unwrap_or_default())
            .map_err(|error| invalid(error.to_string()))?;
        let project = self
            .backend
            .create_project(
                root.into(),
                title,
                input.create_workspace_root_if_missing == Some(true),
                scripts.clone(),
            )
            .await
            .map_err(project_failure)?;
        Ok(project_json(&project, scripts))
    }
}
