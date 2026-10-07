//! MCP service and toolkit tests over a fake orchestration backend.
use super::backend::{Dispatched, Orchestration, ProjectFailure, ProviderModel, ProviderSnapshot};
use super::*;
use agent_domain::{
    Attempt, AttemptStatus, Command, CompletionWake, DeliveryState, Driver, InputIntent,
    InteractionMode, Item, ItemKind, ItemStatus, Message, MessageAuthor, MessageId, ModelSelection,
    NodeId, Question, QuestionOption, Request, RequestBody, RequestStatus, ResponseCapability,
    Role, Run, RunAttemptId, RunId, RunStatus, RuntimeMode, RuntimeRequestId, Task, Thread,
    ThreadShell, Timestamp, TurnItemId,
};
use agent_protocol::models::ProjectScript;
use agent_runtime::{HostProject, LaunchThread, SearchMatch};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
};

/// A created project's root, title, whether its root was created, and scripts.
type CreatedProject = (PathBuf, String, bool, Vec<ProjectScript>);
type Hook = Box<dyn Fn(&ThreadId, &Command) -> Result<Reply, String> + Send + Sync>;

#[derive(Default)]
struct Fake {
    states: Mutex<HashMap<ThreadId, State>>,
    unreadable: Mutex<HashSet<ThreadId>>,
    dispatched: Mutex<Vec<(ThreadId, CommandId, Command)>>,
    hook: Mutex<Option<Hook>>,
    providers: Mutex<Vec<ProviderSnapshot>>,
    projects: Mutex<Vec<HostProject>>,
    launches: Mutex<Vec<LaunchThread>>,
    created: Mutex<Vec<CreatedProject>>,
}
impl Fake {
    fn put(&self, state: State) {
        let id = state.thread.as_ref().unwrap().id.clone();
        self.states.lock().unwrap().insert(id, state);
    }
    fn edit(&self, thread: &str, edit: impl FnOnce(&mut State)) {
        edit(
            self.states
                .lock()
                .unwrap()
                .get_mut(&ThreadId::new(thread).unwrap())
                .unwrap(),
        );
    }
    fn on_dispatch(
        &self,
        hook: impl Fn(&ThreadId, &Command) -> Result<Reply, String> + Send + Sync + 'static,
    ) {
        *self.hook.lock().unwrap() = Some(Box::new(hook));
    }
    fn commands(&self) -> Vec<Command> {
        self.dispatched
            .lock()
            .unwrap()
            .iter()
            .map(|(_, _, command)| command.clone())
            .collect()
    }
    fn command_ids(&self) -> Vec<CommandId> {
        self.dispatched
            .lock()
            .unwrap()
            .iter()
            .map(|(_, id, _)| id.clone())
            .collect()
    }
}
impl Orchestration for Fake {
    fn state(&self, thread: &ThreadId) -> BoxFuture<'_, Result<Arc<State>, String>> {
        let result = if self.unreadable.lock().unwrap().contains(thread) {
            Err("storage unavailable at private-storage-path".to_owned())
        } else {
            Ok(Arc::new(
                self.states
                    .lock()
                    .unwrap()
                    .get(thread)
                    .cloned()
                    .unwrap_or_default(),
            ))
        };
        Box::pin(async move { result })
    }
    fn dispatch(
        &self,
        thread: &ThreadId,
        id: CommandId,
        command: Command,
    ) -> BoxFuture<'_, Result<Dispatched, String>> {
        let mut dispatched = self.dispatched.lock().unwrap();
        dispatched.push((thread.clone(), id, command.clone()));
        let sequence = dispatched.len() as u64;
        let reply = match self.hook.lock().unwrap().as_ref() {
            Some(hook) => hook(thread, &command),
            None => Ok(Reply::Accepted),
        };
        Box::pin(async move { reply.map(|reply| Dispatched { reply, sequence }) })
    }
    fn shells(&self) -> BoxFuture<'_, Result<Vec<ThreadShell>, String>> {
        let shells = self
            .states
            .lock()
            .unwrap()
            .values()
            .filter_map(agent_domain::shell)
            .filter(|shell| shell.deleted_at.is_none())
            .collect();
        Box::pin(async move { Ok(shells) })
    }
    fn search(
        &self,
        _query: String,
        _limit: Option<usize>,
    ) -> BoxFuture<'_, Result<Vec<SearchMatch>, String>> {
        Box::pin(async {
            Ok(vec![
                SearchMatch {
                    thread: ThreadId::new("thread:found").unwrap(),
                    project: "project".into(),
                    source: agent_runtime::SearchSource::User,
                    snippet: "needle".into(),
                    message_created_at: Some(at(0)),
                },
                SearchMatch {
                    thread: ThreadId::new("thread:elsewhere").unwrap(),
                    project: "project:other".into(),
                    source: agent_runtime::SearchSource::Assistant,
                    snippet: "needle".into(),
                    message_created_at: Some(at(0)),
                },
            ])
        })
    }
    fn launch(&self, request: LaunchThread) -> BoxFuture<'_, Result<ThreadId, String>> {
        let thread = request.thread.clone().unwrap();
        let mut launched =
            thread_record(thread.as_str(), &request.project, request.selection.clone());
        launched.title = request.title.clone();
        let mut state = State {
            thread: Some(launched),
            ..State::default()
        };
        if let Some(message) = &request.initial_message {
            let id = message.id.clone().unwrap();
            state.runs.push(run(
                &format!("run:{id}"),
                1,
                RunStatus::Preparing,
                &request.selection.instance,
            ));
            state.runs[0].message = id;
        }
        self.put(state);
        self.launches.lock().unwrap().push(request);
        Box::pin(async move { Ok(thread) })
    }
    fn providers(&self) -> BoxFuture<'_, Result<Vec<ProviderSnapshot>, String>> {
        let providers = self.providers.lock().unwrap().clone();
        Box::pin(async move { Ok(providers) })
    }
    fn projects(&self) -> Vec<HostProject> {
        self.projects.lock().unwrap().clone()
    }
    fn project_scripts(&self, project: &str) -> Vec<ProjectScript> {
        self.created
            .lock()
            .unwrap()
            .iter()
            .find(|(_, title, ..)| format!("project:{title}") == project)
            .map(|(.., scripts)| scripts.clone())
            .unwrap_or_default()
    }
    fn create_project(
        &self,
        root: PathBuf,
        title: String,
        create_missing: bool,
        scripts: Vec<ProjectScript>,
    ) -> BoxFuture<'_, Result<HostProject, ProjectFailure>> {
        self.created
            .lock()
            .unwrap()
            .push((root.clone(), title.clone(), create_missing, scripts));
        let project = HostProject {
            id: format!("project:{title}"),
            name: title,
            root: root.to_string_lossy().into_owned(),
        };
        Box::pin(async move { Ok(project) })
    }
}

