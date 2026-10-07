//! What the tools read and change: the runtime's threads, the Host's projects and
//! the live provider catalog.
use super::ModelCatalog;
use crate::conversation::ProjectCatalog;
use crate::projects::NamedProjectError;
use crate::workspace_files::WorkspaceFiles;
use agent_domain::{Attachment, Command, CommandId, Reply, State, ThreadId, ThreadShell};
use agent_protocol::{
    models::{Model, ProjectScript},
    provider::ProviderKind,
};
use agent_runtime::{HostProject, LaunchThread, Runtime, SearchMatch};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

/// A committed command: its reply and the global sequence number.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Dispatched {
    pub(crate) reply: Reply,
    pub(crate) sequence: u64,
}

/// A handled command's durable receipt: its thread, reply and global sequence
/// number.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CommandReceipt {
    pub(crate) thread: ThreadId,
    pub(crate) reply: Reply,
    pub(crate) sequence: u64,
}

/// Reduced to what capability reporting and target resolution read.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProviderSnapshot {
    pub(crate) instance: String,
    pub(crate) driver: String,
    pub(crate) display_name: Option<String>,
    /// A V2 adapter serves the instance.
    pub(crate) adapter: bool,
    pub(crate) enabled: bool,
    pub(crate) installed: bool,
    pub(crate) unavailable: Option<String>,
    pub(crate) status_error: Option<String>,
    pub(crate) unauthenticated: bool,
    pub(crate) models: Vec<ProviderModel>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProviderModel {
    pub(crate) slug: String,
    pub(crate) name: Option<String>,
    /// Option descriptors; `None` skips option validation.
    pub(crate) options: Option<Vec<Value>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProjectFailure {
    Conflict,
    Operation(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NamedProjectFailure {
    Named(NamedProjectError),
    /// The created project could not be read back.
    Unavailable,
}

pub(crate) trait Orchestration: Send + Sync {
    /// A thread's committed state; a thread never created has no `thread`.
    fn state(&self, thread: &ThreadId) -> BoxFuture<'_, Result<Arc<State>, String>>;
    fn dispatch(
        &self,
        thread: &ThreadId,
        id: CommandId,
        command: Command,
    ) -> BoxFuture<'_, Result<Dispatched, String>>;
    /// The receipt of a command already handled.
    fn receipt(&self, id: &CommandId) -> BoxFuture<'_, Result<Option<CommandReceipt>, String>>;
    /// Every thread that is not deleted.
    fn shells(&self) -> BoxFuture<'_, Result<Vec<ThreadShell>, String>>;
    fn search(
        &self,
        query: String,
        limit: Option<usize>,
    ) -> BoxFuture<'_, Result<Vec<SearchMatch>, String>>;
    fn launch(&self, request: LaunchThread) -> BoxFuture<'_, Result<ThreadId, String>>;
    /// Claims uploads into the thread's attachment storage, as a message's
    /// intake does; the error says why an attachment cannot be sent.
    fn claim_attachments(
        &self,
        thread: &ThreadId,
        attachments: Vec<Attachment>,
    ) -> BoxFuture<'_, Result<Vec<Attachment>, String>>;
    fn providers(&self) -> BoxFuture<'_, Result<Vec<ProviderSnapshot>, String>>;
    fn projects(&self) -> Vec<HostProject>;
    fn project_scripts(&self, project: &str) -> Vec<ProjectScript>;
    fn create_project(
        &self,
        root: PathBuf,
        title: String,
        create_missing: bool,
        scripts: Vec<ProjectScript>,
    ) -> BoxFuture<'_, Result<HostProject, ProjectFailure>>;
    /// Starts a project from just its title in a new repository of its own; the
    /// project, and why its first commit failed if it did.
    fn create_named_project(
        &self,
        title: String,
    ) -> BoxFuture<'_, Result<(HostProject, Option<String>), NamedProjectFailure>>;
}

/// The runtime, project catalog and model catalog this Host serves.
pub(crate) struct HostOrchestration {
    pub(crate) runtime: Arc<Runtime>,
    pub(crate) projects: Arc<ProjectCatalog>,
    pub(crate) files: WorkspaceFiles,
    pub(crate) models: Arc<dyn ModelCatalog>,
    /// Drivers with a configured program.
    pub(crate) installed: Vec<ProviderKind>,
}

impl Orchestration for HostOrchestration {
    fn state(&self, thread: &ThreadId) -> BoxFuture<'_, Result<Arc<State>, String>> {
        let thread = thread.clone();
        Box::pin(async move {
            self.runtime
                .state(&thread)
                .await
                .map(|view| view.state)
                .map_err(|error| error.to_string())
        })
    }

    fn dispatch(
        &self,
        thread: &ThreadId,
        id: CommandId,
        command: Command,
    ) -> BoxFuture<'_, Result<Dispatched, String>> {
        let thread = thread.clone();
        Box::pin(async move {
            self.runtime
                .dispatch(thread, id, command)
                .await
                .map(|committed| Dispatched {
                    reply: committed.reply,
                    sequence: committed.global_seq,
                })
                .map_err(|error| error.to_string())
        })
    }

    fn receipt(&self, id: &CommandId) -> BoxFuture<'_, Result<Option<CommandReceipt>, String>> {
        let id = id.clone();
        Box::pin(async move {
            self.runtime
                .store()
                .blocking(move |store| store.receipt(&id))
                .await
                .map(|stored| {
                    stored.map(|stored| CommandReceipt {
                        thread: stored.thread,
                        reply: stored.receipt.reply,
                        sequence: stored.global_seq,
                    })
                })
                .map_err(|error| error.to_string())
        })
    }

    fn shells(&self) -> BoxFuture<'_, Result<Vec<ThreadShell>, String>> {
        Box::pin(async move {
            self.runtime
                .store()
                .blocking(|store| store.thread_shells())
                .await
                .map(|rows| rows.into_iter().map(|row| *row.row.summary).collect())
                .map_err(|error| error.to_string())
        })
    }

    fn search(
        &self,
        query: String,
        limit: Option<usize>,
    ) -> BoxFuture<'_, Result<Vec<SearchMatch>, String>> {
        Box::pin(async move {
            let runtime = self.runtime.clone();
            tokio::task::spawn_blocking(move || runtime.search(&query, limit))
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())
        })
    }

    fn claim_attachments(
        &self,
        thread: &ThreadId,
        attachments: Vec<Attachment>,
    ) -> BoxFuture<'_, Result<Vec<Attachment>, String>> {
        let (files, thread) = (self.files.clone(), thread.clone());
        Box::pin(async move {
            tokio::task::spawn_blocking(move || files.claim(thread.as_str(), &attachments))
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| format!("{error:#}"))
        })
    }

    fn launch(&self, request: LaunchThread) -> BoxFuture<'_, Result<ThreadId, String>> {
        Box::pin(async move {
            self.runtime
                .launch(request)
                .await
                .map(|reply| reply.thread)
                .map_err(|error| error.to_string())
        })
    }

    fn providers(&self) -> BoxFuture<'_, Result<Vec<ProviderSnapshot>, String>> {
        Box::pin(async move {
            let models = self.models.models().await?;
            Ok([ProviderKind::Codex, ProviderKind::Claude]
                .into_iter()
                .map(|kind| ProviderSnapshot {
                    instance: provider_id(kind).into(),
                    driver: provider_id(kind).into(),
                    display_name: Some(provider_name(kind).into()),
                    adapter: true,
                    enabled: true,
                    installed: self.installed.contains(&kind),
                    unavailable: None,
                    status_error: None,
                    unauthenticated: false,
                    models: models
                        .iter()
                        .filter(|model| model.model.provider == kind)
                        .map(|model| ProviderModel {
                            slug: model.id.clone(),
                            name: Some(model.display_name.clone()),
                            options: Some(model_options(model)),
                        })
                        .collect(),
                })
                .collect())
        })
    }

    fn projects(&self) -> Vec<HostProject> {
        self.projects.list()
    }

    fn project_scripts(&self, project: &str) -> Vec<ProjectScript> {
        self.projects.scripts(project)
    }

    fn create_project(
        &self,
        root: PathBuf,
        title: String,
        create_missing: bool,
        scripts: Vec<ProjectScript>,
    ) -> BoxFuture<'_, Result<HostProject, ProjectFailure>> {
        Box::pin(async move {
            if create_missing {
                tokio::fs::create_dir_all(&root)
                    .await
                    .map_err(|error| ProjectFailure::Operation(error.to_string()))?;
            }
            let id = match self
                .projects
                .store()
                .add(&root, Some(&title), scripts)
                .await
                .map_err(|error| ProjectFailure::Operation(format!("{error:#}")))?
            {
                crate::projects::Registration::Created(id) => id,
                crate::projects::Registration::Existing(_) => return Err(ProjectFailure::Conflict),
            };
            crate::conversation::project_added(&self.runtime, &self.projects, &id).await;
            self.projects
                .list()
                .into_iter()
                .find(|project| project.id == id)
                .ok_or_else(|| ProjectFailure::Operation("The project was not registered.".into()))
        })
    }

    fn create_named_project(
        &self,
        title: String,
    ) -> BoxFuture<'_, Result<(HostProject, Option<String>), NamedProjectFailure>> {
        Box::pin(async move {
            let created = crate::projects::create_named_project(
                self.projects.store(),
                &title,
                &crate::projects::Git::default(),
            )
            .await
            .map_err(NamedProjectFailure::Named)?;
            crate::conversation::project_added(&self.runtime, &self.projects, &created.id).await;
            let project = self
                .projects
                .list()
                .into_iter()
                .find(|project| project.id == created.id)
                .ok_or(NamedProjectFailure::Unavailable)?;
            Ok((project, created.commit_error))
        })
    }
}

