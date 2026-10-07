//! The project toolkit: thread launch and the registered projects.
use super::backend::{LaunchFailed, NamedProjectFailure, ProjectFailure};
use super::orchestrator::{parse_interaction_mode, parse_runtime_mode};
use super::thread::{SelectionInput, model_selection_json};
use super::{
    AgentTools, Outcome, Scope, ToolError, decode, failure, invalid, new_command, unavailable,
};
use crate::conversation::operations::CHATS_PROJECT;
use crate::workspace_files::{Claimed, is_pending_upload};
use agent_domain::{
    Attachment, AttachmentKind, InteractionMode, MessageAuthor, MessageId, RuntimeMode, ThreadId,
};
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
    attachments: Option<Vec<AttachmentInput>>,
}
/// A chat image or file attachment; an image's capture source is app-owned and
/// not taken from tools.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttachmentInput {
    #[serde(rename = "type")]
    kind: String,
    id: String,
    name: String,
    mime_type: String,
    size_bytes: u64,
}
const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;

impl AttachmentInput {
    fn attachment(self) -> Result<Attachment, ToolError> {
        let id = super::trimmed("id", &self.id, Some(128))?;
        // The chat attachment ID schema: `^[a-z0-9_-]+$`, ignoring case.
        if !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(invalid("Invalid attachment"));
        }
        let name = super::trimmed("name", &self.name, Some(255))?;
        let mime_type = super::trimmed("mimeType", &self.mime_type, Some(100))?;
        let kind = match self.kind.as_str() {
            "image" if mime_type.to_ascii_lowercase().starts_with("image/") => {
                super::bounded("sizeBytes", self.size_bytes, 0, Some(MAX_IMAGE_BYTES))?;
                AttachmentKind::Image
            }
            "file" => {
                super::bounded("sizeBytes", self.size_bytes, 1, Some(MAX_FILE_BYTES))?;
                AttachmentKind::File
            }
            _ => return Err(invalid("Invalid attachment")),
        };
        Ok(Attachment {
            kind,
            source: None,
            id,
            name,
            mime_type,
            path: String::new(),
            size: self.size_bytes,
        })
    }
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
    /// Present even when null, which a title-only create also rejects.
    #[serde(default, deserialize_with = "present")]
    default_model_selection: Option<Value>,
    scripts: Option<Vec<ProjectScript>>,
}
fn present<'de, D: serde::Deserializer<'de>>(input: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(input).map(Some)
}
/// A model selection, or null. Project creation records no default model, so
/// it is only validated.
fn default_model_selection(value: &Value) -> Result<(), ToolError> {
    if value.is_null() {
        return Ok(());
    }
    let selection: SelectionInput =
        serde_json::from_value(value.clone()).map_err(|error| invalid(error.to_string()))?;
    super::trimmed("instanceId", &selection.instance_id, None)?;
    super::trimmed("model", &selection.model, None)?;
    if let Some(options) = &selection.options {
        super::orchestrator::option_selections(options)?;
    }
    Ok(())
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
        let attachments = attachments
            .into_iter()
            .map(AttachmentInput::attachment)
            .collect::<Result<Vec<_>, _>>()?;
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
        if attachments
            .iter()
            .any(|attachment| !is_pending_upload(&attachment.id))
        {
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
        // Pending uploads are claimed into the new thread before it exists.
        let claimed = if attachments.is_empty() {
            Claimed::default()
        } else {
            self.backend
                .claim_attachments(&thread, attachments)
                .await
                .map_err(|error| failure("orchestration_error", error))?
        };
        let (attachments, claimed) = (claimed.attachments, claimed.created);
        let initial_message =
            (input.message.is_some() || !attachments.is_empty()).then(|| InitialMessage {
                id: Some(message.clone()),
                text: input.message.unwrap_or_default(),
                attachments,
                created_by: MessageAuthor::Agent,
                creation_source: "mcp".into(),
                context: None,
            });
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
                initial_message,
                created_by: MessageAuthor::Agent,
                creation_source: "mcp".into(),
            })
            .await;
        let launched = match launched {
            Ok(launched) => launched,
            Err(failed) => {
                if failed == LaunchFailed::NotAccepted {
                    self.backend.release_attachments(claimed).await;
                }
                return Err(unavailable());
            }
        };
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
        if let Some(selection) = &input.default_model_selection {
            default_model_selection(selection)?;
        }
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
            let (project, commit_error) =
                self.backend
                    .create_named_project(title)
                    .await
                    .map_err(|error| match error {
                        NamedProjectFailure::Named(error) => {
                            failure("orchestration_error", error.message())
                        }
                        NamedProjectFailure::Unavailable => unavailable(),
                    })?;
            let mut created = project_json(&project, self.backend.project_scripts(&project.id));
            if let Some(commit_error) = commit_error {
                created["commitError"] = json!(commit_error);
            }
            return Ok(created);
        };
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