fn at(minute: u32) -> Timestamp {
    Timestamp::parse(&format!("2026-10-03T10:{minute:02}:00Z")).unwrap()
}
fn selection(instance: &str, model: &str) -> ModelSelection {
    ModelSelection {
        instance: instance.into(),
        driver: if instance.starts_with("claude") {
            Driver::Claude
        } else {
            Driver::Codex
        },
        model: model.into(),
        options: BTreeMap::new(),
    }
}
fn thread_record(id: &str, project: &str, selection: ModelSelection) -> Thread {
    Thread {
        id: ThreadId::new(id).unwrap(),
        project: project.into(),
        title: "MCP parent".into(),
        selection,
        runtime_mode: RuntimeMode::FullAccess,
        interaction_mode: InteractionMode::Default,
        created_at: at(0),
        updated_at: at(0),
        archived_at: None,
        deleted_at: None,
        settled: None,
        settled_at: None,
        unsettled_at: None,
        snoozed_until: None,
        pinned_at: None,
        pin_order: None,
        active_order: None,
        last_visited_at: None,
        auto_settle: true,
        parent: None,
        fork_boundary: None,
        workspace: None,
        title_request: None,
        imported: false,
        created_by: MessageAuthor::User,
        creation_source: "desktop".into(),
        snoozed_at: None,
        limit_recovery: None,
        linked_pull_request: None,
    }
}
fn thread_state(id: &str) -> State {
    State {
        thread: Some(thread_record(id, "project", selection("codex", "gpt-5.4"))),
        ..State::default()
    }
}
fn run(id: &str, ordinal: u64, status: RunStatus, instance: &str) -> Run {
    let terminal = status.terminal();
    Run {
        restart_of: None,
        restart_cancelled_work: vec![],
        checkpoint_scope: None,
        native_baseline_heads: BTreeMap::new(),
        id: RunId::new(id).unwrap(),
        ordinal,
        message: MessageId::new(format!("message:{id}")).unwrap(),
        selection: selection(instance, "gpt-5.4"),
        status,
        attempt: (!matches!(status, RunStatus::Queued | RunStatus::Preparing))
            .then(|| RunAttemptId::new(format!("attempt:{id}")).unwrap()),
        queue_position: None,
        queue_held: false,
        requested_at: at(0),
        started_at: (status != RunStatus::Queued).then(|| at(1)),
        completed_at: terminal.then(|| at(2)),
        source_plan: None,
        checkpoint: None,
        continuation: false,
    }
}
fn attempt_for(run: &Run, status: AttemptStatus) -> Attempt {
    Attempt {
        id: run.attempt.clone().unwrap(),
        run: run.id.clone(),
        ordinal: 1,
        status,
        native_thread: Some("native".into()),
        native_turn: Some("turn".into()),
        native_head: None,
        accepted: true,
        usage: None,
        context_usage: None,
        turn_usage: None,
        usage_accumulator: None,
        usage_observed: false,
        rejected_limits: BTreeMap::new(),
        started_at: at(1),
        completed_at: None,
    }
}
/// A thread whose provider session `instance` runs an active turn.
fn active_state(id: &str, instance: &str) -> State {
    let mut state = thread_state(id);
    state.thread.as_mut().unwrap().selection = selection(instance, "gpt-5.4");
    let active = run(&format!("run:{id}:active"), 1, RunStatus::Running, instance);
    state
        .attempts
        .push(attempt_for(&active, AttemptStatus::Running));
    state.runs.push(active);
    state
}
fn task(id: &str, child: &str, original: &str) -> Task {
    Task {
        original_message: Some(MessageId::new(original).unwrap()),
        native_task: None,
        background: false,
        id: NodeId::new(id).unwrap(),
        native_key: id.into(),
        run: None,
        attempt: RunAttemptId::new("attempt:parent").unwrap(),
        child_thread: ThreadId::new(child).unwrap(),
        parent_task: None,
        prompt: "Summarize the diff.".into(),
        title: None,
        started_at: at(0),
        completed_at: None,
        model: Some("gpt-5.6-terra".into()),
        status: ItemStatus::Running,
        result: None,
        progress: None,
        wake: CompletionWake::Always,
        delivery: DeliveryState::Pending,
        generation: 0,
    }
}
fn message(id: &str, run: Option<&str>, role: Role, text: &str) -> Message {
    Message {
        notification: None,
        id: MessageId::new(id).unwrap(),
        run: run.map(|run| RunId::new(run).unwrap()),
        role,
        text: text.into(),
        attachments: vec![],
        intent: InputIntent::TurnStart,
        streaming: false,
        created_by: MessageAuthor::User,
        creation_source: "desktop".into(),
        context: None,
        created_at: at(0),
        updated_at: at(0),
    }
}
fn item(id: &str, run: Option<&str>, ordinal: u64, kind: ItemKind, text: &str) -> Item {
    Item {
        id: TurnItemId::new(id).unwrap(),
        run: run.map(|run| RunId::new(run).unwrap()),
        attempt: None,
        native_key: id.into(),
        ordinal,
        kind,
        status: ItemStatus::Completed,
        text: text.into(),
        output_omitted: false,
        output_indicates_failure: false,
        started_at: at(0),
        completed_at: Some(at(1)),
    }
}
fn provider(instance: &str, driver: &str, model: Option<&str>, enabled: bool) -> ProviderSnapshot {
    ProviderSnapshot {
        instance: instance.into(),
        driver: driver.into(),
        display_name: None,
        adapter: true,
        enabled,
        installed: true,
        unavailable: None,
        status_error: None,
        unauthenticated: false,
        models: model
            .into_iter()
            .map(|slug| ProviderModel {
                slug: slug.into(),
                name: Some(slug.into()),
                options: None,
            })
            .collect(),
    }
}

fn tools(fake: &Arc<Fake>) -> AgentTools {
    AgentTools::new(fake.clone())
}
async fn call(tools: &AgentTools, caller: &str, name: &str, input: Value) -> Value {
    call_as(tools, caller, "codex", name, input).await
}
async fn call_as(
    tools: &AgentTools,
    caller: &str,
    instance: &str,
    name: &str,
    input: Value,
) -> Value {
    let result = tools
        .call(
            &ThreadId::new(caller).unwrap(),
            instance,
            "invocation",
            name,
            input,
        )
        .await;
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(
        result["content"][0]["text"].as_str().unwrap(),
        result["structuredContent"].to_string()
    );
    result["structuredContent"].clone()
}
fn code(value: &Value) -> &str {
    assert_eq!(value["_tag"], "OrchestratorMcpFailure", "{value}");
    value["code"].as_str().unwrap()
}

/// A parent with a running app-owned task on a child thread.
fn delegation(fake: &Fake, parent: &str, child: &str, task_id: &str) {
    let mut parent_state = thread_state(parent);
    parent_state
        .tasks
        .push(task(task_id, child, &format!("message:{child}:original")));
    fake.put(parent_state);
    let mut child_state = thread_state(child);
    child_state.thread.as_mut().unwrap().parent = Some(ThreadId::new(parent).unwrap());
    fake.put(child_state);
}

#[tokio::test]
async fn retries_terminal_acknowledgement_with_a_fresh_command_id() {
    let fake = Arc::new(Fake::default());
    delegation(
        &fake,
        "thread:mcp-ack-parent",
        "thread:mcp-ack-child",
        "node:mcp-ack-task",
    );
    fake.edit("thread:mcp-ack-child", |child| {
        let mut original = run("run:mcp-ack-child", 1, RunStatus::Completed, "codex");
        original.message = MessageId::new("message:thread:mcp-ack-child:original").unwrap();
        original.completed_at = Some(at(10));
        let mut continuation = run("run:mcp-ack-continuation", 2, RunStatus::Failed, "codex");
        continuation.started_at = Some(at(2));
        continuation.completed_at = Some(at(5));
        child.runs = vec![original, continuation];
        child
            .tasks
            .push(task("node:nested", "thread:nested", "message:nested"));
    });
    let tools = tools(&fake);
    let pending = call(
        &tools,
        "thread:mcp-ack-parent",
        "task_status",
        json!({"taskId":"node:mcp-ack-task"}),
    )
    .await;
    assert_eq!(pending["status"], "running");
    assert_eq!(pending["workState"], "waiting_for_children");
    assert!(pending["summary"].is_null());
    assert!(fake.commands().is_empty());

    fake.edit("thread:mcp-ack-parent", |parent| {
        parent.tasks[0].status = ItemStatus::Completed;
        parent.tasks[0].result = Some("terminal result".into());
    });
    fake.edit("thread:mcp-ack-child", |child| child.tasks.clear());
    let attempts = Arc::new(Mutex::new(0));
    let counter = attempts.clone();
    fake.on_dispatch(move |_, _| {
        let mut attempts = counter.lock().unwrap();
        *attempts += 1;
        if *attempts == 1 {
            Err("simulated acknowledgement failure".into())
        } else {
            Ok(Reply::Accepted)
        }
    });
    let error = call(
        &tools,
        "thread:mcp-ack-parent",
        "task_status",
        json!({"taskId":"node:mcp-ack-task"}),
    )
    .await;
    assert_eq!(code(&error), "orchestration_error");
    let result = call(
        &tools,
        "thread:mcp-ack-parent",
        "task_status",
        json!({"taskId":"node:mcp-ack-task"}),
    )
    .await;
    assert_eq!(result["status"], "completed");
    assert_eq!(result["summary"], "terminal result");
    assert_eq!(result["latestTerminalRunId"], "run:mcp-ack-child");
    assert_eq!(result["latestTerminalStatus"], "completed");
    let ids = fake.command_ids();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
}

#[tokio::test]
async fn reports_a_restart_cut_child_as_working_until_its_continuation_settles() {
    let fake = Arc::new(Fake::default());
    delegation(
        &fake,
        "thread:mcp-restart-parent",
        "thread:mcp-restart-child",
        "node:mcp-restart-task",
    );
    fake.edit("thread:mcp-restart-child", |child| {
        let mut cut = run("run:mcp-restart-child", 1, RunStatus::Cancelled, "codex");
        cut.message = MessageId::new("message:thread:mcp-restart-child:original").unwrap();
        child.runs = vec![cut];
    });
    fake.unreadable
        .lock()
        .unwrap()
        .insert(ThreadId::new("thread:mcp-restart-child").unwrap());
    let tools = tools(&fake);
    let input = json!({"taskId":"node:mcp-restart-task"});
    let failed = call(
        &tools,
        "thread:mcp-restart-parent",
        "task_status",
        input.clone(),
    )
    .await;
    assert_eq!(code(&failed), "orchestration_error");
    assert!(fake.commands().is_empty());
    fake.unreadable.lock().unwrap().clear();
    let held = call(
        &tools,
        "thread:mcp-restart-parent",
        "task_status",
        input.clone(),
    )
    .await;
    assert_eq!(held["status"], "running");
    assert_eq!(held["workState"], "working");
    assert!(held["summary"].is_null());
    // Acknowledging the cut run would suppress the real result's wake.
    assert!(fake.commands().is_empty());
    fake.edit("thread:mcp-restart-parent", |parent| {
        parent.tasks[0].status = ItemStatus::Cancelled;
        parent.tasks[0].result = Some("Child task ended with status cancelled.".into());
    });
    let settled = call(&tools, "thread:mcp-restart-parent", "task_status", input).await;
    assert_eq!(settled["status"], "cancelled");
    assert_eq!(fake.commands().len(), 1);
}

