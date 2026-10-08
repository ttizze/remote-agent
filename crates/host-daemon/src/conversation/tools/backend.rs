//! What the tools read and change: the runtime's threads, the Host's projects and
//! the live provider catalog.
use super::ModelCatalog;
use crate::background::BackgroundOwner;
use crate::conversation::ProjectCatalog;
use crate::projects::NamedProjectError;
use crate::workspace_files::{Claimed, Copies, WorkspaceFiles};
use agent_domain::{
    Attachment, Command, CommandId, Driver, OptionDescriptor, Reply, State, ThreadId, ThreadShell,
};
use agent_protocol::models::{ProjectScript, ProviderStatus};
use agent_protocol::background::{
    BackgroundPolicySnapshot, HostResourcesSnapshot, ProcessDiagnosticsResult,
    ProcessResourceHistoryResult, ReadProcessResourceHistory, ReadTraceDiagnostics,
    TraceDiagnosticsResult,
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

/// A failed launch: before its thread could take the first message, or
/// possibly after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchFailed {
    NotAccepted,
    Uncertain,
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
    fn launch(&self, request: LaunchThread) -> BoxFuture<'_, Result<ThreadId, LaunchFailed>>;
    /// Claims uploads into the thread's attachment storage, as a message's
    /// intake does: the claimed attachments and the copies held. The error
    /// says why an attachment cannot be sent.
    fn claim_attachments(
        &self,
        thread: &ThreadId,
        attachments: Vec<Attachment>,
    ) -> BoxFuture<'_, Result<Claimed, String>>;
    /// Releases the copies of a claim whose command was not accepted.
    fn release_attachments(&self, copies: Copies) -> BoxFuture<'_, ()>;
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
    fn background_policy(&self) -> BoxFuture<'_, Result<BackgroundPolicySnapshot, String>> {
        Box::pin(async { Err("background diagnostics unavailable".into()) })
    }
    fn host_resources(&self) -> BoxFuture<'_, Result<HostResourcesSnapshot, String>> {
        Box::pin(async { Err("Host resources unavailable".into()) })
    }
    fn process_diagnostics(&self) -> BoxFuture<'_, Result<ProcessDiagnosticsResult, String>> {
        Box::pin(async { Err("process diagnostics unavailable".into()) })
    }
    fn process_history(
        &self,
        _request: ReadProcessResourceHistory,
    ) -> BoxFuture<'_, Result<ProcessResourceHistoryResult, String>> {
        Box::pin(async { Err("process resource history unavailable".into()) })
    }
    fn trace_diagnostics(
        &self,
        _request: ReadTraceDiagnostics,
    ) -> BoxFuture<'_, Result<TraceDiagnosticsResult, String>> {
        Box::pin(async { Err("trace diagnostics unavailable".into()) })
    }
}

/// The runtime, project catalog and model catalog this Host serves.
pub(crate) struct HostOrchestration {
    pub(crate) runtime: Arc<Runtime>,
    pub(crate) projects: Arc<ProjectCatalog>,
    pub(crate) files: WorkspaceFiles,
    pub(crate) models: Arc<dyn ModelCatalog>,
    pub(crate) background: Arc<BackgroundOwner>,
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
    ) -> BoxFuture<'_, Result<Claimed, String>> {
        let (files, thread) = (self.files.clone(), thread.clone());
        Box::pin(async move {
            tokio::task::spawn_blocking(move || files.claim(thread.as_str(), &attachments))
                .await
                .map_err(|error| error.to_string())?
        })
    }

    fn release_attachments(&self, copies: Copies) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let _ = tokio::task::spawn_blocking(move || copies.release()).await;
        })
    }

    fn launch(&self, request: LaunchThread) -> BoxFuture<'_, Result<ThreadId, LaunchFailed>> {
        Box::pin(async move {
            self.runtime
                .launch(request)
                .await
                .map(|reply| reply.thread)
                .map_err(|error| {
                    if error.not_accepted() {
                        LaunchFailed::NotAccepted
                    } else {
                        LaunchFailed::Uncertain
                    }
                })
        })
    }

    fn providers(&self) -> BoxFuture<'_, Result<Vec<ProviderSnapshot>, String>> {
        Box::pin(async move {
            Ok(self
                .models
                .providers()
                .await?
                .into_iter()
                .map(|provider| ProviderSnapshot {
                    driver: match provider.driver {
                        Driver::Codex => "codex",
                        Driver::Claude => "claude",
                    }
                    .into(),
                    instance: provider.instance,
                    display_name: Some(provider.display_name),
                    adapter: true,
                    enabled: provider.enabled,
                    installed: provider.installed,
                    unavailable: provider.unavailable_reason,
                    status_error: (provider.status == ProviderStatus::Error)
                        .then_some(provider.message)
                        .flatten(),
                    unauthenticated: false,
                    models: provider
                        .models
                        .into_iter()
                        .map(|model| ProviderModel {
                            options: Some(
                                model
                                    .option_descriptors
                                    .iter()
                                    .map(descriptor_json)
                                    .collect(),
                            ),
                            slug: model.slug,
                            name: Some(model.name),
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

    fn background_policy(&self) -> BoxFuture<'_, Result<BackgroundPolicySnapshot, String>> {
        Box::pin(async move { Ok(self.background.snapshot().await) })
    }

    fn host_resources(&self) -> BoxFuture<'_, Result<HostResourcesSnapshot, String>> {
        Box::pin(async move { Ok(self.background.host_resources().await) })
    }

    fn process_diagnostics(&self) -> BoxFuture<'_, Result<ProcessDiagnosticsResult, String>> {
        Box::pin(async move { Ok(self.background.process_diagnostics().await) })
    }

    fn process_history(
        &self,
        request: ReadProcessResourceHistory,
    ) -> BoxFuture<'_, Result<ProcessResourceHistoryResult, String>> {
        Box::pin(async move {
            Ok(self
                .background
                .process_history(request.window_ms, request.bucket_ms)
                .await)
        })
    }

    fn trace_diagnostics(
        &self,
        request: ReadTraceDiagnostics,
    ) -> BoxFuture<'_, Result<TraceDiagnosticsResult, String>> {
        Box::pin(async move { self.background.trace_diagnostics(&request).await })
    }
}

/// An option descriptor in the tools' JSON.
pub(crate) fn descriptor_json(descriptor: &OptionDescriptor) -> Value {
    let mut value = match descriptor {
        OptionDescriptor::Select(select) => {
            let mut value = json!({
                "id": select.id,
                "label": select.label,
                "type": "select",
                "options": select.options.iter().map(|choice| {
                    let mut option = json!({"id": choice.id, "label": choice.label});
                    if let Some(description) = &choice.description {
                        option["description"] = json!(description);
                    }
                    if choice.is_default {
                        option["isDefault"] = json!(true);
                    }
                    option
                }).collect::<Vec<_>>(),
            });
            if let Some(current) = &select.current_value {
                value["currentValue"] = json!(current);
            }
            if !select.prompt_injected_values.is_empty() {
                value["promptInjectedValues"] = json!(select.prompt_injected_values);
            }
            value
        }
        OptionDescriptor::Boolean(boolean) => {
            let mut value = json!({"id": boolean.id, "label": boolean.label, "type": "boolean"});
            if let Some(current) = boolean.current_value {
                value["currentValue"] = json!(current);
            }
            value
        }
    };
    let description = match descriptor {
        OptionDescriptor::Select(select) => &select.description,
        OptionDescriptor::Boolean(boolean) => &boolean.description,
    };
    if let Some(description) = description {
        value["description"] = json!(description);
    }
    value
}
