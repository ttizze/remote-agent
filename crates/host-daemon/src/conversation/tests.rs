//! The Host's conversation RPCs over a real store in a temporary directory, with
//! provider processes replaying recorded Codex transcripts.
use super::operations::CHATS_PROJECT;
use super::sessions::ProcessSpec;
use super::*;
use crate::{HostRpcService, ProjectStore, SessionId};
use agent_domain::{
    Command, CommandId, Driver, InteractionMode, ItemKind, ModelSelection, Reply, RunStatus,
    RuntimeMode, State, ThreadId, apply,
};
use agent_protocol::{
    conversation as wire,
    conversation::{ConversationError, ErrorCode},
    error::RpcFailure,
    models::Model,
    protocol::{self, Call, Response},
};
use agent_runtime::{ProcessControl, ProviderProcess};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const SIMPLE: &str =
    include_str!("../../../agent-providers/src/fixtures/simple/codex_transcript.ndjson");

/// One recorded request and the frames the provider answered with.
#[derive(Clone)]
struct Exchange {
    method: String,
    id: Option<Value>,
    replies: Vec<Value>,
}
fn exchanges(transcript: &str) -> Vec<Exchange> {
    let mut exchanges: Vec<Exchange> = vec![];
    for line in transcript.lines() {
        let record: Value = serde_json::from_str(line).unwrap();
        match record["type"].as_str() {
            Some("expect_outbound") => exchanges.push(Exchange {
                method: record["frame"]["method"]
                    .as_str()
                    .unwrap_or_default()
                    .into(),
                id: record["frame"].get("id").cloned(),
                replies: vec![],
            }),
            Some("emit_inbound") => exchanges
                .last_mut()
                .expect("a reply follows a request")
                .replies
                .push(record["frame"].clone()),
            _ => {}
        }
    }
    exchanges
}

struct Exit(tokio::sync::watch::Receiver<bool>);
impl ProcessControl for Exit {
    fn wait(&mut self) -> futures_util::future::BoxFuture<'_, bool> {
        Box::pin(async move {
            let _ = self.0.wait_for(|exited| *exited).await;
            true
        })
    }
    fn kill(&mut self) -> futures_util::future::BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// Codex processes that answer each request with the recorded replies, in order.
struct ReplaySpawner {
    transcript: &'static str,
    spawned: Mutex<Vec<ProcessSpec>>,
}
impl Spawner for ReplaySpawner {
    fn spawn(&self, spec: ProcessSpec) -> std::io::Result<ProviderProcess> {
        assert_eq!(spec.driver, Driver::Codex);
        self.spawned.lock().unwrap().push(spec);
        let (input, provider_input) = tokio::io::duplex(1 << 20);
        let (provider_output, output) = tokio::io::duplex(1 << 20);
        let (exited, exit) = tokio::sync::watch::channel(false);
        let mut script = exchanges(self.transcript).into_iter();
        tokio::spawn(async move {
            let mut lines = BufReader::new(provider_input).lines();
            let mut output = provider_output;
            while let Ok(Some(line)) = lines.next_line().await {
                let request: Value = serde_json::from_str(&line).unwrap();
                let Some(method) = request["method"].as_str() else {
                    continue;
                };
                let Some(exchange) = script.by_ref().find(|exchange| exchange.method == method)
                else {
                    continue;
                };
                for mut reply in exchange.replies {
                    if reply.get("method").is_none() && reply.get("id") == exchange.id.as_ref() {
                        reply["id"] = request["id"].clone();
                    }
                    let mut bytes = serde_json::to_vec(&reply).unwrap();
                    bytes.push(b'\n');
                    if output.write_all(&bytes).await.is_err() {
                        return;
                    }
                }
            }
            let _ = exited.send(true);
        });
        Ok(ProviderProcess {
            input: Box::new(input),
            output: Box::new(output),
            stderr: Box::new(tokio::io::empty()),
            control: Box::new(Exit(exit)),
        })
    }
}

struct NoModels;
impl tools::ModelCatalog for NoModels {
    fn models(&self) -> futures_util::future::BoxFuture<'_, Result<Vec<Model>, String>> {
        Box::pin(async { Ok(vec![]) })
    }
}

struct Host {
    _directory: tempfile::TempDir,
    project_root: std::path::PathBuf,
    project: String,
    service: HostRpcService,
    conversation: Arc<Conversation>,
    spawner: Arc<ReplaySpawner>,
    session: crate::HostSession,
}

fn git(cwd: &std::path::Path, args: &[&str]) {
    crate::git::text(cwd, args).unwrap();
}

async fn host() -> Host {
    let spawner = Arc::new(ReplaySpawner {
        transcript: SIMPLE,
        spawned: Mutex::new(vec![]),
    });
    host_with(spawner.clone(), spawner).await
}

async fn host_with(codex: Arc<dyn Spawner>, spawner: Arc<ReplaySpawner>) -> Host {
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    let project_root = root.join("project");
    std::fs::create_dir(&project_root).unwrap();
    git(&project_root, &["init", "--quiet"]);
    std::fs::write(project_root.join("README.md"), "# project\n").unwrap();
    git(&project_root, &["add", "."]);
    git(
        &project_root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "initial",
        ],
    );
    let state = root.join("state");
    std::fs::create_dir(&state).unwrap();
    let projects = ProjectStore::new(state.join("worktrees.json"));
    let project = projects.register(&project_root).await.unwrap();
    let service = HostRpcService::new(Err("fixture".into()), projects).unwrap();
    let mut runtime = agent_runtime::RuntimeConfig::new(state.join("conversation.sqlite"));
    runtime.eviction = None;
    let conversation = Conversation::open(
        ConversationConfig {
            runtime,
            programs: ProviderPrograms {
                codex: Some("/fixture/codex".into()),
                codex_home: None,
                codex_accounts: None,
                claude: None,
            },
            spawner: codex,
            browser: Arc::new(|_| None),
            models: Arc::new(NoModels),
        },
        service.shared(),
    )
    .await
    .unwrap();
    service.install_conversation(conversation.clone());
    service.start().await.unwrap();
    let session = service.open_session();
    Host {
        _directory: directory,
        project_root,
        project,
        service,
        conversation,
        spawner,
        session,
    }
}