#[tokio::test]
async fn does_not_dispose_delivery_when_a_nonterminal_task_has_no_active_child_run() {
    let fake = Arc::new(Fake::default());
    delegation(
        &fake,
        "thread:mcp-cancel-parent",
        "thread:mcp-cancel-child",
        "node:mcp-cancel-task",
    );
    let tools = tools(&fake);
    let error = call(
        &tools,
        "thread:mcp-cancel-parent",
        "task_cancel",
        json!({"taskId":"node:mcp-cancel-task","clientRequestId":"cancel-unstarted-task"}),
    )
    .await;
    assert_eq!(code(&error), "task_not_cancellable");
    assert!(fake.commands().is_empty());
}

#[tokio::test]
async fn does_not_dispose_delivery_when_the_child_interrupt_fails() {
    let fake = Arc::new(Fake::default());
    delegation(
        &fake,
        "thread:mcp-cancel-failed-parent",
        "thread:mcp-cancel-failed-child",
        "node:mcp-cancel-failed-task",
    );
    fake.edit("thread:mcp-cancel-failed-child", |child| {
        child.runs.push(run(
            "run:mcp-cancel-failed-child",
            1,
            RunStatus::Running,
            "codex",
        ));
    });
    fake.on_dispatch(|_, _| Err("simulated interrupt failure".into()));
    let tools = tools(&fake);
    let error = call(
        &tools,
        "thread:mcp-cancel-failed-parent",
        "task_cancel",
        json!({"taskId":"node:mcp-cancel-failed-task","clientRequestId":"cancel-failed-task"}),
    )
    .await;
    assert_eq!(code(&error), "task_not_cancellable");
    assert!(matches!(
        fake.commands().as_slice(),
        [Command::Interrupt {
            hold_queue: false,
            ..
        }]
    ));
}

#[tokio::test]
async fn returns_cancel_requested_when_post_interrupt_disposal_fails() {
    let fake = Arc::new(Fake::default());
    delegation(
        &fake,
        "thread:mcp-dispose-parent",
        "thread:mcp-dispose-child",
        "node:mcp-dispose-task",
    );
    fake.edit("thread:mcp-dispose-child", |child| {
        child
            .runs
            .push(run("run:mcp-dispose-child", 1, RunStatus::Running, "codex"));
    });
    fake.on_dispatch(|_, command| match command {
        Command::DisposeTask { .. } => Err("simulated disposal failure".into()),
        _ => Ok(Reply::Accepted),
    });
    let tools = tools(&fake);
    let result = call(
        &tools,
        "thread:mcp-dispose-parent",
        "task_cancel",
        json!({"taskId":"node:mcp-dispose-task","clientRequestId":"cancel-dispose-failed-task","reason":"stop"}),
    )
    .await;
    assert_eq!(result["status"], "cancel_requested");
    let commands = fake.commands();
    assert!(matches!(
        commands.as_slice(),
        [Command::Interrupt { reason: Some(reason), .. }, Command::DisposeTask { .. }] if reason == "stop"
    ));
}

