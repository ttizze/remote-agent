//! Capabilities, delegated tasks, thread creation and the cross-thread
//! list/send/wait/interrupt.
use super::backend::ProviderSnapshot;
use super::{
    AgentTools, Outcome, Scope, ToolError, bounded, decode, failure, invalid, request_key,
    stable_command, stable_id, thread_id, trimmed,
};
use agent_domain::{
    BackgroundKind, Command, CompletionWake, DeliveryState, DispatchMode, Driver, InputIntent,
    InteractionMode, ItemStatus, LinkedPullRequest, MessageAuthor, MessageId, ModelSelection,
    NodeId, NotificationSource, Run, RunId, RunStatus, RuntimeMode, SendMessage, State, Task,
    ThreadId, delegated_result, delegated_task_status,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

pub(crate) const DEFAULT_WAIT_TIMEOUT_MS: f64 = 10.0 * 60.0 * 1_000.0;
pub(crate) const MAX_WAIT_TIMEOUT_MS: f64 = 60.0 * 60.0 * 1_000.0;
const TASK_POLL_INTERVAL: Duration = Duration::from_millis(50);
pub(crate) const THREAD_POLL_INTERVAL: Duration = Duration::from_millis(250);
const DEFAULT_THREAD_LIST_LIMIT: usize = 50;

/// `min(1 h, max(1 ms, timeoutMs ?? 10 min))`.
pub(crate) fn wait_budget(timeout: Option<f64>) -> Duration {
    let ms = timeout
        .filter(|ms| !ms.is_nan())
        .unwrap_or(DEFAULT_WAIT_TIMEOUT_MS)
        .clamp(1.0, MAX_WAIT_TIMEOUT_MS);
    Duration::from_secs_f64(ms / 1000.0)
}

pub(crate) fn runtime_mode_name(mode: RuntimeMode) -> &'static str {
    match mode {
        RuntimeMode::ApprovalRequired => "approval-required",
        RuntimeMode::AutoAcceptEdits => "auto-accept-edits",
        RuntimeMode::Auto => "auto",
        RuntimeMode::FullAccess => "full-access",
    }
}
pub(crate) fn interaction_mode_name(mode: InteractionMode) -> &'static str {
    match mode {
        InteractionMode::Default => "default",
        InteractionMode::Plan => "plan",
    }
}
pub(crate) fn parse_runtime_mode(value: &str) -> Option<RuntimeMode> {
    Some(match value {
        "approval-required" => RuntimeMode::ApprovalRequired,
        "auto-accept-edits" => RuntimeMode::AutoAcceptEdits,
        "auto" => RuntimeMode::Auto,
        "full-access" => RuntimeMode::FullAccess,
        _ => return None,
    })
}
pub(crate) fn parse_interaction_mode(value: &str) -> Option<InteractionMode> {
    Some(match value {
        "default" => InteractionMode::Default,
        "plan" => InteractionMode::Plan,
        _ => return None,
    })
}
fn runtime_rank(mode: RuntimeMode) -> u8 {
    match mode {
        RuntimeMode::ApprovalRequired => 0,
        RuntimeMode::AutoAcceptEdits => 1,
        RuntimeMode::Auto => 2,
        RuntimeMode::FullAccess => 3,
    }
}
/// A requested mode may only narrow the parent's.
pub(crate) fn resolve_runtime_mode(
    parent: RuntimeMode,
    requested: Option<&str>,
) -> Result<RuntimeMode, ToolError> {
    let resolved = match requested {
        None | Some("inherit") => parent,
        Some(mode) => parse_runtime_mode(mode).ok_or_else(|| invalid("Invalid runtimeMode"))?,
    };
    if runtime_rank(resolved) > runtime_rank(parent) {
        return Err(failure(
            "runtime_mode_escalation_denied",
            format!(
                "Child runtime mode {} is broader than parent mode {}.",
                runtime_mode_name(resolved),
                runtime_mode_name(parent)
            ),
        ));
    }
    Ok(resolved)
}
/// Plan is narrower than default.
pub(crate) fn resolve_interaction_mode(
    parent: InteractionMode,
    requested: Option<&str>,
) -> Result<InteractionMode, ToolError> {
    let resolved = match requested {
        None | Some("inherit") => parent,
        Some(mode) => {
            parse_interaction_mode(mode).ok_or_else(|| invalid("Invalid interactionMode"))?
        }
    };
    if parent == InteractionMode::Plan && resolved == InteractionMode::Default {
        return Err(failure(
            "interaction_mode_escalation_denied",
            format!(
                "Child interaction mode {} is broader than parent mode {}.",
                interaction_mode_name(resolved),
                interaction_mode_name(parent)
            ),
        ));
    }
    Ok(resolved)
}

