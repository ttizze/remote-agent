//! What the tools read and change: the runtime's threads, the Host's projects and
//! the live provider catalog.
use super::ModelCatalog;
use crate::background::BackgroundOwner;
use crate::conversation::ProjectCatalog;
use crate::conversation::SharedResources;
use crate::projects::NamedProjectError;
use crate::workspace_files::{Claimed, Copies, WorkspaceFiles};
use agent_domain::{
    Attachment, Command, CommandId, Driver, OptionDescriptor, Reply, State, ThreadId, ThreadShell,
};
use agent_protocol::{
    background::{
        BackgroundPolicySnapshot, HostResourcesSnapshot, ProcessDiagnosticsResult,
        ProcessResourceHistoryResult, ReadProcessResourceHistory, ReadTraceDiagnostics,
        TraceDiagnosticsResult,
    },
    models::{ProjectScript, ProviderStatus},
    workspace::{ListRefs, RefList, VcsStatus},
};
use agent_runtime::{
    CreatedWorktree as RuntimeCreatedWorktree, HostProject, LaunchThread, Runtime, ScheduledTask,
    ScheduledTaskError, ScheduledTaskInput, SearchMatch, SetupRequest, SetupRun, WorktreeRequest,
};
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
    /// Branch refs visible to a thread-scoped worktree picker.
    fn vcs_refs(&self, request: ListRefs) -> BoxFuture<'_, Result<RefList, String>> {
        let _ = request;
        Box::pin(async { Err("Git ref inspection is unavailable.".into()) })
    }

    /// The local status used to resolve the current branch before a handoff.
    fn vcs_status(&self, cwd: String) -> BoxFuture<'_, Result<VcsStatus, String>> {
        let _ = cwd;
        Box::pin(async { Err("Git status is unavailable.".into()) })
    }

    /// Creates a checkout for a thread-scoped worktree handoff.
    fn create_thread_worktree(
        &self,
        request: WorktreeRequest,
        path: Option<String>,
    ) -> BoxFuture<'_, Result<RuntimeCreatedWorktree, String>> {
        let _ = (request, path);
        Box::pin(async { Err("Worktree creation is unavailable.".into()) })
    }

    /// Removes a checkout created by a handoff, including one at an explicit path.
    fn remove_thread_worktree(
        &self,
        project_root: String,
        path: String,
        branch: Option<String>,
    ) -> BoxFuture<'_, Result<(), String>> {
        let _ = (project_root, path, branch);
        Box::pin(async { Err("Worktree removal is unavailable.".into()) })
    }

    /// Runs the configured project setup script in a newly handed-off checkout.
    fn run_thread_setup(&self, request: SetupRequest) -> BoxFuture<'_, Result<SetupRun, String>> {
        let _ = request;
        Box::pin(async { Err("Project setup is unavailable.".into()) })
    }

    /// The configured default for starting new worktrees from the primary remote.
    fn worktree_start_from_origin(&self, project: &str) -> bool {
        let _ = project;
        true
    }

    /// The durable scheduled-task rows. Backends that do not expose the Host
    /// scheduler keep the MCP surface unavailable; tests can therefore focus
    /// on the ordinary orchestration methods without a scheduler fixture.
    fn scheduled_tasks(&self) -> BoxFuture<'_, Result<Vec<ScheduledTask>, String>> {
        Box::pin(async { Err("scheduled tasks are unavailable".into()) })
    }
    fn upsert_scheduled_task(
        &self,
        input: ScheduledTaskInput,
    ) -> BoxFuture<'_, Result<ScheduledTask, String>> {
        Box::pin(async move {
            let _ = input;
            Err("scheduled tasks are unavailable".into())
        })
    }
    fn set_scheduled_task_enabled(
        &self,
        id: String,
        enabled: bool,
    ) -> BoxFuture<'_, Result<ScheduledTask, String>> {
        Box::pin(async move {
            let _ = (id, enabled);
            Err("scheduled tasks are unavailable".into())
        })
    }
    fn delete_scheduled_task(&self, id: String) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            let _ = id;
            Err("scheduled tasks are unavailable".into())
        })
    }
    fn run_scheduled_task_now(&self, id: String) -> BoxFuture<'_, Result<ScheduledTask, String>> {
        Box::pin(async move {
            let _ = id;
            Err("scheduled tasks are unavailable".into())
        })
    }

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
    pub(crate) resources: SharedResources,
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

    fn scheduled_tasks(&self) -> BoxFuture<'_, Result<Vec<ScheduledTask>, String>> {
        Box::pin(async move {
            self.runtime
                .scheduled_tasks()
                .list()
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn upsert_scheduled_task(
        &self,
        input: ScheduledTaskInput,
    ) -> BoxFuture<'_, Result<ScheduledTask, String>> {
        Box::pin(async move {
            self.runtime
                .scheduled_tasks()
                .upsert(input)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn set_scheduled_task_enabled(
        &self,
        id: String,
        enabled: bool,
    ) -> BoxFuture<'_, Result<ScheduledTask, String>> {
        Box::pin(async move {
            self.runtime
                .scheduled_tasks()
                .set_enabled(&id, enabled)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn delete_scheduled_task(&self, id: String) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.runtime
                .scheduled_tasks()
                .delete(&id)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn run_scheduled_task_now(&self, id: String) -> BoxFuture<'_, Result<ScheduledTask, String>> {
        Box::pin(async move {
            self.runtime
                .scheduled_tasks()
                .run_now(&id)
                .await
                .map_err(|error| match error {
                    ScheduledTaskError::NotFound(id) => format!("Schedule task {id} not found."),
                    ScheduledTaskError::AlreadyRunning(id) => {
                        format!("Schedule task {id} is already running.")
                    }
                    ScheduledTaskError::Store(error) => error.to_string(),
                })
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

    fn vcs_refs(&self, request: ListRefs) -> BoxFuture<'_, Result<RefList, String>> {
        Box::pin(async move {
            crate::vcs::refs(request)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn vcs_status(&self, cwd: String) -> BoxFuture<'_, Result<VcsStatus, String>> {
        Box::pin(async move {
            let path = std::path::PathBuf::from(cwd);
            tokio::task::spawn_blocking(move || crate::vcs::local_status(&path, false))
                .await
                .map_err(|error| error.to_string())?
                .map(|local| VcsStatus::merge(local, None))
                .map_err(|error| error.to_string())
        })
    }

    fn create_thread_worktree(
        &self,
        request: WorktreeRequest,
        path: Option<String>,
    ) -> BoxFuture<'_, Result<RuntimeCreatedWorktree, String>> {
        let resources = self.resources.clone();
        Box::pin(async move {
            if let Some(path) = path {
                let ref_name = if request.start_from_origin {
                    crate::vcs::origin_start(
                        std::path::Path::new(&request.project_root),
                        &request.base_ref,
                    )
                    .await
                    .map_err(|error| error.to_string())?
                } else {
                    request.base_ref.clone()
                };
                let created = crate::vcs::create_worktree(
                    &agent_protocol::vcs::CreateWorktree {
                        cwd: request.project_root.clone(),
                        ref_name,
                        new_ref_name: request.branch.clone(),
                        base_ref_name: Some(request.base_ref.clone()),
                        path: Some(path),
                    },
                    "",
                )
                .await
                .map_err(|error| error.to_string())?;
                return Ok(RuntimeCreatedWorktree {
                    path: created.worktree.path,
                    branch: Some(created.worktree.ref_name),
                });
            }
            resources
                .worktrees
                .create(
                    request.thread.as_str(),
                    &request.project_root,
                    &request.base_ref,
                    request.branch,
                    request.start_from_origin,
                    request.progress,
                    request.cancel,
                )
                .await
                .map(|(path, branch)| RuntimeCreatedWorktree {
                    path: path.to_string_lossy().into_owned(),
                    branch: Some(branch),
                })
                .map_err(|error| format!("{error:#}"))
        })
    }

    fn remove_thread_worktree(
        &self,
        project_root: String,
        path: String,
        branch: Option<String>,
    ) -> BoxFuture<'_, Result<(), String>> {
        let resources = self.resources.clone();
        Box::pin(async move {
            let removed = match resources.worktrees.abandon(path.clone()).await {
                Ok(()) => Ok(()),
                Err(managed_error) => crate::vcs::remove_worktree(&project_root, &path, true)
                    .await
                    .map_err(|error| format!("{error:#}; managed checkout: {managed_error:#}")),
            };
            removed?;
            if let Some(branch) = branch {
                crate::vcs::delete_local_branch(std::path::Path::new(&project_root), &branch, true)
                    .map_err(|error| {
                        format!("Unable to remove created branch '{branch}': {error:#}")
                    })?;
            }
            Ok(())
        })
    }

    fn run_thread_setup(&self, request: SetupRequest) -> BoxFuture<'_, Result<SetupRun, String>> {
        let resources = self.resources.clone();
        Box::pin(async move { crate::conversation::run_project_setup(&resources, request).await })
    }

    fn worktree_start_from_origin(&self, project: &str) -> bool {
        let settings = self.resources.worktrees.latest_host_settings();
        settings
            .project_overrides
            .get(project)
            .and_then(|override_settings| override_settings.new_worktrees_start_from_origin)
            .unwrap_or(settings.new_worktrees_start_from_origin)
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