#[tokio::test]
async fn advertises_orchestration_capability_from_registered_adapters() {
    let fake = Arc::new(Fake::default());
    fake.put(active_state("thread:mcp-providers-parent", "codex"));
    let mut fork_only = provider("forkOnly", "forkOnly", None, true);
    fork_only.adapter = false;
    fork_only.unavailable = Some("Driver 'forkOnly' is not registered in this build.".into());
    *fake.providers.lock().unwrap() = vec![
        provider("codex", "codex", Some("gpt-5.4"), true),
        provider(
            "claudeAgent",
            "claudeAgent",
            Some("claude-sonnet-4-6"),
            true,
        ),
        provider("pi", "pi", Some("pi-model"), true),
        provider("acpRegistry", "acpRegistry", Some("acp-model"), true),
        provider("antigravity", "antigravity", Some("ant-model"), true),
        provider("antigravity-alt", "antigravity", Some("ant-model"), false),
        fork_only,
    ];
    let tools = tools(&fake);
    let capabilities = call(
        &tools,
        "thread:mcp-providers-parent",
        "orchestrator_capabilities",
        json!({}),
    )
    .await;
    let by_id = |id: &str| {
        capabilities["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["providerInstanceId"] == id)
            .cloned()
            .unwrap()
    };
    for provider in fake.providers.lock().unwrap().iter() {
        assert_eq!(
            by_id(&provider.instance)["models"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["id"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>(),
            provider
                .models
                .iter()
                .map(|m| m.slug.clone())
                .collect::<Vec<_>>()
        );
    }
    for provider in fake.providers.lock().unwrap().iter_mut() {
        provider.models.push(ProviderModel {
            slug: format!("{}/custom-model-after-refresh", provider.driver),
            name: Some("Custom model".into()),
            options: None,
        });
    }
    let refreshed = call(
        &tools,
        "thread:mcp-providers-parent",
        "orchestrator_capabilities",
        json!({}),
    )
    .await;
    for provider in fake.providers.lock().unwrap().iter() {
        let entry = refreshed["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["providerInstanceId"] == provider.instance.as_str())
            .unwrap();
        assert_eq!(
            entry["models"].as_array().unwrap().len(),
            provider.models.len()
        );
    }
    for instance in ["codex", "claudeAgent", "pi", "acpRegistry", "antigravity"] {
        let entry = by_id(instance);
        assert_eq!(entry["canRunChildTask"], true, "{instance}");
        assert_eq!(entry["canRunCrossProviderChildTask"], true);
        assert_eq!(entry["constraints"], json!([]));
    }
    let disabled = by_id("antigravity-alt");
    assert_eq!(disabled["canRunChildTask"], false);
    assert_eq!(
        disabled["constraints"],
        json!(["Provider instance is disabled."])
    );
    let fork = by_id("forkOnly");
    assert_eq!(fork["canRunChildTask"], false);
    let constraints = fork["constraints"].to_string();
    assert!(constraints.contains("No V2 provider adapter is registered."));
    assert!(constraints.contains("Driver 'forkOnly' is not registered in this build."));
    assert_eq!(capabilities["features"]["batchThreadCreation"], true);
    assert_eq!(capabilities["features"]["maxBatchThreads"], 20);
}

/// The delegate command a call dispatched, if any.
fn delegated(fake: &Fake) -> Option<ModelSelection> {
    fake.commands()
        .into_iter()
        .find_map(|command| match command {
            Command::Delegate { selection, .. } => Some(selection),
            _ => None,
        })
}
/// Accepting a delegation records the task on the parent, as the domain does.
fn accept_delegations(fake: &Arc<Fake>, parent: &'static str) {
    let states = Arc::downgrade(fake);
    fake.on_dispatch(move |_, command| {
        if let (
            Command::Delegate {
                task: id, child, ..
            },
            Some(fake),
        ) = (command, states.upgrade())
        {
            let created = task(id.as_str(), child.as_str(), "message:delegated");
            fake.edit(parent, |state| {
                if !state.tasks.iter().any(|task| &task.id == id) {
                    state.tasks.push(created);
                }
            });
        }
        Ok(Reply::Accepted)
    });
}

#[tokio::test]
async fn delegates_to_an_instance_whose_adapter_resolves_and_by_driver_kind() {
    for target in [
        json!({"providerInstanceId":"claude-two","model":"claude-model"}),
        json!({"driverKind":"claude"}),
    ] {
        let fake = Arc::new(Fake::default());
        fake.put(active_state("thread:mcp-providers-parent", "codex"));
        *fake.providers.lock().unwrap() = vec![
            provider("codex", "codex", Some("gpt-5.4"), true),
            provider("claude-two", "claude", Some("claude-model"), true),
        ];
        accept_delegations(&fake, "thread:mcp-providers-parent");
        let tools = tools(&fake);
        let result = call(
            &tools,
            "thread:mcp-providers-parent",
            "delegate_task",
            json!({"task":"Summarize the diff.","target":target,"mode":"async","clientRequestId":"delegate-1"}),
        )
        .await;
        assert_eq!(result["status"], "running", "{result}");
        let selection = delegated(&fake).unwrap();
        assert_eq!(
            (selection.instance.as_str(), selection.model.as_str()),
            ("claude-two", "claude-model")
        );
    }
}

#[tokio::test]
async fn rejects_delegation_to_a_provider_without_a_registered_adapter() {
    let fake = Arc::new(Fake::default());
    fake.put(active_state("thread:mcp-providers-parent", "codex"));
    let mut fork_only = provider("forkOnly", "forkOnly", None, true);
    fork_only.adapter = false;
    fork_only.unavailable = Some("Driver 'forkOnly' is not registered in this build.".into());
    *fake.providers.lock().unwrap() =
        vec![provider("codex", "codex", Some("gpt-5.4"), true), fork_only];
    let tools = tools(&fake);
    let by_instance = call(
        &tools,
        "thread:mcp-providers-parent",
        "delegate_task",
        json!({"task":"Summarize the diff.","target":{"providerInstanceId":"forkOnly"},"mode":"async","clientRequestId":"delegate-fork-1"}),
    )
    .await;
    assert_eq!(code(&by_instance), "provider_unavailable");
    assert!(
        by_instance["message"]
            .as_str()
            .unwrap()
            .contains("No V2 provider adapter is registered.")
    );
    let by_driver = call(
        &tools,
        "thread:mcp-providers-parent",
        "delegate_task",
        json!({"task":"Summarize the diff.","target":{"driverKind":"forkOnly"},"mode":"async","clientRequestId":"delegate-fork-2"}),
    )
    .await;
    assert_eq!(code(&by_driver), "provider_unavailable");
    assert!(
        by_driver["message"]
            .as_str()
            .unwrap()
            .contains("No V2 provider adapter is registered for driver forkOnly.")
    );
    assert!(fake.commands().is_empty());
}

#[tokio::test]
async fn inherits_an_available_parent_instance_for_driver_targets_and_otherwise_selects_a_healthy_peer()
 {
    // (name, inherited enabled, peer enabled, explicit, selected, peer driver)
    let cases = [
        (
            "healthy-inherited",
            true,
            true,
            false,
            Some("codex"),
            "codex",
        ),
        (
            "unavailable-inherited-falls-back-to-healthy-peer",
            false,
            true,
            false,
            Some("codex-alt"),
            "codex",
        ),
        ("no-available-peer", false, false, false, None, "codex"),
        (
            "cross-driver-no-available-candidate",
            false,
            false,
            false,
            None,
            "claude",
        ),
        ("explicit-unavailable", false, true, true, None, "codex"),
        (
            "explicit-healthy",
            true,
            true,
            true,
            Some("codex-alt"),
            "codex",
        ),
    ];
    for (name, inherited, peer, explicit, selected, peer_driver) in cases {
        let fake = Arc::new(Fake::default());
        let mut parent = active_state("thread:mcp-providers-parent", "codex");
        let parent_selection = ModelSelection {
            options: BTreeMap::from([("reasoningEffort".into(), "high".into())]),
            ..selection("codex", "gpt-5.4")
        };
        parent.thread.as_mut().unwrap().selection = parent_selection.clone();
        fake.put(parent);
        *fake.providers.lock().unwrap() = vec![
            provider("codex", "codex", Some("gpt-5.4"), inherited),
            provider("codex-alt", peer_driver, Some("codex-alt-model"), peer),
        ];
        accept_delegations(&fake, "thread:mcp-providers-parent");
        let tools = tools(&fake);
        let target = if explicit {
            json!({"providerInstanceId": selected.unwrap_or("codex")})
        } else {
            json!({"driverKind": peer_driver})
        };
        let result = call(
            &tools,
            "thread:mcp-providers-parent",
            "delegate_task",
            json!({"task":"Summarize the diff.","target":target,"mode":"async","clientRequestId":format!("delegate-select-{name}")}),
        )
        .await;
        let Some(selected) = selected else {
            assert_eq!(code(&result), "provider_unavailable", "{name}");
            if name == "cross-driver-no-available-candidate" {
                assert!(
                    result["message"]
                        .as_str()
                        .unwrap()
                        .contains("driver claude"),
                    "{name}"
                );
                let threads = call(
                    &tools,
                    "thread:mcp-providers-parent",
                    "create_threads",
                    json!({"threads":[{"prompt":"Summarize the diff.","target":target}],"clientRequestId":format!("delegate-threads-{name}")}),
                )
                .await;
                assert_eq!(
                    code(&threads),
                    "provider_unavailable",
                    "{name}-createThreads"
                );
                assert!(
                    threads["message"]
                        .as_str()
                        .unwrap()
                        .contains("driver claude")
                );
            }
            assert!(fake.commands().is_empty(), "{name}");
            continue;
        };
        assert_eq!(result["status"], "running", "{name}");
        let request = delegated(&fake).unwrap();
        assert_eq!(request.instance, selected, "{name}");
        if name == "healthy-inherited" {
            assert_eq!(request, parent_selection, "{name}");
        } else {
            assert_eq!(request.model, "codex-alt-model", "{name}");
        }
    }
}

#[tokio::test]
async fn read_thread_prefers_the_activity_run_status_over_a_newer_cancelled_queued_run() {
    for status in [RunStatus::Running, RunStatus::Waiting] {
        let fake = Arc::new(Fake::default());
        let mut state = thread_state("thread-mcp-orchestrator-parent");
        state.runs = vec![
            run("run-mcp-active", 1, status, "codex"),
            run("run-mcp-cancelled", 2, RunStatus::Cancelled, "codex"),
        ];
        fake.put(state);
        let tools = tools(&fake);
        let result = call(
            &tools,
            "thread-mcp-orchestrator-parent",
            "thread_read",
            json!({"threadId":"thread-mcp-orchestrator-parent"}),
        )
        .await;
        assert_eq!(result["thread"]["status"], json!(status));
        assert_eq!(result["thread"]["latestRunId"], "run-mcp-cancelled");
        assert_eq!(result["thread"]["activeRunId"], "run-mcp-active");
    }
}

#[tokio::test]
async fn read_thread_reaches_a_thread_the_user_attached_as_context_but_not_one_an_agent_attached() {
    let fake = Arc::new(Fake::default());
    let attaching = |id: &str, by: MessageAuthor, target: &str| {
        let mut attached = message(id, None, Role::User, "[Attached](context://v1/thread/x)");
        attached.created_by = by;
        attached.context = Some(agent_domain::MessageContext {
            version: 1,
            records: vec![agent_domain::Json(json!({
                "version": 1,
                "kind": "thread",
                "contextId": format!("thread_{target}"),
                "label": "Attached",
                "threadId": target,
                "title": "Attached",
            }))],
        });
        attached
    };
    let mut parent = thread_state("thread:parent");
    parent.messages = vec![
        attaching("message:user", MessageAuthor::User, "thread:foreign"),
        attaching("message:agent", MessageAuthor::Agent, "thread:agent-only"),
    ];
    fake.put(parent);
    for id in ["thread:foreign", "thread:agent-only"] {
        let mut foreign = State {
            thread: Some(thread_record(
                id,
                "project:foreign",
                selection("codex", "gpt-5.4"),
            )),
            ..State::default()
        };
        foreign.messages.push(message(
            "assistant:1",
            None,
            Role::Assistant,
            "Foreign thread said hello",
        ));
        foreign.items.push(item(
            "item:1",
            None,
            0,
            ItemKind::AssistantMessage {
                message: MessageId::new("assistant:1").unwrap(),
            },
            "Foreign thread said hello",
        ));
        fake.put(foreign);
    }
    let tools = tools(&fake);
    let attached = call(
        &tools,
        "thread:parent",
        "thread_read",
        json!({"threadId":"thread:foreign"}),
    )
    .await;
    assert_eq!(attached["thread"]["threadId"], "thread:foreign");
    assert_eq!(
        attached["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["Foreign thread said hello"]
    );
    let denied = call(
        &tools,
        "thread:parent",
        "thread_read",
        json!({"threadId":"thread:agent-only"}),
    )
    .await;
    assert_eq!(code(&denied), "thread_not_found");
    let write = call(
        &tools,
        "thread:parent",
        "thread_send",
        json!({"threadId":"thread:foreign","message":"hi"}),
    )
    .await;
    assert_eq!(code(&write), "thread_not_found");
}

#[tokio::test]
async fn task_status_returns_the_tasks_provider_instance_rather_than_the_driver_kind() {
    let fake = Arc::new(Fake::default());
    delegation(
        &fake,
        "thread-mcp-orchestrator-parent",
        "thread-mcp-orchestrator-child",
        "node-mcp-task-1",
    );
    fake.edit("thread-mcp-orchestrator-child", |child| {
        let mut original = run(
            "run-mcp-child",
            1,
            RunStatus::Running,
            "codex-custom-workspace",
        );
        original.message =
            MessageId::new("message:thread-mcp-orchestrator-child:original").unwrap();
        child.runs.push(original);
    });
    let tools = tools(&fake);
    let result = call(
        &tools,
        "thread-mcp-orchestrator-parent",
        "task_status",
        json!({"taskId":"node-mcp-task-1"}),
    )
    .await;
    assert_eq!(result["providerInstanceId"], "codex-custom-workspace");
    assert_eq!(result["status"], "running");
    assert_eq!(result["taskId"], "node-mcp-task-1");
    assert_eq!(result["childThreadId"], "thread-mcp-orchestrator-child");
}

#[tokio::test]
async fn thread_update_reports_an_absent_or_unreadable_calling_thread() {
    let fake = Arc::new(Fake::default());
    let tools = tools(&fake);
    let input = json!({"action":"rename","title":"Renamed thread","clientRequestId":"metadata-caller-classification"});
    let absent = call(
        &tools,
        "thread:metadata-caller",
        "thread_update",
        input.clone(),
    )
    .await;
    assert_eq!(code(&absent), "thread_not_found");
    fake.unreadable
        .lock()
        .unwrap()
        .insert(ThreadId::new("thread:metadata-caller").unwrap());
    let unreadable = call(&tools, "thread:metadata-caller", "thread_update", input).await;
    assert_eq!(code(&unreadable), "orchestration_error");
    assert!(fake.commands().is_empty());
}

#[tokio::test]
async fn thread_update_validates_each_action_and_links_pull_requests() {
    let fake = Arc::new(Fake::default());
    fake.put(thread_state("thread:caller"));
    let tools = tools(&fake);
    for invalid in [
        json!({"action":"rename"}),
        json!({"action":"rename","title":"x","pullRequest":{"repository":"a/b","number":1,"url":"https://x/1"}}),
        json!({"action":"link_pull_request","title":"x"}),
        json!({"action":"regenerate_title","title":"x"}),
        json!({"action":"link_pull_request","pullRequest":{"repository":"a/b","number":1,"url":"ftp://x/1"}}),
    ] {
        let result = call(&tools, "thread:caller", "thread_update", invalid.clone()).await;
        assert_eq!(result["_tag"], "AiError", "{invalid}");
        assert_eq!(result["reason"]["_tag"], "ToolParameterValidationError");
    }
    let linked = call(
        &tools,
        "thread:caller",
        "thread_update",
        json!({"action":"link_pull_request","pullRequest":{"repository":"pingdotgg/widget","number":8689,"url":"https://github.com/pingdotgg/widget/pull/8689"},"clientRequestId":"link"}),
    )
    .await;
    assert_eq!(linked["action"], "link_pull_request");
    assert_eq!(linked["threadId"], "thread:caller");
    assert!(
        linked["commandId"]
            .as_str()
            .unwrap()
            .contains(":thread-update:thread%3Acaller:link_pull_request:link")
    );
    let Some(Command::UpdateMetadata {
        linked_pull_request: Some(Some(pr)),
        ..
    }) = fake.commands().pop()
    else {
        panic!("a link updates the metadata");
    };
    assert_eq!((pr.project.as_str(), pr.number), ("project", 8689));
}

#[test]
fn publishes_unique_tool_names_with_reference_free_object_root_inputs() {
    let mut names = HashSet::new();
    for tool in super::tools() {
        let name = tool["name"].as_str().unwrap().to_owned();
        assert!(names.insert(name.clone()), "{name}");
        assert_eq!(tool["inputSchema"]["type"], "object", "{name}");
        assert!(
            !tool["inputSchema"].to_string().contains("\"$ref\""),
            "{name}"
        );
        assert!(!tool["description"].as_str().unwrap().is_empty());
    }
    assert!(names.contains("thread_launch"));
    assert!(!names.contains("thread_start"));
}

#[test]
fn orchestrator_tool_descriptions_separate_delegation_from_threads() {
    let tools = super::tools();
    let find = |name: &str| {
        tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap()
            .clone()
    };
    let delegate = find("delegate_task")["description"]
        .as_str()
        .unwrap()
        .to_owned();
    let create = find("create_threads")["description"]
        .as_str()
        .unwrap()
        .to_owned();
    for text in [
        "child agent/subagent",
        "cross-provider",
        "waitTimedOut",
        "does not cancel the child",
        "keep that taskId",
        "call delegate_task again",
        "childThreadId is backing storage",
    ] {
        assert!(delegate.contains(text), "{text}");
    }
    assert!(create.contains("not delegation"));
    assert!(create.contains("call delegate_task"));
    assert!(
        find("thread_send")["description"]
            .as_str()
            .unwrap()
            .contains("Do not use a delegated task's childThreadId to start another review round")
    );
    assert!(
        find("task_cancel")["description"]
            .as_str()
            .unwrap()
            .contains("without interrupting later child-thread runs")
    );
    let schema = &find("delegate_task")["inputSchema"]["properties"];
    assert!(
        schema["mode"]["description"]
            .as_str()
            .unwrap()
            .contains("Defaults to async")
    );
    assert!(
        schema["timeoutMs"]["description"]
            .as_str()
            .unwrap()
            .contains("does not cancel the child")
    );
    let update = find("thread_update");
    assert_eq!(update["inputSchema"]["type"], "object");
    let mut keys: Vec<_> = update["inputSchema"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "action",
            "clientRequestId",
            "pullRequest",
            "threadId",
            "title"
        ]
    );
    assert!(
        update["description"]
            .as_str()
            .unwrap()
            .contains("Workspace and branch changes")
    );
    let capabilities = find("orchestrator_capabilities");
    assert_eq!(
        capabilities["annotations"]["title"],
        "Get orchestration capabilities"
    );
    assert_eq!(capabilities["annotations"]["readOnlyHint"], true);
    assert_eq!(capabilities["annotations"]["idempotentHint"], true);
    assert_eq!(
        find("delegate_task")["annotations"]["destructiveHint"],
        true
    );
    assert_eq!(find("thread_read")["annotations"]["readOnlyHint"], false);
}

#[test]
fn a_read_only_claude_sandbox_pre_approves_only_read_only_served_tools() {
    let read_only = read_only_tools();
    for tool in ["orchestrator_capabilities", "thread_list", "queue_read"] {
        assert!(read_only.iter().any(|name| name == tool), "{tool}");
    }
    for tool in ["thread_send", "delegate_task", "project_create"] {
        assert!(!read_only.iter().any(|name| name == tool), "{tool}");
    }
}

/// Every snake_case name the provider instructions and tool descriptions
/// mention is a served tool, a provider-native tool or a known non-tool term.
#[test]
fn instructions_and_descriptions_name_only_served_tools() {
    use agent_domain::InteractionMode;
    let mut served: HashSet<String> = super::tools()
        .into_iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect();
    served.insert(
        crate::browser::mcp::tool()["name"]
            .as_str()
            .unwrap()
            .to_owned(),
    );
    let codex_native = ["request_user_input", "update_plan"];
    let tags = ["runtime_info", "collaboration_mode", "proposed_plan"];
    let values = [
        "existing_worktree",
        "link_pull_request",
        "unlink_pull_request",
        "regenerate_title",
        "waiting_for_children",
        "result_available",
    ];
    let mut texts = vec![
        agent_providers::ORCHESTRATION_INSTRUCTIONS.to_owned(),
        agent_providers::BROWSER_TOOL_INSTRUCTIONS.to_owned(),
        agent_providers::claude_append_system_prompt(true),
        agent_providers::codex_additional_context("gpt-5.4", "high", true).to_string(),
        agent_providers::codex_developer_instructions(InteractionMode::Plan).to_owned(),
        agent_providers::codex_developer_instructions(InteractionMode::Default).to_owned(),
    ];
    fn descriptions(value: &Value, texts: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    match (key.as_str(), value) {
                        ("description", Value::String(text)) => texts.push(text.clone()),
                        _ => descriptions(value, texts),
                    }
                }
            }
            Value::Array(values) => values.iter().for_each(|value| descriptions(value, texts)),
            _ => {}
        }
    }
    for tool in super::tools() {
        descriptions(&tool, &mut texts);
    }
    let names = regex::Regex::new(r"\b[a-z][a-z0-9]*(?:_[a-z0-9]+)+\b").unwrap();
    let keys = [
        "orchestration_instructions",
        "runtime_instructions",
        "browser_instructions",
    ];
    let mut mentioned = HashSet::new();
    for text in &texts {
        let text = text.replace("mcp__orchestration__", "");
        for name in names.find_iter(&text) {
            mentioned.insert(name.as_str().to_owned());
        }
    }
    for name in &mentioned {
        assert!(
            served.contains(name)
                || codex_native.contains(&name.as_str())
                || tags.contains(&name.as_str())
                || values.contains(&name.as_str())
                || keys.contains(&name.as_str()),
            "{name} is not a served tool"
        );
    }
    for tool in ["thread_launch", "delegate_task", "bex_browser"] {
        assert!(mentioned.contains(tool), "{tool}");
    }
}
#[tokio::test]
async fn returns_a_bounded_public_failure_without_serializing_storage_causes() {
    let fake = Arc::new(Fake::default());
    fake.unreadable
        .lock()
        .unwrap()
        .insert(ThreadId::new("mcp-core-thread").unwrap());
    let tools = tools(&fake);
    let result = call(
        &tools,
        "mcp-core-thread",
        "thread_organize",
        json!({"action":"pin"}),
    )
    .await;
    assert_eq!(
        result,
        json!({"_tag":"OrchestratorMcpFailure","code":"orchestration_error","message":"The operation could not be completed."})
    );
}

#[tokio::test]
async fn launches_threads_from_a_full_access_caller_and_scratch_threads_into_chats() {
    let fake = Arc::new(Fake::default());
    fake.put(active_state("source-thread", "codex"));
    let tools = tools(&fake);
    let launched = call(
        &tools,
        "source-thread",
        "thread_launch",
        json!({"title":"Audit","message":"Review the change"}),
    )
    .await;
    assert_eq!(launched["projectId"], "project");
    assert_eq!(
        launched["modelSelection"],
        json!({"instanceId":"codex","model":"gpt-5.4"})
    );
    assert_eq!(launched["status"], "preparing");
    let scratch = call(
        &tools,
        "source-thread",
        "thread_launch",
        json!({"title":"Notes","scratch":true,"message":"Draft a list"}),
    )
    .await;
    assert_eq!(
        scratch["projectId"],
        crate::conversation::operations::CHATS_PROJECT
    );
    {
        let launches = fake.launches.lock().unwrap();
        assert_eq!(launches.len(), 2);
        assert_eq!(
            launches[1].workspace,
            agent_runtime::WorkspaceStrategy::Root { branch: None }
        );
        let first = &launches[0];
        assert_eq!(
            (first.created_by, first.creation_source.as_str()),
            (MessageAuthor::Agent, "mcp")
        );
        let message = first.initial_message.as_ref().unwrap();
        assert_eq!(
            (message.created_by, message.creation_source.as_str()),
            (MessageAuthor::Agent, "mcp")
        );
    }
    let rejected = call(
        &tools,
        "source-thread",
        "thread_launch",
        json!({"title":"Notes","scratch":true,"projectId":"project"}),
    )
    .await;
    assert_eq!(code(&rejected), "invalid_request");
    assert_eq!(fake.launches.lock().unwrap().len(), 2);
    fake.edit("source-thread", |state| {
        state.thread.as_mut().unwrap().runtime_mode = RuntimeMode::AutoAcceptEdits;
    });
    let denied = call(
        &tools,
        "source-thread",
        "thread_launch",
        json!({"title":"Audit"}),
    )
    .await;
    assert_eq!(code(&denied), "capability_denied");
}

#[tokio::test]
async fn creates_projects_from_a_path_and_rejects_fields_it_cannot_apply() {
    let fake = Arc::new(Fake::default());
    fake.put(active_state("source-thread", "codex"));
    let tools = tools(&fake);
    let created = call(
        &tools,
        "source-thread",
        "project_create",
        json!({"title":"Existing","workspaceRoot":"/work/existing"}),
    )
    .await;
    assert_eq!(created["workspaceRoot"], "/work/existing");
    assert_eq!(created["title"], "Existing");
    assert_eq!(created["scripts"], json!([]));
    assert_eq!(fake.created.lock().unwrap().len(), 1);
    let script = |command: &str| json!({"id":" setup ","name":"Setup","command":command,"icon":"configure","runOnWorktreeCreate":true});
    let scripted = call(
        &tools,
        "source-thread",
        "project_create",
        json!({"title":"Scripted","workspaceRoot":"/work/scripted","scripts":[script(" vp install ")]}),
    )
    .await;
    assert_eq!(scripted["scripts"][0]["id"], "setup");
    assert_eq!(scripted["scripts"][0]["command"], "vp install");
    fake.projects.lock().unwrap().push(HostProject {
        id: "project:Scripted".into(),
        name: "Scripted".into(),
        root: "/work/scripted".into(),
    });
    let read = call(
        &tools,
        "source-thread",
        "project_read",
        json!({"projectId":"project:Scripted"}),
    )
    .await;
    assert_eq!(read["scripts"], scripted["scripts"]);
    let blank = call(
        &tools,
        "source-thread",
        "project_create",
        json!({"title":"Blank","workspaceRoot":"/work/blank","scripts":[script(" ")]}),
    )
    .await;
    assert_eq!(blank["_tag"], "AiError");
    let unkept = call(
        &tools,
        "source-thread",
        "project_create",
        json!({"title":"Modelled","workspaceRoot":"/work/modelled","defaultModelSelection":{"instanceId":"codex","model":"gpt-5"}}),
    )
    .await;
    assert_eq!(code(&unkept), "invalid_request");
    assert_eq!(fake.created.lock().unwrap().len(), 2);
    for extra in [
        json!({"scripts":[]}),
        json!({"defaultModelSelection":{"instanceId":"codex","model":"gpt-5"}}),
    ] {
        let mut input = json!({"title":"Configured"});
        input
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let rejected = call(&tools, "source-thread", "project_create", input).await;
        assert_eq!(code(&rejected), "invalid_request");
    }
    assert_eq!(fake.created.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn interrupt_picks_the_newest_interruptible_run_and_reports_statuses() {
    let fake = Arc::new(Fake::default());
    let mut state = thread_state("thread:target");
    state.runs = vec![
        run("run:done", 1, RunStatus::Completed, "codex"),
        run("run:active", 2, RunStatus::Running, "codex"),
        run("run:queued", 3, RunStatus::Queued, "codex"),
    ];
    fake.put(state);
    fake.put(thread_state("thread:caller"));
    let tools = tools(&fake);
    let interrupt = |input: Value| {
        let tools = &tools;
        async move { call(tools, "thread:caller", "thread_interrupt", input).await }
    };
    let requested = interrupt(json!({"threadId":"thread:target","reason":"user asked"})).await;
    assert_eq!(
        requested,
        json!({"threadId":"thread:target","runId":"run:active","status":"interrupt_requested"})
    );
    assert!(matches!(
        fake.commands().as_slice(),
        [Command::Interrupt { run, hold_queue: false, reason: Some(reason) }] if run.as_str() == "run:active" && reason == "user asked"
    ));
    let terminal = interrupt(json!({"threadId":"thread:target","runId":"run:done"})).await;
    assert_eq!(
        terminal,
        json!({"threadId":"thread:target","runId":"run:done","status":"completed"})
    );
    let queued = interrupt(json!({"threadId":"thread:target","runId":"run:queued"})).await;
    assert_eq!(code(&queued), "thread_not_interruptible");
    let missing = interrupt(json!({"threadId":"thread:target","runId":"run:missing"})).await;
    assert_eq!(code(&missing), "run_not_found");
    fake.edit("thread:target", |state| {
        state.runs[1].status = RunStatus::Interrupted
    });
    let idle = interrupt(json!({"threadId":"thread:target"})).await;
    assert_eq!(
        idle,
        json!({"threadId":"thread:target","runId":null,"status":"no_active_run"})
    );
    let not_active = interrupt(json!({"threadId":"thread:target","runId":"run:queued"})).await;
    assert_eq!(code(&not_active), "thread_not_interruptible");
    assert_eq!(fake.commands().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn wait_selects_the_latest_run_clamps_its_budget_and_reports_timeouts() {
    assert_eq!(
        super::orchestrator::wait_budget(None),
        Duration::from_secs(600)
    );
    assert_eq!(
        super::orchestrator::wait_budget(Some(-5.0)),
        Duration::from_millis(1)
    );
    assert_eq!(
        super::orchestrator::wait_budget(Some(1e12)),
        Duration::from_secs(3600)
    );
    let fake = Arc::new(Fake::default());
    fake.put(thread_state("thread:caller"));
    let tools = tools(&fake);
    let idle = call(
        &tools,
        "thread:caller",
        "thread_wait",
        json!({"threadId":"thread:caller"}),
    )
    .await;
    assert_eq!(
        idle,
        json!({"threadId":"thread:caller","runId":null,"status":"idle","timedOut":false})
    );
    fake.edit("thread:caller", |state| {
        state
            .runs
            .push(run("run:one", 1, RunStatus::Running, "codex"))
    });
    let timed_out = call(
        &tools,
        "thread:caller",
        "thread_wait",
        json!({"threadId":"thread:caller","timeoutMs":1000}),
    )
    .await;
    assert_eq!(
        timed_out,
        json!({"threadId":"thread:caller","runId":"run:one","status":"running","timedOut":true})
    );
    let missing = call(
        &tools,
        "thread:caller",
        "thread_wait",
        json!({"threadId":"thread:caller","runId":"run:none"}),
    )
    .await;
    assert_eq!(code(&missing), "run_not_found");
    let finisher = fake.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(600)).await;
        finisher.edit("thread:caller", |state| {
            state.runs[0].status = RunStatus::Completed
        });
    });
    let finished = call(
        &tools,
        "thread:caller",
        "thread_wait",
        json!({"threadId":"thread:caller"}),
    )
    .await;
    assert_eq!(
        finished,
        json!({"threadId":"thread:caller","runId":"run:one","status":"completed","timedOut":false})
    );
}

#[tokio::test]
async fn read_defaults_to_fifty_messages_of_twenty_thousand_units_and_pages_long_text() {
    let fake = Arc::new(Fake::default());
    let mut state = thread_state("thread:caller");
    state
        .runs
        .push(run("run:1", 1, RunStatus::Completed, "codex"));
    for index in 0..60u64 {
        let id = format!("message:{index}");
        state.messages.push(message(
            &id,
            Some("run:1"),
            Role::User,
            &format!("hello {index}"),
        ));
        state.items.push(item(
            &format!("item:{index}"),
            Some("run:1"),
            index,
            ItemKind::UserMessage {
                message: MessageId::new(&id).unwrap(),
            },
            "",
        ));
    }
    state.messages[0].text = "😀".repeat(15_000);
    fake.put(state);
    let tools = tools(&fake);
    let read = call(
        &tools,
        "thread:caller",
        "thread_read",
        json!({"threadId":"thread:caller"}),
    )
    .await;
    let items = read["items"].as_array().unwrap();
    assert_eq!(items.len(), 50);
    assert_eq!(read["hasMore"], true);
    assert_eq!(read["nextPosition"], 49);
    assert_eq!(items[0]["type"], "user_message");
    assert_eq!(items[0]["visibility"], "local");
    assert_eq!(items[0]["createdBy"], "user");
    assert_eq!(items[0]["textTruncated"], true);
    assert_eq!(items[0]["nextTextOffset"], 20_000);
    assert_eq!(
        items[0]["text"].as_str().unwrap().encode_utf16().count(),
        20_000
    );
    assert_eq!(read["thread"]["itemCount"], 60);
    assert_eq!(read["thread"]["createdBy"], "user");
    assert_eq!(read["recentRuns"][0]["runId"], "run:1");
    let rest = call(
        &tools,
        "thread:caller",
        "thread_read",
        json!({"threadId":"thread:caller","itemId":"item:0","textOffset":20_000}),
    )
    .await;
    assert_eq!(
        rest["items"][0]["text"]
            .as_str()
            .unwrap()
            .encode_utf16()
            .count(),
        10_000
    );
    assert_eq!(rest["items"][0]["textTruncated"], false);
    assert!(rest["items"][0]["nextTextOffset"].is_null());
    let next = call(
        &tools,
        "thread:caller",
        "thread_read",
        json!({"threadId":"thread:caller","afterPosition":49}),
    )
    .await;
    assert_eq!(next["items"].as_array().unwrap().len(), 10);
    assert_eq!(next["hasMore"], false);
}

#[tokio::test]
async fn archived_callers_read_but_cannot_change_threads() {
    let fake = Arc::new(Fake::default());
    let mut caller = active_state("thread:caller", "codex");
    caller.thread.as_mut().unwrap().archived_at = Some(at(3));
    fake.put(caller);
    let tools = tools(&fake);
    let read = call(
        &tools,
        "thread:caller",
        "thread_read",
        json!({"threadId":"thread:caller"}),
    )
    .await;
    assert_eq!(read["thread"]["archived"], true);
    let listed = call(&tools, "thread:caller", "thread_list", json!({})).await;
    assert_eq!(listed["total"], 1);
    let organize = call(
        &tools,
        "thread:caller",
        "thread_organize",
        json!({"action":"pin"}),
    )
    .await;
    assert_eq!(code(&organize), "parent_not_active");
    let send = call(
        &tools,
        "thread:caller",
        "thread_send",
        json!({"threadId":"thread:caller","message":"hi"}),
    )
    .await;
    assert_eq!(code(&send), "thread_not_sendable");
    assert!(fake.commands().is_empty());
}

#[tokio::test]
async fn list_orders_newest_first_and_filters_status_settlement_title_and_subagents() {
    let fake = Arc::new(Fake::default());
    let mut caller = thread_state("thread:a");
    caller.thread.as_mut().unwrap().title = "Alpha".into();
    fake.put(caller);
    let mut newer = thread_state("thread:b");
    {
        let thread = newer.thread.as_mut().unwrap();
        thread.title = "Beta work".into();
        thread.updated_at = at(30);
        thread.settled = Some(true);
        thread.settled_at = Some(at(31));
    }
    newer
        .runs
        .push(run("run:b", 1, RunStatus::Completed, "codex"));
    fake.put(newer);
    let mut child = thread_state("thread:c");
    {
        let thread = child.thread.as_mut().unwrap();
        thread.title = "Child".into();
        thread.updated_at = at(20);
        thread.parent = Some(ThreadId::new("thread:a").unwrap());
        thread.created_by = MessageAuthor::Agent;
        thread.creation_source = "mcp".into();
    }
    fake.put(child);
    let mut other = thread_state("thread:other");
    other.thread.as_mut().unwrap().project = "project:other".into();
    fake.put(other);
    let tools = tools(&fake);
    let all = call(&tools, "thread:a", "thread_list", json!({"limit":2})).await;
    assert_eq!(all["projectId"], "project");
    assert_eq!(all["currentThreadId"], "thread:a");
    assert_eq!(all["total"], 3);
    assert_eq!(all["nextCursor"], 2);
    assert_eq!(all["threads"][0]["threadId"], "thread:b");
    assert_eq!(all["threads"][0]["settled"], true);
    assert_eq!(all["threads"][0]["settledAt"], json!(at(31)));
    assert_eq!(all["threads"][1]["relationshipToParent"], "subagent");
    assert_eq!(all["threads"][1]["createdBy"], "agent");
    assert_eq!(all["threads"][1]["creationSource"], "mcp");
    let completed = call(
        &tools,
        "thread:a",
        "thread_list",
        json!({"statuses":["completed"]}),
    )
    .await;
    assert_eq!(completed["total"], 1);
    let idle = call(
        &tools,
        "thread:a",
        "thread_list",
        json!({"statuses":["idle"],"includeSubagents":false}),
    )
    .await;
    assert_eq!(idle["threads"].as_array().unwrap().len(), 1);
    assert_eq!(idle["threads"][0]["threadId"], "thread:a");
    let active = call(
        &tools,
        "thread:a",
        "thread_list",
        json!({"settled":false,"titleContains":"  CHI "}),
    )
    .await;
    assert_eq!(active["total"], 1);
    assert_eq!(active["threads"][0]["threadId"], "thread:c");
    assert!(active["nextCursor"].is_null());
}

#[tokio::test]
async fn queue_tools_page_read_and_change_queued_messages() {
    let fake = Arc::new(Fake::default());
    let mut state = active_state("thread:caller", "codex");
    for (index, text) in ["first", &"x".repeat(1_200)].iter().enumerate() {
        let mut queued = run(
            &format!("run:q{index}"),
            index as u64 + 2,
            RunStatus::Queued,
            "codex",
        );
        queued.queue_position = Some(index as u64 + 1);
        state.messages.push(message(
            queued.message.as_str(),
            Some(queued.id.as_str()),
            Role::User,
            text,
        ));
        state.runs.push(queued);
    }
    fake.put(state);
    let tools = tools(&fake);
    let first = call(&tools, "thread:caller", "queue_list", json!({"limit":1})).await;
    assert_eq!(
        first,
        json!({"items":[{"queuedRunId":"run:q0","text":"first","truncated":false}],"nextCursor":1})
    );
    let second = call(
        &tools,
        "thread:caller",
        "queue_list",
        json!({"cursor":1,"limit":1}),
    )
    .await;
    assert_eq!(second["items"][0]["truncated"], true);
    assert_eq!(second["items"][0]["text"].as_str().unwrap().len(), 1_000);
    assert!(second["nextCursor"].is_null());
    let read = call(
        &tools,
        "thread:caller",
        "queue_read",
        json!({"queuedRunId":"run:q1"}),
    )
    .await;
    assert_eq!(read["truncated"], false);
    let missing = call(
        &tools,
        "thread:caller",
        "queue_read",
        json!({"queuedRunId":"run:thread:caller:active"}),
    )
    .await;
    assert_eq!(code(&missing), "invalid_request");
    let edited = call(
        &tools,
        "thread:caller",
        "queue_edit",
        json!({"queuedRunId":"run:q0","text":"changed"}),
    )
    .await;
    assert!(edited["sequence"].is_u64());
    call(
        &tools,
        "thread:caller",
        "queue_reorder",
        json!({"queuedRunId":"run:q1","beforeRunId":"run:q0"}),
    )
    .await;
    call(
        &tools,
        "thread:caller",
        "queue_reorder",
        json!({"queuedRunId":"run:q1","beforeRunId":null}),
    )
    .await;
    call(
        &tools,
        "thread:caller",
        "queue_cancel",
        json!({"queuedRunId":"run:q1"}),
    )
    .await;
    call(
        &tools,
        "thread:caller",
        "queue_promote_to_steer",
        json!({"queuedRunId":"run:q0","targetRunId":"run:thread:caller:active"}),
    )
    .await;
    let missing_before = call(
        &tools,
        "thread:caller",
        "queue_reorder",
        json!({"queuedRunId":"run:q1"}),
    )
    .await;
    assert_eq!(missing_before["_tag"], "AiError");
    assert!(matches!(
        fake.commands().as_slice(),
        [
            Command::EditQueued { text, attachments: None, .. },
            Command::ReorderQueued { before: Some(_), .. },
            Command::ReorderQueued { before: None, .. },
            Command::CancelQueued { .. },
            Command::PromoteToSteer { .. },
        ] if text == "changed"
    ));
}

#[tokio::test]
async fn pending_request_tools_answer_only_open_user_questions() {
    let fake = Arc::new(Fake::default());
    let mut state = active_state("thread:caller", "codex");
    let request = |id: &str, body: RequestBody, status: RequestStatus| Request {
        owner_path: vec![],
        id: RuntimeRequestId::new(id).unwrap(),
        attempt: RunAttemptId::new("attempt:x").unwrap(),
        native_key: id.into(),
        body,
        capability: ResponseCapability::Live,
        status,
        decision: None,
        answers: None,
        attachments: BTreeMap::new(),
        created_at: at(0),
        resolved_at: None,
    };
    let questions = RequestBody::Questions {
        questions: vec![Question {
            required: true,
            id: "scope".into(),
            header: "Scope".into(),
            question: "Which scope?".into(),
            multiple: false,
            options: vec![QuestionOption {
                label: "All".into(),
                description: Some("Everything".into()),
            }],
        }],
    };
    state.requests = vec![
        request(
            "request:question",
            questions.clone(),
            RequestStatus::Pending,
        ),
        request("request:answered", questions, RequestStatus::Resolved),
        request(
            "request:approval",
            RequestBody::Approval {
                kind: "command".into(),
                title: "Run".into(),
                detail: None,
                options: vec![],
                input: agent_domain::Json(json!({})),
            },
            RequestStatus::Pending,
        ),
    ];
    state.items.push(item(
        "item:question",
        None,
        1,
        ItemKind::UserInputRequest {
            request: RuntimeRequestId::new("request:question").unwrap(),
        },
        "",
    ));
    fake.put(state);
    let tools = tools(&fake);
    let list = call(&tools, "thread:caller", "pending_request_list", json!({})).await;
    assert_eq!(list, json!({"requestIds":["request:question"]}));
    let read = call(
        &tools,
        "thread:caller",
        "pending_request_read",
        json!({"requestId":"request:question"}),
    )
    .await;
    assert_eq!(
        read["questions"][0]["options"][0],
        json!({"label":"All","description":"Everything"})
    );
    for other in ["request:answered", "request:approval"] {
        let missing = call(
            &tools,
            "thread:caller",
            "pending_request_respond",
            json!({"requestId":other,"answers":{"scope":"All"}}),
        )
        .await;
        assert_eq!(code(&missing), "invalid_request");
    }
    call(
        &tools,
        "thread:caller",
        "pending_request_respond",
        json!({"requestId":"request:question","answers":{"scope":["All"]}}),
    )
    .await;
    let Some(Command::Respond {
        answers: Some(answers),
        decision: None,
        ..
    }) = fake.commands().pop()
    else {
        panic!("the answer is dispatched");
    };
    assert_eq!(
        answers["scope"],
        agent_domain::Answer::Choices(vec!["All".into()])
    );
}

#[tokio::test]
async fn organize_maps_each_action_and_requires_snooze_time() {
    let fake = Arc::new(Fake::default());
    fake.put(active_state("thread:caller", "codex"));
    let tools = tools(&fake);
    for action in [
        "pin",
        "unpin",
        "unsnooze",
        "settle",
        "unsettle",
        "archive",
        "unarchive",
        "mark_unread",
    ] {
        let result = call(
            &tools,
            "thread:caller",
            "thread_organize",
            json!({"action":action}),
        )
        .await;
        assert!(result["sequence"].is_u64(), "{action}");
    }
    let snooze = call(
        &tools,
        "thread:caller",
        "thread_organize",
        json!({"action":"snooze"}),
    )
    .await;
    assert_eq!(code(&snooze), "invalid_request");
    call(
        &tools,
        "thread:caller",
        "thread_organize",
        json!({"action":"snooze","snoozedUntil":"2026-10-04T00:00:00.000Z"}),
    )
    .await;
    let other_instance = call_as(
        &tools,
        "thread:caller",
        "claude",
        "thread_organize",
        json!({"action":"pin"}),
    )
    .await;
    assert_eq!(code(&other_instance), "parent_not_active");
    assert!(matches!(
        fake.commands().as_slice(),
        [
            Command::Pin { pinned: true, .. },
            Command::Pin { pinned: false, .. },
            Command::Snooze { until: None },
            Command::Settle { settled: true, .. },
            Command::Settle { settled: false, .. },
            Command::Archive { archived: true },
            Command::Archive { archived: false },
            Command::MarkUnread,
            Command::Snooze { until: Some(_) },
        ]
    ));
}

#[tokio::test]
async fn search_returns_only_matches_of_the_calling_project() {
    let fake = Arc::new(Fake::default());
    fake.put(thread_state("thread:caller"));
    let tools = tools(&fake);
    let result = call(
        &tools,
        "thread:caller",
        "thread_search",
        json!({"query":"needle"}),
    )
    .await;
    assert_eq!(result["matches"].as_array().unwrap().len(), 1);
    assert_eq!(result["matches"][0]["threadId"], "thread:found");
    assert_eq!(result["matches"][0]["source"], "user");
    let short = call(
        &tools,
        "thread:caller",
        "thread_search",
        json!({"query":" n "}),
    )
    .await;
    assert_eq!(short["_tag"], "AiError");
}

#[tokio::test]
async fn send_maps_modes_and_rejects_escalation() {
    let fake = Arc::new(Fake::default());
    let mut caller = active_state("thread:caller", "codex");
    caller.thread.as_mut().unwrap().runtime_mode = RuntimeMode::AutoAcceptEdits;
    fake.put(caller);
    fake.put(thread_state("thread:target"));
    let tools = tools(&fake);
    let escalation = call(
        &tools,
        "thread:caller",
        "thread_send",
        json!({"threadId":"thread:target","message":"hi"}),
    )
    .await;
    assert_eq!(code(&escalation), "runtime_mode_escalation_denied");
    fake.edit("thread:target", |state| {
        state.thread.as_mut().unwrap().runtime_mode = RuntimeMode::ApprovalRequired
    });
    let steer = call(
        &tools,
        "thread:caller",
        "thread_send",
        json!({"threadId":"thread:target","message":"hi","mode":"steer"}),
    )
    .await;
    assert_eq!(code(&steer), "thread_not_sendable");
    assert_eq!(
        steer["message"],
        "Thread thread:target has no running turn that can be steered."
    );
    let elsewhere = call(
        &tools,
        "thread:caller",
        "thread_send",
        json!({"threadId":"thread:missing","message":"hi"}),
    )
    .await;
    assert_eq!(code(&elsewhere), "thread_not_found");
    let durable = call(
        &tools,
        "thread:caller",
        "thread_send",
        json!({"threadId":"thread:target","message":" hi ","clientRequestId":"send-1"}),
    )
    .await;
    assert_eq!(code(&durable), "orchestration_error");
    assert!(matches!(
        fake.commands().as_slice(),
        [Command::Send(message)] if message.mode == agent_domain::DispatchMode::StartImmediately && message.text == "hi" && message.created_by == MessageAuthor::Agent && message.creation_source == "mcp"
    ));
}

#[tokio::test]
async fn transfers_and_configuration_read_the_addressed_thread() {
    let fake = Arc::new(Fake::default());
    let mut state = active_state("thread:caller", "codex");
    state.thread.as_mut().unwrap().selection.options =
        BTreeMap::from([("reasoningEffort".into(), "high".into())]);
    fake.put(state);
    *fake.providers.lock().unwrap() = vec![
        provider("codex", "codex", Some("gpt-5.4"), true),
        provider("claude", "claude", Some("claude-sonnet"), true),
    ];
    let tools = tools(&fake);
    let configuration = call(&tools, "thread:caller", "thread_configuration", json!({})).await;
    assert_eq!(
        configuration,
        json!({"threadId":"thread:caller","modelSelection":{"instanceId":"codex","model":"gpt-5.4","options":[{"id":"reasoningEffort","value":"high"}]},"runtimeMode":"full-access","interactionMode":"default"})
    );
    call(
        &tools,
        "thread:caller",
        "thread_configure",
        json!({"modelSelection":{"instanceId":"codex","model":"gpt-5.5"}}),
    )
    .await;
    call(
        &tools,
        "thread:caller",
        "thread_configure",
        json!({"modelSelection":{"instanceId":"claude","model":"claude-sonnet"}}),
    )
    .await;
    assert!(matches!(
        fake.commands().as_slice(),
        [Command::SelectModel { .. }, Command::SwitchProvider { selection }] if selection.driver == Driver::Claude
    ));
    let transfers = call(&tools, "thread:caller", "thread_transfers", json!({})).await;
    assert_eq!(transfers, json!({"transfers":[]}));
}

#[tokio::test]
async fn the_bridge_authenticates_its_scope_and_revoked_tokens_stop_working() {
    let listener = ToolBridge::bind().unwrap();
    let thread = ThreadId::new("parent").unwrap();
    let config = listener.provider_config(&thread, "codex").unwrap();
    assert_eq!(listener.provider_config(&thread, "codex").unwrap(), config);
    let token = config["env"][TOKEN_ENV].as_str().unwrap().to_owned();
    listener.serve(Weak::new()).unwrap();
    let request = |token: String| BridgeRequest {
        token,
        invocation: "read".into(),
        name: "thread_read".into(),
        arguments: json!({"threadId":"parent"}),
    };
    assert_eq!(
        bridge(listener.address(), request("invalid".into())).await,
        Err("Invalid orchestration scope".into())
    );
    listener.revoke(&thread, Some("codex"));
    assert_eq!(
        bridge(listener.address(), request(token)).await,
        Err("Invalid orchestration scope".into())
    );
}

#[test]
fn stable_ids_scope_retries_to_the_session_and_encode_keys() {
    let parent = ThreadId::new("parent").unwrap();
    let other = ThreadId::new("other").unwrap();
    let scope = Scope {
        thread: &parent,
        instance: "codex",
    };
    let elsewhere = Scope {
        thread: &other,
        instance: "codex",
    };
    assert_eq!(
        stable_command(scope, "delegate-task", "round one", None),
        stable_command(scope, "delegate-task", "round one", None)
    );
    assert_ne!(
        stable_command(scope, "delegate-task", "round one", None),
        stable_command(elsewhere, "delegate-task", "round one", None)
    );
    let id = stable_command(scope, "create-thread", "a:b", Some(2));
    assert!(id.as_str().starts_with("command:mcp:"));
    assert!(id.as_str().ends_with(":create-thread:a%3Ab:2"));
}