impl Host {
    fn id(&self) -> SessionId {
        self.session.id()
    }
    async fn reply(&self, call: Call) -> crate::host_rpc::connections::HostReply {
        self.service.dispatch(self.id(), &call).await.unwrap()
    }
    async fn call<T: DeserializeOwned>(&self, call: Call) -> Result<T, RpcFailure> {
        match protocol::decode::<Response<T>>(&self.reply(call).await.initial).unwrap() {
            Response::Success { result } => Ok(result),
            Response::Failure { error } => Err(error),
        }
    }
}

fn selection() -> ModelSelection {
    ModelSelection {
        instance: "codex".into(),
        driver: Driver::Codex,
        model: "gpt-6-luna".into(),
        options: BTreeMap::new(),
    }
}
fn command(id: &str) -> CommandId {
    CommandId::new(id).unwrap()
}
fn code(failure: &RpcFailure) -> Option<ErrorCode> {
    ErrorCode::of(failure)
}
fn launch(host: &Host, command_id: &str, text: &str) -> Call {
    Call::Launch(Box::new(wire::Launch {
        command_id: command(command_id),
        thread_id: None,
        project_id: host.project.clone(),
        title: "New thread".into(),
        selection: selection(),
        runtime_mode: RuntimeMode::FullAccess,
        interaction_mode: InteractionMode::Default,
        workspace: wire::WorkspaceStrategy::Root { branch: None },
        message: Some(wire::LaunchMessage {
            context: None,
            id: None,
            text: text.into(),
            attachments: vec![],
            creation_source: "desktop".into(),
            title_seed: None,
        }),
    }))
}

/// Folds a thread stream into the client's projection.
struct Folded {
    state: State,
    sequence: u64,
    synchronized: bool,
}
impl Folded {
    fn update(&mut self, update: wire::ThreadUpdate) {
        match update {
            wire::ThreadUpdate::Snapshot(snapshot) => {
                self.state = State::clone(&snapshot.state);
                self.sequence = snapshot.snapshot_sequence;
            }
            wire::ThreadUpdate::Facts(facts) => {
                for fact in facts {
                    assert!(fact.sequence > self.sequence, "facts arrive once, in order");
                    apply(&mut self.state, &fact.fact).unwrap();
                    self.sequence = fact.sequence;
                }
            }
            wire::ThreadUpdate::Synchronized => self.synchronized = true,
            wire::ThreadUpdate::Failed(error) => panic!("{error:?}"),
        }
    }
}

async fn subscribe(
    host: &Host,
    thread: &ThreadId,
    after: Option<u64>,
) -> (Folded, crate::host_rpc::connections::HostSubscription) {
    let reply = host
        .reply(Call::ThreadStream(wire::SubscribeThread {
            thread_id: thread.clone(),
            after_sequence: after,
            request_completion_marker: true,
            accept_bounded_snapshot: false,
        }))
        .await;
    let Response::Success { result } =
        protocol::decode::<Response<wire::ThreadUpdate>>(&reply.initial).unwrap()
    else {
        panic!("subscription failed");
    };
    let mut folded = Folded {
        state: State::default(),
        sequence: after.unwrap_or(0),
        synchronized: false,
    };
    folded.update(result);
    (folded, reply.updates.expect("the stream stays open"))
}

async fn until(
    folded: &mut Folded,
    updates: &mut crate::host_rpc::connections::HostSubscription,
    done: impl Fn(&State) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !done(&folded.state) {
            let frame = updates.recv().await.expect("the stream stays open");
            folded.update(protocol::decode(&frame).unwrap());
        }
    })
    .await
    .expect("the thread reaches the expected state");
}