pub(crate) fn provider_id(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Codex => "codex",
        ProviderKind::Claude => "claude",
    }
}
fn provider_name(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Codex => "Codex",
        ProviderKind::Claude => "Claude",
    }
}

/// The option descriptors the composer offers for a model.
pub(crate) fn model_options(model: &Model) -> Vec<Value> {
    let mut options = vec![];
    let effort = if model.model.provider == ProviderKind::Claude {
        "effort"
    } else {
        "reasoningEffort"
    };
    if !model.supported_reasoning_efforts.is_empty() {
        options.push(json!({
            "id": effort,
            "type": "select",
            "label": "Reasoning effort",
            "options": model.supported_reasoning_efforts.iter().map(|e| json!({
                "id": e.reasoning_effort,
                "label": e.reasoning_effort,
                "isDefault": e.reasoning_effort == model.default_reasoning_effort,
            })).collect::<Vec<_>>(),
        }));
    }
    if let Some(tiers) = &model.service_tiers {
        options.push(json!({
            "id": "serviceTier",
            "type": "select",
            "label": "Service tier",
            "options": tiers.iter().map(|t| json!({
                "id": t.id,
                "label": t.name.as_ref().unwrap_or(&t.id),
                "isDefault": model.default_service_tier.as_ref() == Some(&t.id),
            })).collect::<Vec<_>>(),
        }));
    }
    options
}