pub(crate) fn latest_active_run(state: &State) -> Option<&Run> {
    state.active_run()
}
pub(crate) fn latest_run(state: &State) -> Option<&Run> {
    state.runs.iter().max_by_key(|run| run.ordinal)
}
pub(crate) fn terminal(status: RunStatus) -> bool {
    status.terminal()
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Target {
    provider_instance_id: Option<String>,
    driver_kind: Option<String>,
    model: Option<String>,
    options: Option<Value>,
}

/// Provider option selection values in request order.
pub(crate) fn option_selections(value: &Value) -> Result<Vec<(String, Value)>, ToolError> {
    let selection = |id: &str, value: &Value| -> Result<(String, Value), ToolError> {
        let id = super::trimmed("option id", id, None)?;
        match value {
            Value::String(text) => Ok((id, json!(super::trimmed("option value", text, None)?))),
            Value::Bool(_) => Ok((id, value.clone())),
            _ => Err(invalid("Model option values must be strings or booleans")),
        }
    };
    match value {
        Value::Array(entries) => entries
            .iter()
            .map(|entry| {
                selection(
                    entry["id"]
                        .as_str()
                        .ok_or_else(|| invalid("Model options need an id"))?,
                    entry.get("value").unwrap_or(&Value::Null),
                )
            })
            .collect(),
        Value::Object(entries) => entries
            .iter()
            .map(|(id, value)| selection(id, value))
            .collect(),
        _ => Err(invalid("Invalid model options")),
    }
}

fn invalid_options(
    selections: &[(String, Value)],
    descriptors: Option<&Vec<Value>>,
) -> Vec<String> {
    let mut problems = vec![];
    let mut seen = std::collections::BTreeSet::new();
    for (id, value) in selections {
        if !seen.insert(id.clone()) {
            problems.push(format!("Option {id} was specified more than once."));
            continue;
        }
        let Some(descriptors) = descriptors else {
            continue;
        };
        let Some(descriptor) = descriptors.iter().find(|d| d["id"] == *id) else {
            let known = descriptors
                .iter()
                .filter_map(|d| d["id"].as_str())
                .collect::<Vec<_>>()
                .join(", ");
            problems.push(format!(
                "Unknown option {id}; supported options: {}.",
                if known.is_empty() { "none" } else { &known }
            ));
            continue;
        };
        if descriptor["type"] == "boolean" && !value.is_boolean() {
            problems.push(format!("Option {id} expects a boolean value."));
            continue;
        }
        if descriptor["type"] == "select"
            && !descriptor["options"]
                .as_array()
                .is_some_and(|choices| choices.iter().any(|choice| choice["id"] == *value))
        {
            let choices = descriptor["options"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|choice| choice["id"].as_str())
                .collect::<Vec<_>>()
                .join(", ");
            problems.push(format!("Option {id} must be one of: {choices}."));
        }
    }
    problems
}

pub(crate) fn provider_constraints(provider: &ProviderSnapshot) -> Vec<String> {
    let mut constraints = vec![];
    if !provider.adapter {
        constraints.push("No V2 provider adapter is registered.".to_owned());
    }
    if !provider.enabled {
        constraints.push("Provider instance is disabled.".into());
    }
    if !provider.installed {
        constraints.push("Provider executable is not installed.".into());
    }
    if let Some(reason) = &provider.unavailable {
        constraints.push(reason.clone());
    }
    if let Some(message) = &provider.status_error {
        constraints.push(message.clone());
    }
    if provider.unauthenticated {
        constraints.push("Provider is not authenticated.".into());
    }
    constraints
}

fn driver(kind: &str) -> Option<Driver> {
    match kind {
        "codex" => Some(Driver::Codex),
        "claude" => Some(Driver::Claude),
        _ => None,
    }
}
fn option_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

pub(crate) fn resolve_target(
    parent: &ModelSelection,
    target: Option<&Target>,
    providers: &[ProviderSnapshot],
) -> Result<ModelSelection, ToolError> {
    let default = Target::default();
    let target = target.unwrap_or(&default);
    let requested_driver = target.driver_kind.as_deref();
    let mut instance = target.provider_instance_id.clone();
    if instance.is_none()
        && let Some(requested) = requested_driver
    {
        let candidates: Vec<_> = providers
            .iter()
            .filter(|provider| provider.driver == requested && provider.adapter)
            .collect();
        if candidates.is_empty() {
            return Err(failure(
                "provider_unavailable",
                format!("No V2 provider adapter is registered for driver {requested}."),
            ));
        }
        let healthy = |candidate: &&&ProviderSnapshot| provider_constraints(candidate).is_empty();
        let inherited = candidates
            .iter()
            .filter(healthy)
            .find(|candidate| candidate.instance == parent.instance);
        instance = inherited
            .or_else(|| candidates.iter().find(healthy))
            .map(|candidate| candidate.instance.clone());
        if instance.is_none() {
            return Err(failure(
                "provider_unavailable",
                format!("No available V2 provider instance for driver {requested}."),
            ));
        }
    }
    let instance = instance.unwrap_or_else(|| parent.instance.clone());
    let Some(provider) = providers.iter().find(|p| p.instance == instance) else {
        return Err(failure(
            "provider_unavailable",
            format!("Provider instance {instance} is not registered."),
        ));
    };
    if let Some(requested) = requested_driver
        && provider.driver != requested
    {
        return Err(failure(
            "invalid_request",
            format!(
                "Provider instance {instance} uses driver {}, not {requested}.",
                provider.driver
            ),
        ));
    }
    let constraints = provider_constraints(provider);
    if !constraints.is_empty() {
        return Err(failure(
            "provider_unavailable",
            format!(
                "Provider {instance} cannot run a child task: {}",
                constraints.join(" ")
            ),
        ));
    }
    let requested_model = target.model.as_deref();
    let model = requested_model
        .map(str::to_owned)
        .or_else(|| (instance == parent.instance).then(|| parent.model.clone()))
        .or_else(|| provider.models.first().map(|model| model.slug.clone()))
        .ok_or_else(|| {
            failure(
                "model_unavailable",
                format!("Provider {instance} has no model available for inheritance."),
            )
        })?;
    if let Some(requested) = requested_model
        && !provider.models.is_empty()
        && !provider
            .models
            .iter()
            .any(|candidate| candidate.slug == requested)
    {
        return Err(failure(
            "model_unavailable",
            format!("Model {requested} is not advertised by provider {instance}."),
        ));
    }
    let requested_options = target.options.as_ref().map(option_selections).transpose()?;
    if let Some(selections) = &requested_options {
        let descriptors = provider
            .models
            .iter()
            .find(|candidate| candidate.slug == model)
            .and_then(|candidate| candidate.options.as_ref());
        let problems = invalid_options(selections, descriptors);
        if !problems.is_empty() {
            return Err(failure(
                "invalid_request",
                format!(
                    "Model {model} on provider {instance} rejected options: {}",
                    problems.join(" ")
                ),
            ));
        }
    }
    let driver = driver(&provider.driver).ok_or_else(|| {
        failure(
            "provider_unavailable",
            format!("Provider instance {instance} is not registered."),
        )
    })?;
    if instance == parent.instance && model == parent.model && requested_options.is_none() {
        return Ok(parent.clone());
    }
    Ok(ModelSelection {
        instance,
        driver,
        model,
        options: requested_options
            .unwrap_or_default()
            .into_iter()
            .map(|(id, value)| (id, option_text(&value)))
            .collect::<BTreeMap<_, _>>(),
    })
}

fn task_status_for_run(run: Option<RunStatus>) -> &'static str {
    match run {
        Some(RunStatus::Queued) => "queued",
        Some(RunStatus::Waiting) => "waiting",
        Some(RunStatus::Completed) => "completed",
        Some(RunStatus::Failed) => "failed",
        Some(RunStatus::Cancelled | RunStatus::RolledBack) => "cancelled",
        Some(RunStatus::Interrupted) => "interrupted",
        Some(RunStatus::Preparing | RunStatus::Starting | RunStatus::Running) | None => "running",
    }
}
fn terminal_task_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "interrupted")
}
fn monitor_runs(state: &State) -> Vec<&RunId> {
    state
        .messages
        .iter()
        .filter(|message| {
            message.notification.as_ref().is_some_and(|notification| {
                notification.source == NotificationSource::Native(BackgroundKind::Monitor)
            })
        })
        .filter_map(|message| message.run.as_ref())
        .collect()
}
/// The child's state once its delegated turn settled.
pub(crate) fn delegated_task_progress(child: &State) -> &'static str {
    let monitors = monitor_runs(child);
    let work: Vec<_> = child
        .runs
        .iter()
        .filter(|run| !monitors.contains(&&run.id) && run.status != RunStatus::RolledBack)
        .collect();
    let active = work.iter().any(|run| !terminal(run.status));
    let children = child.tasks.iter().any(|task| {
        !task.status.terminal()
            || matches!(
                task.delivery,
                DeliveryState::Pending | DeliveryState::Claimed
            ) && task.app_owned()
    }) || !child.background_work.is_empty();
    let result = work
        .iter()
        .any(|run| terminal(run.status) && (run.started_at.is_some() || run.ordinal == 1));
    if active || !result {
        "working"
    } else if children {
        "waiting_for_children"
    } else {
        "result_available"
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DelegateInput {
    task: String,
    target: Option<Target>,
    title: Option<String>,
    role: Option<String>,
    mode: Option<String>,
    timeout_ms: Option<f64>,
    client_request_id: Option<String>,
    runtime_mode: Option<String>,
    interaction_mode: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskInput {
    task_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelInput {
    task_id: String,
    reason: Option<String>,
    client_request_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateThreadRequest {
    prompt: Option<String>,
    title: Option<String>,
    target: Option<Target>,
    runtime_mode: Option<String>,
    interaction_mode: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateThreadsInput {
    threads: Vec<CreateThreadRequest>,
    client_request_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListInput {
    statuses: Option<Vec<String>>,
    title_contains: Option<String>,
    settled: Option<bool>,
    include_subagents: Option<bool>,
    cursor: Option<u64>,
    limit: Option<u64>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendInput {
    thread_id: String,
    message: String,
    mode: Option<String>,
    client_request_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WaitInput {
    thread_id: String,
    run_id: Option<String>,
    timeout_ms: Option<f64>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InterruptInput {
    thread_id: String,
    run_id: Option<String>,
    reason: Option<String>,
    client_request_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequestInput {
    repository: String,
    number: u64,
    url: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateInput {
    thread_id: Option<String>,
    action: String,
    title: Option<String>,
    pull_request: Option<PullRequestInput>,
    client_request_id: Option<String>,
}

fn client_key(value: Option<&String>) -> Result<Option<String>, ToolError> {
    value
        .map(|value| trimmed("clientRequestId", value, Some(256)))
        .transpose()
}
fn max_utf16(field: &str, value: Option<&String>, max: usize) -> Result<(), ToolError> {
    if value.is_some_and(|value| value.encode_utf16().count() > max) {
        return Err(invalid(format!("{field} must be at most {max} characters")));
    }
    Ok(())
}
/// The thread title for create_threads.
fn created_title(parent: &str, prompt: Option<&str>, title: Option<&str>, index: usize) -> String {
    let detail = title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .or(prompt.map(str::trim).filter(|p| !p.is_empty()));
    let Some(detail) = detail else {
        return format!("{parent} thread {}", index + 1);
    };
    let units: Vec<u16> = detail.encode_utf16().collect();
    if units.len() > 80 {
        format!("{}...", String::from_utf16_lossy(&units[..77]))
    } else {
        detail.to_owned()
    }
}
fn is_http_url(value: &str) -> bool {
    let rest = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"));
    rest.is_some_and(|rest| {
        let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
        !host.is_empty() && !host.contains(char::is_whitespace)
    })
}
fn iso(value: &agent_domain::Timestamp) -> Value {
    json!(value)
}

impl AgentTools {
    /// The caller's full projection.
    pub(crate) async fn load_caller(&self, scope: Scope<'_>) -> Result<Arc<State>, ToolError> {
        let unreadable = |error: String| {
            failure(
                "orchestration_error",
                format!("Unable to read thread {}: {error}", scope.thread),
            )
        };
        let state = self.state(scope.thread).await.map_err(unreadable)?;
        if state.thread.is_none() {
            return Err(unreadable(format!(
                "Thread {} was not found.",
                scope.thread
            )));
        }
        Ok(state)
    }
    pub(crate) async fn load_project_thread(
        &self,
        project: &str,
        thread: &ThreadId,
    ) -> Result<Arc<State>, ToolError> {
        let state = self.state(thread).await.map_err(|_| {
            failure(
                "orchestration_error",
                format!("Unable to load thread {thread} in project {project}."),
            )
        })?;
        match &state.thread {
            Some(found) if found.project == project && found.deleted_at.is_none() => Ok(state),
            _ => Err(failure(
                "thread_not_found",
                format!("Thread {thread} was not found in project {project}."),
            )),
        }
    }
    async fn load_scoped(
        &self,
        scope: Scope<'_>,
        target: &ThreadId,
    ) -> Result<(Arc<State>, Arc<State>), ToolError> {
        let parent = self.load_caller(scope).await?;
        let target = if target == scope.thread {
            parent.clone()
        } else {
            let project = parent.thread.as_ref().expect("loaded").project.clone();
            self.load_project_thread(&project, target).await?
        };
        Ok((parent, target))
    }
    async fn providers(&self) -> Result<Vec<ProviderSnapshot>, ToolError> {
        self.backend
            .providers()
            .await
            .map_err(|error| failure("orchestration_error", error))
    }

    pub(crate) async fn capabilities(&self, scope: Scope<'_>) -> Outcome {
        let parent = self.load_caller(scope).await?;
        let thread = parent.thread.as_ref().expect("loaded");
        let providers = self.providers().await?;
        Ok(json!({
            "parentThreadId": scope.thread,
            "inheritedProviderInstanceId": thread.selection.instance,
            "inheritedModel": thread.selection.model,
            "runtimeMode": runtime_mode_name(thread.runtime_mode),
            "interactionMode": interaction_mode_name(thread.interaction_mode),
            "providers": providers.iter().map(|provider| {
                let constraints = provider_constraints(provider);
                json!({
                    "providerInstanceId": provider.instance,
                    "driverKind": provider.driver,
                    "displayName": provider.display_name,
                    "models": provider.models.iter().map(|model| {
                        let mut entry = json!({"id": model.slug, "label": model.name});
                        if let Some(options) = &model.options {
                            entry["options"] = json!(options);
                        }
                        entry
                    }).collect::<Vec<_>>(),
                    "canRunChildTask": constraints.is_empty(),
                    "canRunCrossProviderChildTask": constraints.is_empty(),
                    "constraints": constraints,
                })
            }).collect::<Vec<_>>(),
            "features": {
                "appOwnedSubagents": true,
                "asyncPolling": true,
                "cancellation": true,
                "batchThreadCreation": true,
                "threadManagement": true,
                "incrementalThreadRead": true,
                "scheduledTasks": false,
                "maxBatchThreads": 20,
            },
        }))
    }

    pub(crate) async fn read_task(
        &self,
        scope: Scope<'_>,
        task_id: &NodeId,
        wait_timed_out: bool,
        acknowledge: bool,
    ) -> Outcome {
        let parent = self.load_caller(scope).await?;
        let Some(task) = parent
            .tasks
            .iter()
            .find(|task| &task.id == task_id && task.app_owned())
        else {
            return Err(failure(
                "task_not_found",
                format!(
                    "Delegated task {task_id} does not belong to thread {}.",
                    scope.thread
                ),
            ));
        };
        let child = self.state(&task.child_thread).await.map_err(|error| {
            failure(
                "orchestration_error",
                format!("Unable to read thread {}: {error}", task.child_thread),
            )
        })?;
        let response = task_response(&parent, task, &child, wait_timed_out);
        let status = response["status"].as_str().unwrap_or_default();
        if acknowledge
            && terminal_task_status(status)
            && !matches!(
                task.delivery,
                DeliveryState::Acknowledged | DeliveryState::Disposed
            )
        {
            self.dispatch(
                scope.thread,
                super::new_command(),
                Command::AcknowledgeTask {
                    task: task_id.clone(),
                },
            )
            .await
            .map_err(|error| {
                failure(
                    "orchestration_error",
                    format!("Unable to acknowledge delegated task {task_id}: {error}"),
                )
            })?;
        }
        Ok(response)
    }

    pub(crate) async fn delegate_task(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: DelegateInput = decode(input)?;
        let task_text = trimmed("task", &input.task, Some(120_000))?;
        let title = input
            .title
            .as_deref()
            .map(|title| trimmed("title", title, Some(512)))
            .transpose()?;
        let role = match input.role.as_deref() {
            None | Some("general") => None,
            Some(role @ ("implementation" | "research" | "review" | "design" | "test")) => {
                Some(role)
            }
            Some(_) => return Err(invalid("Invalid role")),
        };
        let wait = match input.mode.as_deref() {
            None | Some("async") => false,
            Some("wait") => true,
            Some(_) => return Err(invalid("Invalid mode")),
        };
        let client = client_key(input.client_request_id.as_ref())?;
        let parent = self.load_caller(scope).await?;
        let thread = parent.thread.as_ref().expect("loaded");
        let Some(parent_run) = latest_active_run(&parent)
            .filter(|run| run.attempt.is_some() && run.selection.instance == scope.instance)
        else {
            return Err(failure(
                "parent_not_active",
                "Delegated tasks require an active run owned by this MCP provider session.",
            ));
        };
        let _ = parent_run;
        let providers = self.providers().await?;
        let selection = resolve_target(&thread.selection, input.target.as_ref(), &providers)?;
        let runtime_mode =
            resolve_runtime_mode(thread.runtime_mode, input.runtime_mode.as_deref())?;
        let interaction_mode =
            resolve_interaction_mode(thread.interaction_mode, input.interaction_mode.as_deref())?;
        let key = request_key(client.as_deref());
        let command = stable_command(scope, "delegate-task", &key, None);
        let task_id = NodeId::new(format!("node:delegated:{command}")).expect("derived id");
        let prompt = match role {
            None => task_text,
            Some(role) => format!("Act as the {role} sub-agent for this task.\n\n{task_text}"),
        };
        self.dispatch(
            scope.thread,
            command.clone(),
            Command::Delegate {
                task: task_id.clone(),
                child: ThreadId::new(format!("thread:delegated:{command}")).expect("derived id"),
                prompt,
                title,
                selection,
                runtime_mode,
                interaction_mode,
                wake: if wait {
                    CompletionWake::SettledOnly
                } else {
                    CompletionWake::Always
                },
            },
        )
        .await
        .map_err(|error| {
            failure(
                "orchestration_error",
                format!("Unable to create delegated task: {error}"),
            )
        })?;
        if !wait {
            return self.read_task(scope, &task_id, false, true).await;
        }
        let budget = wait_budget(input.timeout_ms);
        let waited = tokio::time::timeout(budget, async {
            loop {
                let result = self.read_task(scope, &task_id, false, true).await?;
                if terminal_task_status(result["status"].as_str().unwrap_or_default()) {
                    return Ok(result);
                }
                tokio::time::sleep(TASK_POLL_INTERVAL).await;
            }
        })
        .await;
        if let Ok(result) = waited {
            return result;
        }
        // The wait no longer owns delivery, so a later terminal wakes the parent.
        if let Err(error) = self
            .dispatch(
                scope.thread,
                stable_command(scope, "delegate-task-wake-policy", &key, None),
                Command::SetTaskWake {
                    task: task_id.clone(),
                    wake: CompletionWake::Always,
                },
            )
            .await
        {
            tracing::warn!(operation = "orchestrator-mcp.delegate-task.wake-policy-failed", task = %task_id, message = %error);
        }
        self.read_task(scope, &task_id, true, true).await
    }

    pub(crate) async fn task_status(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: TaskInput = decode(input)?;
        let task = NodeId::new(input.task_id).map_err(|error| invalid(error.to_string()))?;
        self.read_task(scope, &task, false, true).await
    }

    pub(crate) async fn task_cancel(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: CancelInput = decode(input)?;
        max_utf16("reason", input.reason.as_ref(), 2_000)?;
        let client = client_key(input.client_request_id.as_ref())?;
        let task_id = NodeId::new(input.task_id).map_err(|error| invalid(error.to_string()))?;
        let current = self.read_task(scope, &task_id, false, false).await?;
        let key = request_key(client.as_deref());
        let parent = self.load_caller(scope).await?;
        let disposed = parent
            .tasks
            .iter()
            .find(|task| task.id == task_id && task.app_owned())
            .is_some_and(|task| task.delivery == DeliveryState::Disposed);
        let dispose = || async {
            if disposed {
                return Ok(());
            }
            self.dispatch(
                scope.thread,
                stable_command(scope, "cancel-task-completion-delivery", &key, None),
                Command::DisposeTask {
                    task: task_id.clone(),
                },
            )
            .await
            .map(|_| ())
            .map_err(|error| {
                failure(
                    "orchestration_error",
                    format!(
                        "Unable to dispose delegated task {task_id} completion delivery: {error}"
                    ),
                )
            })
        };
        let status = current["status"].as_str().unwrap_or_default().to_owned();
        if terminal_task_status(&status) {
            dispose().await?;
            return Ok(json!({"taskId": task_id, "status": status}));
        }
        let child_id = thread_id(current["childThreadId"].as_str().unwrap_or_default())?;
        let child = self.state(&child_id).await.map_err(|error| {
            failure(
                "orchestration_error",
                format!("Unable to read thread {child_id}: {error}"),
            )
        })?;
        let Some(active) = latest_active_run(&child) else {
            return Err(failure(
                "task_not_cancellable",
                format!("Delegated task {task_id} has no interruptible child run."),
            ));
        };
        self.dispatch(
            &child_id,
            stable_command(scope, "cancel-task", &key, None),
            Command::Interrupt {
                run: active.id.clone(),
                hold_queue: false,
                reason: input.reason.clone(),
            },
        )
        .await
        .map_err(|error| {
            failure(
                "task_not_cancellable",
                format!("Unable to interrupt delegated task {task_id}: {error}"),
            )
        })?;
        if let Err(ToolError::Failure { message, .. }) = dispose().await {
            tracing::warn!(operation = "orchestrator-mcp.cancel-task.delivery-dispose-failed", task = %task_id, message = %message);
        }
        Ok(json!({"taskId": task_id, "status": "cancel_requested"}))
    }

    pub(crate) async fn create_threads(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: CreateThreadsInput = decode(input)?;
        if input.threads.is_empty() || input.threads.len() > 20 {
            return Err(invalid("threads must contain 1 to 20 entries"));
        }
        let client = client_key(input.client_request_id.as_ref())?;
        let mut requests = vec![];
        for request in &input.threads {
            requests.push((
                request
                    .prompt
                    .as_deref()
                    .map(|prompt| trimmed("prompt", prompt, Some(120_000)))
                    .transpose()?,
                request
                    .title
                    .as_deref()
                    .map(|title| trimmed("title", title, Some(512)))
                    .transpose()?,
            ));
        }
        let parent = self.load_caller(scope).await?;
        let thread = parent.thread.as_ref().expect("loaded");
        let Some(parent_run) = latest_active_run(&parent)
            .filter(|run| run.attempt.is_some() && run.selection.instance == scope.instance)
            .cloned()
        else {
            return Err(failure(
                "parent_not_active",
                "Thread creation requires an active run owned by this MCP provider session.",
            ));
        };
        let providers = self.providers().await?;
        let key = request_key(client.as_deref());
        let mut created = vec![];
        for (index, (request, (prompt, title))) in input.threads.iter().zip(requests).enumerate() {
            let selection = resolve_target(&thread.selection, request.target.as_ref(), &providers)?;
            let runtime_mode =
                resolve_runtime_mode(thread.runtime_mode, request.runtime_mode.as_deref())?;
            let interaction_mode = resolve_interaction_mode(
                thread.interaction_mode,
                request.interaction_mode.as_deref(),
            )?;
            let child = ThreadId::new(stable_id("thread", scope, &[&key, &index.to_string()]))
                .expect("derived id");
            let child_title =
                created_title(&thread.title, prompt.as_deref(), title.as_deref(), index);
            self.dispatch(
                &child,
                stable_command(scope, "create-thread", &key, Some(index)),
                Command::Create {
                    thread: child.clone(),
                    project: thread.project.clone(),
                    title: child_title,
                    selection: selection.clone(),
                    runtime_mode,
                    interaction_mode,
                    workspace: thread.workspace.clone(),
                    created_by: MessageAuthor::Agent,
                    creation_source: "mcp".into(),
                },
            )
            .await
            .map_err(|error| {
                failure(
                    "orchestration_error",
                    format!("Unable to create thread {}: {error}", index + 1),
                )
            })?;
            if let Some(prompt) = &prompt {
                self.dispatch(
                    &child,
                    stable_command(scope, "dispatch-thread", &key, Some(index)),
                    Command::Send(SendMessage {
                        scheduled_task: None,
                        context: None,
                        created_by: MessageAuthor::Agent,
                        creation_source: "mcp".into(),
                        id: MessageId::new(stable_id(
                            "message",
                            scope,
                            &[&key, &index.to_string()],
                        ))
                        .expect("derived id"),
                        text: prompt.clone(),
                        attachments: vec![],
                        selection: Some(selection.clone()),
                        mode: DispatchMode::StartImmediately,
                        intent: None,
                        source_plan: None,
                        resolved_plan: None,
                        continuation: None,
                        title_seed: None,
                    }),
                )
                .await
                .map_err(|error| {
                    failure(
                        "orchestration_error",
                        format!("Unable to start thread {}: {error}", index + 1),
                    )
                })?;
            }
            let projection = self.state(&child).await.map_err(|error| {
                failure(
                    "orchestration_error",
                    format!("Unable to read thread {child}: {error}"),
                )
            })?;
            let run = projection.runs.last();
            let created_thread = projection.thread.as_ref().ok_or_else(|| {
                failure(
                    "orchestration_error",
                    format!("Unable to read thread {child}: Thread {child} was not found."),
                )
            })?;
            self.dispatch(
                scope.thread,
                stable_command(scope, "record-created-thread", &key, Some(index)),
                Command::RecordCreatedThread {
                    run: parent_run.id.clone(),
                    thread: child.clone(),
                    project: created_thread.project.clone(),
                    target_run: run.map(|run| run.id.clone()),
                    title: created_thread.title.clone(),
                    selection: created_thread.selection.clone(),
                },
            )
            .await
            .map_err(|error| {
                failure(
                    "orchestration_error",
                    format!(
                        "Unable to record thread {} in the parent timeline: {error}",
                        index + 1
                    ),
                )
            })?;
            created.push(json!({
                "threadId": child,
                "runId": run.map(|run| &run.id),
                "status": run.map_or(json!("idle"), |run| json!(run.status)),
                "title": created_thread.title,
                "createdBy": super::read::actor(created_thread.created_by),
                "creationSource": created_thread.creation_source,
                "providerInstanceId": selection.instance,
                "model": selection.model,
            }));
        }
        Ok(json!({"threads": created}))
    }

    pub(crate) async fn list_threads(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: ListInput = decode(input)?;
        if input.statuses.as_ref().is_some_and(|s| s.len() > 10) {
            return Err(invalid("statuses must contain at most 10 entries"));
        }
        let title = input
            .title_contains
            .as_deref()
            .map(|title| trimmed("titleContains", title, Some(256)))
            .transpose()?
            .map(|title| title.to_lowercase());
        if let Some(limit) = input.limit {
            bounded("limit", limit, 1, Some(100))?;
        }
        let parent = self.load_caller(scope).await?;
        let project = parent.thread.as_ref().expect("loaded").project.clone();
        let mut threads: Vec<_> = self
            .backend
            .shells()
            .await
            .map_err(|error| {
                failure(
                    "orchestration_error",
                    format!("Unable to list threads: Unable to list threads in project {project}. {error}"),
                )
            })?
            .into_iter()
            .filter(|shell| shell.project == project && shell.deleted_at.is_none())
            .filter(|shell| {
                input.include_subagents != Some(false)
                    || super::read::relationship(shell.parent.as_ref(), shell.fork_boundary)
                        != Some("subagent")
            })
            .collect();
        threads.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        let filtered: Vec<_> = threads
            .iter()
            .filter(|shell| {
                input.statuses.as_ref().is_none_or(|statuses| {
                    statuses
                        .iter()
                        .any(|status| *status == super::read::shell_status(shell))
                })
            })
            .filter(|shell| {
                input
                    .settled
                    .is_none_or(|settled| (shell.settled == Some(true)) == settled)
            })
            .filter(|shell| {
                title
                    .as_ref()
                    .is_none_or(|title| shell.title.to_lowercase().contains(title))
            })
            .collect();
        let cursor = input.cursor.unwrap_or(0) as usize;
        let limit = input
            .limit
            .map_or(DEFAULT_THREAD_LIST_LIMIT, |l| l as usize);
        let page: Vec<_> = filtered.iter().skip(cursor).take(limit).collect();
        let next = (cursor + page.len() < filtered.len()).then_some(cursor + page.len());
        Ok(json!({
            "projectId": project,
            "currentThreadId": scope.thread,
            "threads": page.iter().map(|shell| super::read::list_item(shell)).collect::<Vec<_>>(),
            "nextCursor": next,
            "total": filtered.len(),
        }))
    }

    pub(crate) async fn update_thread(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: UpdateInput = decode(input)?;
        let title = input
            .title
            .as_deref()
            .map(|title| trimmed("title", title, Some(512)))
            .transpose()?;
        let pull_request = match &input.pull_request {
            Some(pr) => {
                let repository = trimmed("repository", &pr.repository, None)?;
                let url = trimmed("url", &pr.url, None)?;
                if pr.number == 0 {
                    return Err(invalid("Pull request number must be positive."));
                }
                if !is_http_url(&url) {
                    return Err(invalid(
                        "Pull request URL must be a well-formed HTTP(S) URL.",
                    ));
                }
                Some((repository, pr.number, url))
            }
            None => None,
        };
        let client = match &input.client_request_id {
            Some(key) => {
                let key = trimmed("clientRequestId", key, Some(256))?;
                Some(key)
            }
            None => None,
        };
        match input.action.as_str() {
            "rename" if title.is_none() || pull_request.is_some() => {
                return Err(invalid(
                    "rename requires title and does not accept pullRequest.",
                ));
            }
            "link_pull_request" if pull_request.is_none() || title.is_some() => {
                return Err(invalid(
                    "link_pull_request requires pullRequest and does not accept title.",
                ));
            }
            action @ ("regenerate_title" | "unlink_pull_request")
                if title.is_some() || pull_request.is_some() =>
            {
                return Err(invalid(format!(
                    "{action} does not accept title or pullRequest."
                )));
            }
            "rename" | "link_pull_request" | "regenerate_title" | "unlink_pull_request" => {}
            _ => return Err(invalid("Invalid action")),
        }
        let caller = self.state(scope.thread).await.map_err(|error| {
            failure(
                "orchestration_error",
                format!("Unable to locate calling thread {}: {error}", scope.thread),
            )
        })?;
        let Some(caller_thread) = caller.thread.as_ref() else {
            return Err(failure(
                "thread_not_found",
                format!("Calling thread {} was not found.", scope.thread),
            ));
        };
        let target_id = match &input.thread_id {
            Some(id) => thread_id(id)?,
            None => scope.thread.clone(),
        };
        let target = if &target_id == scope.thread {
            caller.clone()
        } else {
            self.load_project_thread(&caller_thread.project, &target_id)
                .await?
        };
        let project = target.thread.as_ref().expect("loaded").project.clone();
        let key = request_key(client.as_deref());
        let command = super::CommandId::new(stable_id(
            "command",
            scope,
            &["thread-update", target_id.as_str(), &input.action, &key],
        ))
        .expect("derived id");
        let mut update = Command::UpdateMetadata {
            title: None,
            regenerate_title: None,
            branch: None,
            worktree_path: None,
            expected_worktree_path: None,
            expected_empty: false,
            limit_recovery: None,
            linked_pull_request: None,
            project_root: None,
        };
        if let Command::UpdateMetadata {
            title: new_title,
            regenerate_title,
            linked_pull_request,
            ..
        } = &mut update
        {
            match input.action.as_str() {
                "rename" => *new_title = title,
                "regenerate_title" => *regenerate_title = Some(true),
                "link_pull_request" => {
                    let (repository, number, url) = pull_request.expect("validated");
                    *linked_pull_request = Some(Some(LinkedPullRequest {
                        project,
                        repository,
                        number,
                        url,
                    }));
                }
                _ => *linked_pull_request = Some(None),
            }
        }
        let sequence = self
            .dispatch(&target_id, command.clone(), update)
            .await
            .map_err(|error| {
                failure(
                    "orchestration_error",
                    format!("Unable to {} for thread {target_id}: {error}", input.action),
                )
            })?;
        let updated = self.state(&target_id).await.ok();
        let Some(thread) = updated.as_ref().and_then(|state| state.thread.as_ref()) else {
            return Err(failure(
                "orchestration_error",
                format!("Thread {target_id} metadata update completed without a resultant state."),
            ));
        };
        Ok(json!({
            "threadId": target_id,
            "action": input.action,
            "commandId": command,
            "sequence": sequence,
            "title": thread.title,
            "titleRegeneration": super::read::title_regeneration(thread),
            "linkedPullRequest": super::read::linked_pull_request(thread),
            "updatedAt": iso(&thread.updated_at),
        }))
    }

    pub(crate) async fn send_to_thread(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: SendInput = decode(input)?;
        let target_id = thread_id(&input.thread_id)?;
        let message = trimmed("message", &input.message, Some(120_000))?;
        let mode = input.mode.as_deref().unwrap_or("auto");
        if !matches!(mode, "auto" | "queue" | "steer" | "restart") {
            return Err(invalid("Invalid mode"));
        }
        let client = client_key(input.client_request_id.as_ref())?;
        let (parent, target) = self.load_scoped(scope, &target_id).await?;
        let caller = parent.thread.as_ref().expect("loaded");
        let thread = target.thread.as_ref().expect("loaded");
        resolve_runtime_mode(
            caller.runtime_mode,
            Some(runtime_mode_name(thread.runtime_mode)),
        )?;
        resolve_interaction_mode(
            caller.interaction_mode,
            Some(interaction_mode_name(thread.interaction_mode)),
        )?;
        let key = request_key(client.as_deref());
        let message_id = MessageId::new(stable_id("message", scope, &["thread-send", &key]))
            .expect("derived id");
        if thread.archived_at.is_some() {
            return Err(failure(
                "thread_not_sendable",
                format!("Thread {target_id} is archived and cannot receive messages."),
            ));
        }
        let steerable = steerable_run(&target).map(|run| run.id.clone());
        let dispatch_mode = match (mode, steerable) {
            ("steer" | "restart", None) => {
                return Err(failure(
                    "thread_not_sendable",
                    format!(
                        "Thread {target_id} has no running turn that can be {}.",
                        if mode == "steer" {
                            "steered"
                        } else {
                            "restarted"
                        }
                    ),
                ));
            }
            ("restart", Some(run)) => DispatchMode::RestartActive { run },
            ("steer" | "auto", Some(run)) => DispatchMode::SteerActive { run },
            ("queue", _) => DispatchMode::QueueAfterActive,
            _ => DispatchMode::StartImmediately,
        };
        self.dispatch(
            &target_id,
            stable_command(scope, "thread-send", &key, None),
            Command::Send(SendMessage {
                scheduled_task: None,
                context: None,
                created_by: MessageAuthor::Agent,
                creation_source: "mcp".into(),
                id: message_id.clone(),
                text: message,
                attachments: vec![],
                selection: None,
                mode: dispatch_mode,
                intent: None,
                source_plan: None,
                resolved_plan: None,
                continuation: None,
                title_seed: None,
            }),
        )
        .await
        .map_err(|error| {
            failure(
                "orchestration_error",
                format!("Unable to send to thread {target_id}: {error}"),
            )
        })?;
        let durable = || {
            failure(
                "orchestration_error",
                format!(
                    "Message {message_id} was accepted on thread {target_id} without a durable run projection."
                ),
            )
        };
        let after = self
            .load_project_thread(&caller.project, &target_id)
            .await?;
        let sent = after
            .messages
            .iter()
            .find(|message| message.id == message_id)
            .ok_or_else(durable)?;
        let run = after
            .runs
            .iter()
            .find(|run| Some(&run.id) == sent.run.as_ref())
            .ok_or_else(durable)?;
        let delivery = match sent.intent {
            InputIntent::QueuedTurn => "queued",
            InputIntent::TurnStart => "started",
            _ if mode == "restart" => "restarted",
            _ => "steered",
        };
        Ok(json!({
            "threadId": target_id,
            "messageId": message_id,
            "runId": run.id,
            "status": run.status,
            "delivery": delivery,
        }))
    }

    pub(crate) async fn wait_for_thread(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: WaitInput = decode(input)?;
        let target_id = thread_id(&input.thread_id)?;
        let run_id = input
            .run_id
            .map(|id| RunId::new(id).map_err(|error| invalid(error.to_string())))
            .transpose()?;
        let (parent, target) = self.load_scoped(scope, &target_id).await?;
        let project = parent.thread.as_ref().expect("loaded").project.clone();
        let run_not_found = |run: &RunId| {
            failure(
                "run_not_found",
                format!("Run {run} does not belong to thread {target_id}."),
            )
        };
        let selected = match &run_id {
            Some(id) => Some(
                target
                    .runs
                    .iter()
                    .find(|run| &run.id == id)
                    .ok_or_else(|| run_not_found(id))?,
            ),
            None => latest_run(&target),
        };
        let Some(selected) = selected.cloned() else {
            return Ok(
                json!({"threadId": target_id, "runId": null, "status": "idle", "timedOut": false}),
            );
        };
        let result = |run: &Run, timed_out: bool| json!({"threadId": target_id, "runId": run.id, "status": run.status, "timedOut": timed_out});
        if terminal(selected.status) {
            return Ok(result(&selected, false));
        }
        let current = |state: &State| state.runs.iter().find(|run| run.id == selected.id).cloned();
        let waited = tokio::time::timeout(wait_budget(input.timeout_ms), async {
            loop {
                let state = self.load_project_thread(&project, &target_id).await?;
                let run = current(&state).ok_or_else(|| run_not_found(&selected.id))?;
                if terminal(run.status) {
                    return Ok::<_, ToolError>(run);
                }
                tokio::time::sleep(THREAD_POLL_INTERVAL).await;
            }
        })
        .await;
        if let Ok(run) = waited {
            return Ok(result(&run?, false));
        }
        let state = self.load_project_thread(&project, &target_id).await?;
        let run = current(&state).ok_or_else(|| run_not_found(&selected.id))?;
        Ok(result(&run, !terminal(run.status)))
    }

    pub(crate) async fn interrupt_thread(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: InterruptInput = decode(input)?;
        let target_id = thread_id(&input.thread_id)?;
        max_utf16("reason", input.reason.as_ref(), 2_000)?;
        let client = client_key(input.client_request_id.as_ref())?;
        let run_id = input
            .run_id
            .map(|id| RunId::new(id).map_err(|error| invalid(error.to_string())))
            .transpose()?;
        let (_, target) = self.load_scoped(scope, &target_id).await?;
        let key = request_key(client.as_deref());
        let explicit =
            match &run_id {
                Some(id) => Some(target.runs.iter().find(|run| &run.id == id).ok_or_else(
                    || {
                        failure(
                            "run_not_found",
                            format!("Run {id} does not belong to thread {target_id}."),
                        )
                    },
                )?),
                None => None,
            };
        if let Some(run) = explicit
            && terminal(run.status)
        {
            return Ok(json!({"threadId": target_id, "runId": run.id, "status": run.status}));
        }
        let not_interruptible = |run: &RunId| {
            failure(
                "thread_not_interruptible",
                format!("Run {run} is not currently interruptible."),
            )
        };
        let Some(active) = latest_active_run(&target) else {
            return match &run_id {
                None => {
                    Ok(json!({"threadId": target_id, "runId": null, "status": "no_active_run"}))
                }
                Some(run) => Err(not_interruptible(run)),
            };
        };
        if let Some(run) = &run_id
            && run != &active.id
        {
            return Err(not_interruptible(run));
        }
        self.dispatch(
            &target_id,
            stable_command(scope, "thread-interrupt", &key, None),
            Command::Interrupt {
                run: active.id.clone(),
                hold_queue: false,
                reason: input.reason.clone(),
            },
        )
        .await
        .map_err(|error| {
            failure(
                "orchestration_error",
                format!("Unable to interrupt thread {target_id}: {error}"),
            )
        })?;
        Ok(json!({"threadId": target_id, "runId": active.id, "status": "interrupt_requested"}))
    }
}

/// A running run whose provider turn is running.
pub(crate) fn steerable_run(state: &State) -> Option<&Run> {
    state
        .runs
        .iter()
        .filter(|run| {
            run.status == RunStatus::Running
                && run.attempt.as_ref().is_some_and(|attempt| {
                    state.attempts.iter().any(|candidate| {
                        &candidate.id == attempt
                            && candidate.status == agent_domain::AttemptStatus::Running
                    })
                })
        })
        .max_by_key(|run| run.ordinal)
}

/// The delegated task's status record.
fn task_response(parent: &State, task: &Task, child: &State, wait_timed_out: bool) -> Value {
    let status = delegated_task_status(
        task,
        &child.runs,
        &child.items,
        &parent.transfers,
        &child.messages,
    );
    let child_run = status
        .child_run_id
        .as_ref()
        .and_then(|id| child.runs.iter().find(|run| &run.id == id));
    let published = task.result.is_some();
    let work_state = if published {
        "result_available"
    } else {
        // An unpublished result is a run cut by a restart or still on its way.
        match delegated_task_progress(child) {
            "waiting_for_children" => "waiting_for_children",
            _ => "working",
        }
    };
    let task_status = if published {
        match task.status {
            ItemStatus::Completed => "completed",
            ItemStatus::Failed => "failed",
            ItemStatus::Cancelled => "cancelled",
            ItemStatus::Interrupted => "interrupted",
            _ => task_status_for_run(child_run.map(|run| run.status)),
        }
    } else if task_status_for_run(child_run.map(|run| run.status)) == "queued" {
        "queued"
    } else {
        "running"
    };
    let latest = status
        .latest_terminal_run_id
        .as_ref()
        .and_then(|id| child.runs.iter().find(|run| &run.id == id));
    let latest_status = latest.map(|run| task_status_for_run(Some(run.status)));
    let latest_summary = latest.map(|run| {
        if Some(&run.id) == status.child_run_id.as_ref() {
            task.result.clone()
        } else {
            Some(delegated_result(run, &child.items, &child.messages))
        }
    });
    json!({
        "taskId": task.id,
        "childThreadId": task.child_thread,
        "childRunId": status.child_run_id,
        "childNodeId": task.id,
        "status": task_status,
        "workState": work_state,
        "hasPendingChildRuns": status.has_pending_child_runs,
        "providerInstanceId": status.provider_instance_id.or_else(|| child.thread.as_ref().map(|t| t.selection.instance.clone())),
        "model": task.model,
        "summary": task.result,
        "resultContextTransferId": status.result_context_transfer_id,
        "latestTerminalRunId": status.latest_terminal_run_id,
        "latestTerminalStatus": latest_status.filter(|s| terminal_task_status(s)),
        "latestTerminalSummary": latest_summary.flatten(),
        "latestTerminalResultContextTransferId": status.latest_terminal_result_context_transfer_id,
        "waitTimedOut": wait_timed_out,
    })
}