fn answered(state: &State) -> bool {
    state
        .runs
        .first()
        .is_some_and(|run| run.status == RunStatus::Completed)
        && state
            .messages
            .iter()
            .any(|message| message.text == "fixture simple ok")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_launched_thread_streams_its_turn_and_reads_back_through_every_query() {
    let host = host().await;
    let launched: wire::Launched = host
        .call(launch(
            &host,
            "launch-1",
            "Respond with the following text: fixture simple ok",
        ))
        .await
        .unwrap();
    assert!(!launched.resumed);
    assert!(matches!(launched.committed.reply, Reply::Run(_)));
    let thread = launched.thread_id.clone();

    let (mut folded, mut updates) = subscribe(&host, &thread, None).await;
    until(&mut folded, &mut updates, answered).await;
    assert!(folded.state.thread.as_ref().unwrap().workspace.is_some());
    let spawned = host.spawner.spawned.lock().unwrap().clone();
    assert_eq!(spawned.len(), 1);
    assert_eq!(spawned[0].args, ["app-server", "--listen", "stdio://"]);
    assert_eq!(spawned[0].cwd, host.project_root);

    // A resend of the launch resumes it without a second thread or turn.
    let resent: wire::Launched = host
        .call(launch(
            &host,
            "launch-1",
            "Respond with the following text: fixture simple ok",
        ))
        .await
        .unwrap();
    assert_eq!(resent.thread_id, thread);
    assert!(resent.resumed);

    let snapshot: wire::ThreadSnapshot = host
        .call(Call::GetThread(wire::GetThread {
            thread_id: thread.clone(),
            bounded: false,
        }))
        .await
        .unwrap();
    assert!(answered(&snapshot.state));
    assert!(snapshot.snapshot_sequence >= folded.sequence);

    let page: wire::HistoryPage = host
        .call(Call::ReadHistory(wire::ReadHistory {
            thread_id: thread.clone(),
            cursor: None,
        }))
        .await
        .unwrap();
    let assistant = page
        .rows
        .iter()
        .find(|row| matches!(row.item.kind, ItemKind::AssistantMessage { .. }))
        .expect("the answer is in the history");
    let item: Option<wire::HistoryRow> = host
        .call(Call::TurnItem(wire::GetTurnItem {
            thread_id: thread.clone(),
            item_id: assistant.item.id.clone(),
        }))
        .await
        .unwrap();
    assert_eq!(item.as_ref(), Some(assistant));

    let found: Vec<wire::SearchMatch> = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let found: Vec<wire::SearchMatch> = host
                .call(Call::Search(wire::Search {
                    query: "fixture simple".into(),
                    limit: None,
                }))
                .await
                .unwrap();
            if !found.is_empty() {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(found[0].thread_id, thread);
    assert_eq!(found[0].project_id, host.project);

    // The turn's checkpoint against the workspace before it: nothing changed.
    let diff = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match host
                .call::<wire::TurnDiff>(Call::TurnDiff(wire::GetTurnDiff {
                    thread_id: thread.clone(),
                    from_run_ordinal: 0,
                    to_run_ordinal: 1,
                    ignore_whitespace: Some(false),
                }))
                .await
            {
                Ok(diff) => return diff,
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(diff.diff, "");
    host.conversation.shutdown().await;
}

#[tokio::test]
async fn a_stream_resumed_at_its_head_answers_without_replaying() {
    let host = host().await;
    let thread = ThreadId::new("thread:resumed").unwrap();
    let created: wire::Committed = host
        .call(Call::Dispatch(Box::new(wire::Dispatch {
            thread_id: thread.clone(),
            command_id: command("create"),
            command: Command::Create {
                thread: thread.clone(),
                project: CHATS_PROJECT.into(),
                title: "Chat".into(),
                selection: selection(),
                runtime_mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Default,
                workspace: None,
                created_by: agent_domain::MessageAuthor::User,
                creation_source: "desktop".into(),
            },
        })))
        .await
        .unwrap();
    let (resumed, _updates) = subscribe(&host, &thread, Some(created.sequence)).await;
    assert!(resumed.synchronized);
    assert_eq!(resumed.sequence, created.sequence);
    let quiet: wire::ThreadUpdate = host
        .call(Call::ThreadStream(wire::SubscribeThread {
            thread_id: thread.clone(),
            after_sequence: Some(created.sequence),
            request_completion_marker: false,
            accept_bounded_snapshot: true,
        }))
        .await
        .unwrap();
    assert_eq!(quiet, wire::ThreadUpdate::Facts(vec![]));
    let (replayed, _updates) = subscribe(&host, &thread, Some(0)).await;
    assert_eq!(replayed.sequence, created.sequence);
    assert_eq!(replayed.state.thread.unwrap().title, "Chat");
}

// ThreadLaunchService.test.ts "runs a Scratch thread launched at the root in its own
// folder" against the Host's chats folder.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chat_launched_at_the_root_runs_in_a_folder_of_its_own() {
    let host = host().await;
    let chats = host.project_root.parent().unwrap().join("state/chats");
    let mut folders = vec![];
    for (id, text) in [
        ("chat-1", "Convert these PNGs"),
        ("chat-2", "Convert these PNGs"),
    ] {
        let mut call = launch(&host, id, text);
        if let Call::Launch(launch) = &mut call {
            launch.project_id = CHATS_PROJECT.into();
        }
        let launched: wire::Launched = host.call(call).await.unwrap();
        let snapshot: wire::ThreadSnapshot = host
            .call(Call::GetThread(wire::GetThread {
                thread_id: launched.thread_id,
                bounded: false,
            }))
            .await
            .unwrap();
        let workspace = snapshot.state.thread.clone().unwrap().workspace.unwrap();
        let folder = std::path::PathBuf::from(workspace.worktree_path.unwrap());
        assert_eq!(workspace.cwd, folder.to_string_lossy());
        assert_eq!(folder.parent(), Some(chats.as_path()));
        assert!(folder.is_dir());
        let name = folder.file_name().unwrap().to_str().unwrap().to_owned();
        let (date, words) = name.split_at(10);
        assert!(
            date.bytes()
                .enumerate()
                .all(|(index, byte)| if index == 4 || index == 7 {
                    byte == b'-'
                } else {
                    byte.is_ascii_digit()
                }),
            "{name}"
        );
        let id = words.strip_prefix("-convert-these-pngs-").unwrap();
        assert!(
            id.len() == 8 && id.bytes().all(|byte| byte.is_ascii_alphanumeric()),
            "{name}"
        );
        folders.push(folder);
    }
    assert_ne!(folders[0], folders[1]);
    host.conversation.shutdown().await;
}

// ThreadLaunchService.ts and ProjectSetupScriptRunner.ts: a worktree launch runs the
// project's setup script in the new worktree; a failing one the agent waits for
// fails the preparation with its exit code.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_worktree_launch_runs_the_projects_setup_script() {
    use agent_protocol::models::{Empty, Project, ProjectScript, ProjectScriptIcon};
    let host = host().await;
    let set_setup = |command: &str| {
        Call::UpdateProject(agent_protocol::operations::UpdateProject {
            project_id: host.project.clone(),
            scripts: Some(vec![ProjectScript {
                id: " setup ".into(),
                name: "Setup".into(),
                command: command.into(),
                icon: ProjectScriptIcon::Configure,
                run_on_worktree_create: true,
                run_async: Some(false),
                preview_url: None,
                auto_open_preview: None,
            }]),
        })
    };
    let launch_in_worktree = |id: &str| {
        let mut call = launch(&host, id, "Set up");
        if let Call::Launch(launch) = &mut call {
            launch.workspace = wire::WorkspaceStrategy::Worktree {
                base_ref: "HEAD".into(),
                branch: None,
                start_from_origin: false,
            };
        }
        call
    };
    let prepared = |state: &State| {
        state
            .runs
            .first()
            .is_some_and(|run| run.status != RunStatus::Preparing)
    };

    let _: Empty = host
        .call(set_setup("touch \"$WORKTREE_PATH/setup-ran\""))
        .await
        .unwrap();
    let projects: Vec<Project> = host.call(Call::ListProjects(Empty {})).await.unwrap();
    assert_eq!(projects[0].scripts[0].id, "setup");
    let shell = host
        .reply(Call::ShellStream(wire::SubscribeShell {
            after_sequence: None,
            request_completion_marker: false,
            location: wire::ShellLocation::Active,
        }))
        .await;
    let Response::Success {
        result: wire::ShellUpdate::Snapshot(snapshot),
    } = protocol::decode::<Response<wire::ShellUpdate>>(&shell.initial).unwrap()
    else {
        panic!("a snapshot opens the shell stream");
    };
    assert_eq!(snapshot.projects[0].scripts, projects[0].scripts);
    let launched: wire::Launched = host.call(launch_in_worktree("setup-ok")).await.unwrap();
    let (mut folded, mut updates) = subscribe(&host, &launched.thread_id, None).await;
    until(&mut folded, &mut updates, prepared).await;
    let workspace = folded
        .state
        .thread
        .as_ref()
        .unwrap()
        .workspace
        .clone()
        .unwrap();
    assert_ne!(std::path::Path::new(&workspace.cwd), host.project_root);
    assert!(
        std::path::Path::new(&workspace.cwd)
            .join("setup-ran")
            .is_file()
    );

    let _: Empty = host.call(set_setup("exit 1")).await.unwrap();
    let launched: wire::Launched = host.call(launch_in_worktree("setup-fail")).await.unwrap();
    let (mut folded, mut updates) = subscribe(&host, &launched.thread_id, None).await;
    until(&mut folded, &mut updates, prepared).await;
    assert_eq!(folded.state.runs[0].status, RunStatus::Failed);
    let error = folded.state.items.iter().find_map(|item| match &item.kind {
        ItemKind::Error { message, .. } => Some(message.clone()),
        _ => None,
    });
    assert_eq!(
        error.as_deref(),
        Some("Workspace preparation failed during run setup script: Setup script exited with 1.")
    );
    // The setup card keeps the outcome for late subscribers.
    let card = host
        .reply(Call::SetupStream(wire::SubscribeSetup {
            thread_id: launched.thread_id.clone(),
        }))
        .await;
    let Response::Success { result: Some(card) } =
        protocol::decode::<Response<Option<agent_domain::WorktreeSetupSnapshot>>>(&card.initial)
            .unwrap()
    else {
        panic!("the failed setup is still on its card");
    };
    assert_eq!(card.phase, agent_domain::WorktreeSetupPhase::Failed);
    assert_eq!(card.setup_script.unwrap().command, "exit 1");
    let cancelled: wire::SetupCancelled = host
        .call(Call::CancelSetup(wire::CancelSetup {
            thread_id: launched.thread_id.clone(),
        }))
        .await
        .unwrap();
    assert!(!cancelled.cancelled);
    host.conversation.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_cut_by_shutdown_is_settled_when_the_host_starts_again() {
    let unfinished: &'static str = SIMPLE
        .lines()
        .take_while(|line| !line.contains("\"label\":\"turn/completed\""))
        .collect::<Vec<_>>()
        .join("\n")
        .leak();
    let host = host().await;
    let replay = Arc::new(ReplaySpawner {
        transcript: unfinished,
        spawned: Mutex::new(vec![]),
    });
    let state = host.project_root.parent().unwrap().join("state");
    let open = |spawner: Arc<ReplaySpawner>| {
        let mut runtime = agent_runtime::RuntimeConfig::new(state.join("cut.sqlite"));
        runtime.eviction = None;
        Conversation::open(
            ConversationConfig {
                runtime,
                programs: ProviderPrograms {
                    codex: Some("/fixture/codex".into()),
                    codex_home: None,
                    codex_accounts: None,
                    claude: None,
                },
                spawner,
                browser: Arc::new(|_| None),
                models: Arc::new(NoModels),
            },
            host.service.shared(),
        )
    };
    let first = open(replay.clone()).await.unwrap();
    first.start().await.unwrap();
    let launched = first
        .launch(&match launch(
            &host,
            "cut",
            "Respond with the following text: fixture simple ok",
        ) {
            Call::Launch(launch) => *launch,
            _ => unreachable!(),
        })
        .await
        .unwrap();
    let thread = launched.thread_id;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let state = first.runtime.state(&thread).await.unwrap().state;
            if state.messages.iter().any(|m| m.text == "fixture simple ok") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the turn streams its answer");
    first.shutdown().await;
    drop(first);

    let second = open(replay).await.unwrap();
    second.start().await.unwrap();
    let state = second.runtime.state(&thread).await.unwrap().state;
    let run = &state.runs[0];
    assert!(!run.status.blocking(), "{:?}", run.status);
    assert_eq!(run.status, RunStatus::Cancelled);
    second.shutdown().await;
}

#[tokio::test]
async fn the_shell_stream_lists_projects_and_threads_then_follows_changes() {
    let host = host().await;
    let reply = host
        .reply(Call::ShellStream(wire::SubscribeShell {
            after_sequence: None,
            request_completion_marker: true,
            location: wire::ShellLocation::Active,
        }))
        .await;
    let Response::Success {
        result: wire::ShellUpdate::Snapshot(snapshot),
    } = protocol::decode::<Response<wire::ShellUpdate>>(&reply.initial).unwrap()
    else {
        panic!("a snapshot opens the shell stream");
    };
    let mut updates = reply.updates.unwrap();
    assert!(snapshot.threads.is_empty());
    let ids: Vec<_> = snapshot.projects.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, [host.project.as_str(), CHATS_PROJECT]);
    assert!(matches!(
        protocol::decode::<wire::ShellUpdate>(&updates.recv().await.unwrap()).unwrap(),
        wire::ShellUpdate::Synchronized
    ));

    let thread = ThreadId::new("thread:created").unwrap();
    let created: wire::Committed = host
        .call(Call::Dispatch(Box::new(wire::Dispatch {
            thread_id: thread.clone(),
            command_id: command("create"),
            command: Command::Create {
                thread: thread.clone(),
                project: CHATS_PROJECT.into(),
                title: "Chat".into(),
                selection: selection(),
                runtime_mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Default,
                workspace: None,
                created_by: agent_domain::MessageAuthor::User,
                creation_source: "desktop".into(),
            },
        })))
        .await
        .unwrap();
    assert_eq!(created.reply, Reply::Thread(thread.clone()));
    let update = tokio::time::timeout(Duration::from_secs(5), updates.recv())
        .await
        .unwrap()
        .unwrap();
    let wire::ShellUpdate::ThreadUpdated {
        sequence,
        thread: row,
    } = protocol::decode::<wire::ShellUpdate>(&update).unwrap()
    else {
        panic!("the new thread is listed");
    };
    assert_eq!(sequence, created.sequence);
    assert_eq!((row.id, row.title.as_str()), (thread, "Chat"));
}

#[tokio::test]
async fn conversation_calls_answer_with_typed_errors() {
    let host = host().await;
    let missing = ThreadId::new("thread:missing").unwrap();
    let dispatch = |thread: &ThreadId, id: &str, command| {
        Call::Dispatch(Box::new(wire::Dispatch {
            thread_id: thread.clone(),
            command_id: self::command(id),
            command,
        }))
    };

    let internal = host
        .call::<wire::Committed>(dispatch(
            &missing,
            "internal",
            Command::AcceptTaskWake { task_ids: vec![] },
        ))
        .await
        .unwrap_err();
    assert_eq!(code(&internal), Some(ErrorCode::InternalCommand));

    let unknown = host
        .call::<wire::Committed>(dispatch(
            &missing,
            "rename",
            Command::Rename {
                title: "Renamed".into(),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(code(&unknown), Some(ErrorCode::ThreadNotFound));

    let unregistered = host
        .call::<wire::Committed>(dispatch(
            &missing,
            "create-elsewhere",
            Command::Create {
                thread: missing.clone(),
                project: "unregistered".into(),
                title: "Elsewhere".into(),
                selection: selection(),
                runtime_mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Default,
                workspace: None,
                created_by: agent_domain::MessageAuthor::User,
                creation_source: "desktop".into(),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(code(&unregistered), Some(ErrorCode::ProjectNotFound));

    let first = ThreadId::new("thread:first").unwrap();
    let create = |thread: &ThreadId| Command::Create {
        thread: thread.clone(),
        project: CHATS_PROJECT.into(),
        title: "Chat".into(),
        selection: selection(),
        runtime_mode: RuntimeMode::FullAccess,
        interaction_mode: InteractionMode::Default,
        workspace: None,
        created_by: agent_domain::MessageAuthor::User,
        creation_source: "desktop".into(),
    };
    host.call::<wire::Committed>(dispatch(&first, "shared-id", create(&first)))
        .await
        .unwrap();
    let replayed = host
        .call::<wire::Committed>(dispatch(&first, "shared-id", create(&first)))
        .await
        .unwrap();
    assert!(replayed.replayed);
    let second = ThreadId::new("thread:second").unwrap();
    let conflict = host
        .call::<wire::Committed>(dispatch(&second, "shared-id", create(&second)))
        .await
        .unwrap_err();
    assert_eq!(code(&conflict), Some(ErrorCode::CommandIdConflict));

    let mut wrong_project = launch(&host, "shared-id", "hello");
    if let Call::Launch(launch) = &mut wrong_project {
        launch.thread_id = Some(first.clone());
    }
    let launch_conflict = host
        .call::<wire::Launched>(wrong_project)
        .await
        .unwrap_err();
    assert_eq!(code(&launch_conflict), Some(ErrorCode::CommandIdConflict));

    for call in [
        Call::GetThread(wire::GetThread {
            thread_id: missing.clone(),
            bounded: true,
        }),
        Call::ThreadStream(wire::SubscribeThread {
            thread_id: missing.clone(),
            after_sequence: None,
            request_completion_marker: false,
            accept_bounded_snapshot: false,
        }),
        // The typed missing-thread contract.
        Call::TurnDiff(wire::GetTurnDiff {
            thread_id: missing.clone(),
            from_run_ordinal: 0,
            to_run_ordinal: 1,
            ignore_whitespace: None,
        }),
    ] {
        let Response::Failure { error } =
            protocol::decode::<Response<()>>(&host.reply(call).await.initial).unwrap()
        else {
            panic!("an unknown thread fails");
        };
        assert_eq!(code(&error), Some(ErrorCode::ThreadNotFound));
    }

    let unavailable = host
        .call::<wire::TurnDiff>(Call::TurnDiff(wire::GetTurnDiff {
            thread_id: first.clone(),
            from_run_ordinal: 0,
            to_run_ordinal: 1,
            ignore_whitespace: None,
        }))
        .await
        .unwrap_err();
    assert_eq!(code(&unavailable), Some(ErrorCode::CheckpointUnavailable));
    let same = host
        .call::<wire::TurnDiff>(Call::TurnDiff(wire::GetTurnDiff {
            thread_id: missing,
            from_run_ordinal: 2,
            to_run_ordinal: 2,
            ignore_whitespace: None,
        }))
        .await
        .unwrap();
    assert_eq!(same.diff, "");

    let search = host
        .call::<Vec<wire::SearchMatch>>(Call::Search(wire::Search {
            query: "x".into(),
            limit: None,
        }))
        .await
        .unwrap_err();
    assert_eq!(code(&search), Some(ErrorCode::InvalidSearch));
    let cursor = host
        .call::<wire::HistoryPage>(Call::ReadHistory(wire::ReadHistory {
            thread_id: first,
            cursor: Some("not a cursor".into()),
        }))
        .await
        .unwrap_err();
    assert_eq!(code(&cursor), Some(ErrorCode::InvalidCursor));

    let missing_upload = host
        .call::<wire::Launched>(Call::Launch(Box::new(wire::Launch {
            message: Some(wire::LaunchMessage {
                context: None,
                id: None,
                text: "with a file".into(),
                attachments: vec![agent_domain::Attachment {
                    kind: agent_domain::AttachmentKind::File,
                    source: None,
                    id: "pending:missing".into(),
                    name: "notes.md".into(),
                    mime_type: "text/markdown".into(),
                    path: "/etc/passwd".into(),
                    size: 4,
                }],
                creation_source: "desktop".into(),
                title_seed: None,
            }),
            ..match launch(&host, "with-file", "") {
                Call::Launch(launch) => *launch,
                _ => unreachable!(),
            }
        })))
        .await
        .unwrap_err();
    assert_eq!(
        code(&missing_upload),
        Some(ErrorCode::AttachmentUnavailable)
    );

    let error: ConversationError = ConversationError::ProjectNotFound("p".into());
    assert_eq!(error.code(), ErrorCode::ProjectNotFound);
}

async fn tool(host: &Host, thread: &ThreadId, name: &str, arguments: Value) -> Value {
    let result = host
        .conversation
        .tools
        .call(thread, "codex", "invocation", name, arguments)
        .await;
    assert_eq!(result["isError"], false, "{result}");
    result["structuredContent"].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_tools_read_a_thread_of_their_own_project() {
    let host = host().await;
    let launched: wire::Launched = host
        .call(launch(
            &host,
            "tools",
            "Respond with the following text: fixture simple ok",
        ))
        .await
        .unwrap();
    let thread = launched.thread_id;
    let (mut folded, mut updates) = subscribe(&host, &thread, None).await;
    until(&mut folded, &mut updates, answered).await;
    let read = tool(
        &host,
        &thread,
        "thread_read",
        json!({"threadId": thread, "limit": 1}),
    )
    .await;
    assert_eq!(read["thread"]["status"], json!(RunStatus::Completed));
    assert_eq!(read["thread"]["createdBy"], "user");
    assert_eq!(read["thread"]["creationSource"], "desktop");
    assert_eq!(read["items"].as_array().unwrap().len(), 1);
    assert_eq!(read["items"][0]["type"], "user_message");
    let next = tool(
        &host,
        &thread,
        "thread_read",
        json!({"threadId": thread, "afterPosition": read["nextPosition"]}),
    )
    .await;
    assert_eq!(next["items"][0]["text"], "fixture simple ok");
    assert_eq!(next["items"][0]["type"], "assistant_message");
    let elsewhere = tool(
        &host,
        &thread,
        "thread_read",
        json!({"threadId": "thread:elsewhere"}),
    )
    .await;
    assert_eq!(elsewhere["code"], "thread_not_found");
    // Without an active turn there is nothing to delegate from.
    let delegated = tool(&host, &thread, "delegate_task", json!({"task": "help"})).await;
    assert_eq!(
        delegated,
        json!({"_tag":"OrchestratorMcpFailure","code":"parent_not_active","message":"Delegated tasks require an active run owned by this MCP provider session."})
    );
    host.conversation.shutdown().await;
}

/// A Codex app-server that holds turns whose input contains `[hold]` until they
/// are interrupted, and answers every other turn at once.
struct HeldCodex;
impl Spawner for HeldCodex {
    fn spawn(&self, spec: ProcessSpec) -> std::io::Result<ProviderProcess> {
        assert_eq!(spec.driver, Driver::Codex);
        let (input, provider_input) = tokio::io::duplex(1 << 20);
        let (provider_output, output) = tokio::io::duplex(1 << 20);
        let (exited, exit) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            let mut lines = BufReader::new(provider_input).lines();
            let mut output = provider_output;
            let mut next = 0u64;
            let mut active: BTreeMap<String, String> = BTreeMap::new();
            while let Ok(Some(line)) = lines.next_line().await {
                let request: Value = serde_json::from_str(&line).unwrap();
                let (Some(id), Some(method)) = (request.get("id"), request["method"].as_str())
                else {
                    continue;
                };
                let params = &request["params"];
                let thread = params["threadId"].as_str().unwrap_or_default().to_owned();
                let mut frames = vec![];
                match method {
                    "thread/start" => {
                        next += 1;
                        frames.push(json!({"id": id, "result": {"thread": {"id": format!("native-{next}")}}}));
                    }
                    "thread/resume" => {
                        frames.push(json!({"id": id, "result": {"thread": {"id": thread}}}))
                    }
                    "turn/start" => {
                        next += 1;
                        let turn = format!("turn-{next}");
                        let text = params["input"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|part| part["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n");
                        let started =
                            json!({"id": turn, "items": [], "status": "inProgress", "error": null});
                        frames.push(json!({"id": id, "result": {"turn": started}}));
                        frames.push(json!({"method": "turn/started", "params": {"threadId": thread, "turn": started}}));
                        if text.contains("[hold]") {
                            active.insert(thread.clone(), turn);
                        } else {
                            let message = format!("msg-{turn}");
                            let answer = json!({"type": "agentMessage", "id": message, "text": "", "phase": "final_answer"});
                            frames.push(json!({"method": "item/started", "params": {"item": answer, "threadId": thread, "turnId": turn}}));
                            let mut done = answer.clone();
                            done["text"] = json!("done");
                            frames.push(json!({"method": "item/completed", "params": {"item": done, "threadId": thread, "turnId": turn}}));
                            frames.push(json!({"method": "turn/completed", "params": {"threadId": thread, "turn": {"id": turn, "items": [], "status": "completed", "error": null}}}));
                        }
                    }
                    "turn/interrupt" => {
                        frames.push(json!({"id": id, "result": {}}));
                        if let Some(turn) = active.remove(&thread) {
                            frames.push(json!({"method": "turn/completed", "params": {"threadId": thread, "turn": {"id": turn, "items": [], "status": "interrupted", "error": null}}}));
                        }
                    }
                    "turn/steer" => {
                        frames.push(json!({"id": id, "result": {"turnId": active.get(&thread)}}))
                    }
                    _ => frames.push(json!({"id": id, "result": {}})),
                }
                for frame in frames {
                    let mut bytes = serde_json::to_vec(&frame).unwrap();
                    bytes.push(b'\n');
                    if output.write_all(&bytes).await.is_err() {
                        return;
                    }
                }
            }
            let _ = exited.send(true);
        });
        Ok(ProviderProcess {
            input: Box::new(input),
            output: Box::new(output),
            stderr: Box::new(tokio::io::empty()),
            control: Box::new(Exit(exit)),
        })
    }
}

async fn eventually(host: &Host, thread: &ThreadId, done: impl Fn(&State) -> bool) -> Arc<State> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let state = host.conversation.runtime.state(thread).await.unwrap().state;
            if done(&state) {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the thread reaches the expected state")
}
fn running(state: &State) -> bool {
    state.runs.iter().any(|run| {
        run.status == RunStatus::Running
            && run.attempt.as_ref().is_some_and(|attempt| {
                state.attempts.iter().any(|candidate| {
                    &candidate.id == attempt
                        && candidate.status == agent_domain::AttemptStatus::Running
                })
            })
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_tools_delegate_create_queue_and_interrupt_through_the_runtime() {
    let replay = Arc::new(ReplaySpawner {
        transcript: SIMPLE,
        spawned: Mutex::new(vec![]),
    });
    let host = host_with(Arc::new(HeldCodex), replay).await;
    let launched: wire::Launched = host
        .call(launch(&host, "parent", "[hold] parent work"))
        .await
        .unwrap();
    let parent = launched.thread_id;
    eventually(&host, &parent, running).await;

    let pinned = tool(&host, &parent, "thread_organize", json!({"action": "pin"})).await;
    assert!(pinned["sequence"].is_u64(), "{pinned}");
    tool(
        &host,
        &parent,
        "thread_organize",
        json!({"action": "unpin"}),
    )
    .await;
    let listed = tool(&host, &parent, "thread_list", json!({})).await;
    assert_eq!(listed["currentThreadId"], json!(parent));
    assert_eq!(listed["threads"][0]["status"], "running");

    // A delegated child runs until it is cancelled; its result reaches the parent.
    let delegated = tool(
        &host,
        &parent,
        "delegate_task",
        json!({"task": "[hold] child work", "title": "Child review", "clientRequestId": "round-1"}),
    )
    .await;
    assert_eq!(delegated["status"], "running", "{delegated}");
    assert_eq!(delegated["waitTimedOut"], false);
    let child = ThreadId::new(delegated["childThreadId"].as_str().unwrap()).unwrap();
    eventually(&host, &child, running).await;
    let child_state = host.conversation.runtime.state(&child).await.unwrap().state;
    let child_thread = child_state.thread.as_ref().unwrap();
    assert_eq!(child_thread.title, "Child review");
    assert_eq!(
        (
            child_thread.created_by,
            child_thread.creation_source.as_str()
        ),
        (agent_domain::MessageAuthor::Agent, "mcp")
    );
    let child_read = tool(&host, &parent, "thread_read", json!({"threadId": child})).await;
    assert_eq!(child_read["thread"]["relationshipToParent"], "subagent");
    assert_eq!(child_read["items"][0]["text"], "[hold] child work");
    let timed_out = tool(
        &host,
        &parent,
        "thread_wait",
        json!({"threadId": child, "timeoutMs": 300}),
    )
    .await;
    assert_eq!(timed_out["timedOut"], true);
    assert_eq!(timed_out["status"], "running");
    let task = delegated["taskId"].clone();
    let cancelled = tool(
        &host,
        &parent,
        "task_cancel",
        json!({"taskId": task, "reason": "enough"}),
    )
    .await;
    assert_eq!(cancelled["status"], "cancel_requested");
    let finished = tool(&host, &parent, "thread_wait", json!({"threadId": child})).await;
    assert_eq!(finished["status"], "interrupted");
    eventually(&host, &parent, |state| {
        state.tasks.iter().any(|t| t.status.terminal())
    })
    .await;
    let status = tool(&host, &parent, "task_status", json!({"taskId": task})).await;
    assert_eq!(status["status"], "interrupted");
    assert_eq!(status["workState"], "result_available");

    // Top-level threads share the caller's checkout and are recorded in its timeline.
    let created = tool(
        &host,
        &parent,
        "create_threads",
        json!({"threads": [{"prompt": "quick task"}], "clientRequestId": "batch-1"}),
    )
    .await;
    let entry = &created["threads"][0];
    assert_eq!(entry["title"], "quick task", "{created}");
    assert_eq!(entry["createdBy"], "agent");
    assert_eq!(entry["creationSource"], "mcp");
    let created_thread = ThreadId::new(entry["threadId"].as_str().unwrap()).unwrap();
    let done = tool(
        &host,
        &parent,
        "thread_wait",
        json!({"threadId": created_thread}),
    )
    .await;
    assert_eq!(done["status"], "completed");
    let activity = tool(
        &host,
        &parent,
        "thread_read",
        json!({"threadId": parent, "view": "activity"}),
    )
    .await;
    let record = activity["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "thread_created")
        .cloned()
        .expect("the parent timeline records the created thread");
    assert_eq!(record["title"], "quick task");
    assert_eq!(
        record["text"],
        json!(format!(
            "Created thread {created_thread} with codex (gpt-6-luna)."
        ))
    );
    let again = tool(
        &host,
        &parent,
        "create_threads",
        json!({"threads": [{"prompt": "quick task"}], "clientRequestId": "batch-1"}),
    )
    .await;
    assert_eq!(again["threads"][0]["threadId"], json!(created_thread));
    let followup = tool(
        &host,
        &parent,
        "thread_send",
        json!({"threadId": created_thread, "message": "follow up"}),
    )
    .await;
    assert_eq!(followup["delivery"], "started", "{followup}");
    let renamed = tool(
        &host,
        &parent,
        "thread_update",
        json!({"threadId": created_thread, "action": "rename", "title": "Renamed"}),
    )
    .await;
    assert_eq!(renamed["title"], "Renamed");

    // A queued follow-up of the caller can be read, edited and cancelled.
    let queued = tool(
        &host,
        &parent,
        "thread_send",
        json!({"threadId": parent, "message": "[hold] later", "mode": "queue"}),
    )
    .await;
    assert_eq!(queued["delivery"], "queued", "{queued}");
    let queue = tool(&host, &parent, "queue_list", json!({})).await;
    assert_eq!(queue["items"][0]["text"], "[hold] later");
    let run = queue["items"][0]["queuedRunId"].clone();
    tool(
        &host,
        &parent,
        "queue_edit",
        json!({"queuedRunId": run, "text": "[hold] edited"}),
    )
    .await;
    let edited = tool(&host, &parent, "queue_read", json!({"queuedRunId": run})).await;
    assert_eq!(edited["text"], "[hold] edited");
    tool(&host, &parent, "queue_cancel", json!({"queuedRunId": run})).await;
    let empty = tool(&host, &parent, "queue_list", json!({})).await;
    assert_eq!(empty["items"], json!([]));

    let interrupted = tool(
        &host,
        &parent,
        "thread_interrupt",
        json!({"threadId": parent}),
    )
    .await;
    assert_eq!(
        interrupted["status"], "interrupt_requested",
        "{interrupted}"
    );
    let stopped = tool(
        &host,
        &parent,
        "thread_wait",
        json!({"threadId": parent, "runId": interrupted["runId"]}),
    )
    .await;
    assert_eq!(stopped["status"], "interrupted");
    let idle = tool(
        &host,
        &parent,
        "thread_interrupt",
        json!({"threadId": parent}),
    )
    .await;
    assert_eq!(
        idle,
        json!({"threadId": parent, "runId": null, "status": "no_active_run"})
    );
    host.conversation.shutdown().await;
}

// OrchestratorMcpToolkit.integration.test.ts: a thread_send repeated with its
// clientRequestId returns the first run. Here the retry arrives once that run is
// running, so the same request now resolves to a steer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retried_thread_send_returns_its_first_run_after_that_run_starts() {
    let replay = Arc::new(ReplaySpawner {
        transcript: SIMPLE,
        spawned: Mutex::new(vec![]),
    });
    let host = host_with(Arc::new(HeldCodex), replay).await;
    let parent: wire::Launched = host
        .call(launch(&host, "parent", "[hold] parent work"))
        .await
        .unwrap();
    let parent = parent.thread_id;
    let target: wire::Launched = host
        .call(launch(&host, "target", "first task"))
        .await
        .unwrap();
    let target = target.thread_id;
    eventually(&host, &target, |state| {
        !state.runs.is_empty() && state.runs.iter().all(|run| run.status.terminal())
    })
    .await;
    let send = json!({"threadId": target, "message": "[hold] next task", "clientRequestId": "loop-send-1"});
    let sent = tool(&host, &parent, "thread_send", send.clone()).await;
    assert_eq!(sent["delivery"], "started", "{sent}");
    eventually(&host, &target, running).await;

    let repeated = tool(&host, &parent, "thread_send", send).await;

    assert_eq!(repeated["runId"], sent["runId"], "{repeated}");
    assert_eq!(repeated["messageId"], sent["messageId"]);
    assert_eq!(repeated["delivery"], "started");
    let state = host
        .conversation
        .runtime
        .state(&target)
        .await
        .unwrap()
        .state;
    assert_eq!(state.runs.len(), 2);
    host.conversation.shutdown().await;
}

// Projects carry their repository identity.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shell_projects_carry_their_repository_identity() {
    let host = host().await;
    git(
        &host.project_root,
        &["remote", "add", "origin", "git@github.com:Acme/widget.git"],
    );
    let _: agent_protocol::models::Empty = host
        .call(Call::UpdateProject(
            agent_protocol::operations::UpdateProject {
                project_id: host.project.clone(),
                scripts: None,
            },
        ))
        .await
        .unwrap();
    let shell = host
        .reply(Call::ShellStream(wire::SubscribeShell {
            after_sequence: None,
            request_completion_marker: false,
            location: wire::ShellLocation::Active,
        }))
        .await;
    let Response::Success {
        result: wire::ShellUpdate::Snapshot(snapshot),
    } = protocol::decode::<Response<wire::ShellUpdate>>(&shell.initial).unwrap()
    else {
        panic!("a snapshot opens the shell stream");
    };
    let project = snapshot
        .projects
        .iter()
        .find(|project| project.id == host.project)
        .unwrap();
    let identity = project.repository_identity.as_ref().unwrap();
    assert_eq!(identity.canonical_key, "github.com/acme/widget");
    assert_eq!(identity.provider.as_deref(), Some("github"));
    host.conversation.shutdown().await;
}
